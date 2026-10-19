//! TLS client for UltraViewer services, on the system OpenSSL.
//!
//! Two compatibility details matter:
//! * the legacy SNI routing label ("ultraviewer") differs from the DNS name
//!   the certificate is verified against;
//! * some relays omit their intermediate certificate. macOS completes such
//!   chains from the certificate's Authority Information Access URL, so this
//!   module does too (with a bundled copy of the current Sectigo intermediate
//!   and an on-disk cache). Verification always ends at a system trust root.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use openssl::nid::Nid;
use openssl::ssl::{Ssl, SslConnector, SslMethod, SslVerifyMode, SslVersion};
use openssl::x509::verify::X509CheckFlags;
use openssl::x509::X509;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_openssl::SslStream;

use crate::transport::TransportError;

static BUNDLED_INTERMEDIATES: &[u8] = include_bytes!("../resources/intermediates.pem");

struct State {
    connector: Option<SslConnector>,
    /// DER of every extra intermediate, bundled or fetched.
    intermediates: Vec<Vec<u8>>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        // Debug switch to exercise AIA completion without the bundled copy.
        let bundled = if std::env::var_os("LINUXVIEWER_TLS_NO_BUNDLED").is_some() { &[][..] } else { BUNDLED_INTERMEDIATES };
        let mut intermediates: Vec<Vec<u8>> = X509::stack_from_pem(bundled)
            .unwrap_or_default()
            .iter()
            .filter_map(|cert| cert.to_der().ok())
            .collect();
        if let Some(directory) = cache_directory() {
            if let Ok(entries) = std::fs::read_dir(directory) {
                for entry in entries.flatten() {
                    if let Ok(bytes) = std::fs::read(entry.path()) {
                        if X509::from_der(&bytes).is_ok() && !intermediates.contains(&bytes) {
                            intermediates.push(bytes);
                        }
                    }
                }
            }
        }
        Mutex::new(State { connector: None, intermediates })
    })
}

fn cache_directory() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("linuxviewer").join("intermediates"))
}

fn connector() -> Result<SslConnector, TransportError> {
    let tls = |e: openssl::error::ErrorStack| TransportError::Tls(e.to_string());
    let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(connector) = &state.connector {
        return Ok(connector.clone());
    }
    let mut builder = SslConnector::builder(SslMethod::tls_client()).map_err(tls)?;
    builder.set_verify(SslVerifyMode::PEER);
    builder.set_min_proto_version(Some(SslVersion::TLS1_2)).map_err(tls)?;
    for der in &state.intermediates {
        if let Ok(cert) = X509::from_der(der) {
            // Path building still has to end at a self-signed system root.
            let _ = builder.cert_store_mut().add_cert(cert);
        }
    }
    let connector = builder.build();
    state.connector = Some(connector.clone());
    Ok(connector)
}

fn session(server_name: &str, certificate_name: &str) -> Result<Ssl, TransportError> {
    let tls = |e: openssl::error::ErrorStack| TransportError::Tls(e.to_string());
    let mut configuration = connector()?.configure().map_err(tls)?;
    configuration.set_verify_hostname(false);
    let mut ssl = configuration.into_ssl(server_name).map_err(tls)?;
    let parameters = ssl.param_mut();
    parameters.set_hostflags(X509CheckFlags::NO_PARTIAL_WILDCARDS);
    parameters.set_host(certificate_name).map_err(tls)?;
    Ok(ssl)
}

async fn handshake(tcp: TcpStream, server_name: &str, certificate_name: &str) -> Result<SslStream<TcpStream>, (TransportError, Option<X509>)> {
    let mut ssl = session(server_name, certificate_name).map_err(|e| (e, None))?;
    // Remember the leaf when its issuer is missing locally; the handshake
    // still fails exactly as OpenSSL decided.
    let missing_leaf: Arc<Mutex<Option<X509>>> = Arc::default();
    let captured = missing_leaf.clone();
    ssl.set_verify_callback(SslVerifyMode::PEER, move |preverified, context| {
        let raw = context.error().as_raw();
        if !preverified && (raw == 20 || raw == 21) {
            if let Some(chain) = context.chain() {
                if let Some(leaf) = chain.get(0) {
                    *captured.lock().unwrap_or_else(|e| e.into_inner()) = Some(leaf.to_owned());
                }
            }
        }
        preverified
    });
    let mut stream = SslStream::new(ssl, tcp).map_err(|e| (TransportError::Tls(e.to_string()), None))?;
    match Pin::new(&mut stream).connect().await {
        Ok(()) => Ok(stream),
        Err(error) => {
            let result = stream.ssl().verify_result();
            let message = if result.as_raw() != 0 { format!("{} ({error})", result.error_string()) } else { error.to_string() };
            let leaf = missing_leaf.lock().unwrap_or_else(|e| e.into_inner()).take();
            Err((TransportError::Tls(message), leaf))
        }
    }
}

/// Connects TLS on a fresh TCP connection to `address`. `connect` opens the
/// TCP stream so a retry after completing a chain uses a new socket.
pub(crate) async fn connect<F, Fut>(connect: F, server_name: &str, certificate_name: &str) -> Result<SslStream<TcpStream>, TransportError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<TcpStream, TransportError>>,
{
    match handshake(connect().await?, server_name, certificate_name).await {
        Ok(stream) => Ok(stream),
        Err((error, Some(leaf))) => {
            if !complete_chain(&leaf).await {
                return Err(error);
            }
            handshake(connect().await?, server_name, certificate_name).await.map_err(|(error, _)| error)
        }
        Err((error, None)) => Err(error),
    }
}

/// Fetches the leaf's issuer certificate from its AIA caIssuers URL.
async fn complete_chain(leaf: &X509) -> bool {
    let Some(access) = leaf.authority_info() else { return false };
    let urls: Vec<String> = access
        .iter()
        .filter(|description| description.method().nid() == Nid::AD_CA_ISSUERS)
        .filter_map(|description| description.location().uri().map(str::to_string))
        .collect();
    for url in urls {
        let Some(bytes) = http_get(&url).await else { continue };
        let certificate = X509::from_der(&bytes).or_else(|_| X509::from_pem(&bytes));
        let Ok(certificate) = certificate else { continue };
        // Only accept the certificate that actually issued this leaf.
        let issued = leaf.issuer_name().to_der().ok() == certificate.subject_name().to_der().ok()
            && certificate.public_key().ok().is_some_and(|key| leaf.verify(&key).unwrap_or(false));
        if !issued {
            continue;
        }
        let Ok(der) = certificate.to_der() else { continue };
        {
            let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
            if !state.intermediates.contains(&der) {
                state.intermediates.push(der.clone());
            }
            state.connector = None;
        }
        if let Some(directory) = cache_directory() {
            if std::fs::create_dir_all(&directory).is_ok() {
                let name = openssl::sha::sha256(&der).iter().map(|b| format!("{b:02x}")).collect::<String>();
                let _ = std::fs::write(directory.join(format!("{name}.der")), &der);
            }
        }
        log::info!("completed a TLS chain with the intermediate from {url}");
        return true;
    }
    false
}

/// Minimal bounded HTTP/1.0 GET for AIA certificate downloads (plain http only).
async fn http_get(url: &str) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || !authority.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.:".contains(&b)) {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (authority, 80u16),
    };
    if !path.bytes().all(|b| (33..=126).contains(&b)) {
        return None;
    }
    let fetch = async {
        let mut stream = TcpStream::connect((host, port)).await.ok()?;
        let request = format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: LinuxViewer\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut response = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let count = stream.read(&mut buffer).await.ok()?;
            if count == 0 {
                break;
            }
            response.extend_from_slice(&buffer[..count]);
            if response.len() > 64 * 1024 {
                return None;
            }
        }
        let split = response.windows(4).position(|w| w == b"\r\n\r\n")?;
        let status = std::str::from_utf8(&response[..split]).ok()?.lines().next()?.to_string();
        if !status.split_whitespace().nth(1).is_some_and(|code| code == "200") {
            return None;
        }
        Some(response[split + 4..].to_vec())
    };
    tokio::time::timeout(Duration::from_secs(8), fetch).await.ok().flatten()
}
