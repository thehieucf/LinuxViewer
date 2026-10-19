//! Native socket transport for the recovered byte framing. Opening this stream
//! does not authenticate an UltraViewer session or establish partner access.

use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::frame::{self, FrameDecoder, FrameError};
use crate::relay::{HttpPreambleDecoder, RelayIPv4Address};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("The network connection is already open.")]
    AlreadyOpen,
    #[error("The relay returned an invalid network address.")]
    InvalidPort,
    #[error("The relay returned an invalid network address.")]
    InvalidTlsName,
    #[error("The UltraViewer relay did not respond. Check the connection and try again.")]
    ConnectionTimedOut,
    #[error("The UltraViewer network connection closed. Try connecting again.")]
    NotOpen,
    #[error("The UltraViewer network connection closed. Try connecting again.")]
    Closed,
    #[error("The relay connection ended before an update finished. Try connecting again.")]
    TruncatedFrame,
    #[error("The remote connection could not process its updates. Try connecting again.")]
    ConsumerTooSlow,
    #[error("The UltraViewer service address is invalid.")]
    InvalidHost,
    #[error("The UltraViewer service address could not be resolved. Check the connection and try again.")]
    DnsLookupFailed,
    #[error("The UltraViewer service address could not be resolved. Check the connection and try again.")]
    NoIpv4Addresses,
    #[error("The secure UltraViewer connection failed: {0}")]
    Tls(String),
    #[error("The network connection failed: {0}")]
    Io(String),
    #[error("The relay sent an invalid frame: {0}")]
    Frame(#[from] FrameError),
    #[error("The relay sent an oversized HTTP header.")]
    HeaderTooLarge,
}

impl From<std::io::Error> for TransportError {
    fn from(error: std::io::Error) -> Self {
        TransportError::Io(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Security {
    Plaintext,
    /// Uses platform certificate verification. A distinct certificate name
    /// permits the legacy SNI routing label while verifying the service's
    /// authoritative DNS identity. Both names are caller-selected.
    Tls { server_name: String, certificate_name: Option<String> },
}

impl Security {
    pub fn ultraviewer(certificate_name: &str) -> Self {
        Security::Tls { server_name: "ultraviewer".into(), certificate_name: Some(certificate_name.into()) }
    }
}

pub(crate) trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

type BoxStream = Pin<Box<dyn Stream>>;

/// Events produced by a connection's reader task.
#[derive(Debug)]
pub enum ReaderEvent {
    Frame(Vec<u8>),
    /// The stream ended; `None` is an orderly close.
    Closed(Option<TransportError>),
}

/// The writing side of an open framed stream. Dropping it (together with the
/// owning [`Connection`]) closes the socket.
pub struct Connection {
    writer: WriteHalf<BoxStream>,
    reader: JoinHandle<()>,
    pub address: String,
}

impl Connection {
    const WRITE_TIMEOUT: Duration = Duration::from_secs(20);

    /// Sends one length-prefixed frame.
    pub async fn send(&mut self, payload: &[u8]) -> Result<(), TransportError> {
        let frame = frame::encode(payload)?;
        self.write_raw(&frame).await
    }

    pub async fn write_raw(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        match tokio::time::timeout(Self::WRITE_TIMEOUT, async {
            self.writer.write_all(bytes).await?;
            self.writer.flush().await
        })
        .await
        {
            Ok(result) => result.map_err(TransportError::from),
            Err(_) => Err(TransportError::ConnectionTimedOut),
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Parameters for [`open`].
#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub initial_request: Option<Vec<u8>>,
    pub expects_http_response: bool,
    pub timeout: Duration,
    pub maximum_payload_length: usize,
}

impl OpenOptions {
    pub fn new(host: impl Into<String>, port: u16, security: Security) -> Self {
        Self {
            host: host.into(),
            port,
            security,
            initial_request: None,
            expects_http_response: false,
            timeout: Duration::from_secs(10),
            maximum_payload_length: frame::DEFAULT_MAXIMUM_PAYLOAD,
        }
    }
}

fn valid_dns_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 253 && name.bytes().all(|b| b == b'-' || b == b'.' || b.is_ascii_alphanumeric())
}

/// Opens a framed stream. Each received frame and the final close are mapped
/// with `map` and delivered to `sink`, in order, by a dedicated reader task.
pub async fn open<T, F>(options: OpenOptions, sink: mpsc::Sender<T>, map: F) -> Result<Connection, TransportError>
where
    T: Send + 'static,
    F: Fn(ReaderEvent) -> T + Send + Sync + 'static,
{
    if options.port == 0 {
        return Err(TransportError::InvalidPort);
    }
    let tls = match &options.security {
        Security::Plaintext => None,
        Security::Tls { server_name, certificate_name } => {
            if !valid_dns_name(server_name) || certificate_name.as_deref().is_some_and(|n| !valid_dns_name(n)) {
                return Err(TransportError::InvalidTlsName);
            }
            Some((server_name.clone(), certificate_name.clone()))
        }
    };
    let host = options.host.clone();
    let port = options.port;
    let stream: BoxStream = match tokio::time::timeout(options.timeout, async move {
        let tcp = || async {
            let tcp = TcpStream::connect((host.as_str(), port)).await?;
            tcp.set_nodelay(true)?;
            Ok::<_, TransportError>(tcp)
        };
        let stream: BoxStream = match tls {
            None => Box::pin(tcp().await?),
            Some((server_name, certificate_name)) => {
                let certificate_name = certificate_name.unwrap_or_else(|| server_name.clone());
                Box::pin(crate::tls::connect(tcp, &server_name, &certificate_name).await?)
            }
        };
        Ok::<_, TransportError>(stream)
    })
    .await
    {
        Ok(result) => result?,
        Err(_) => return Err(TransportError::ConnectionTimedOut),
    };
    let (read_half, write_half) = tokio::io::split(stream);
    let mut connection = Connection {
        writer: write_half,
        reader: tokio::spawn(async {}),
        address: format!("{}:{}", options.host, options.port),
    };
    if let Some(request) = &options.initial_request {
        connection.write_raw(request).await?;
    }
    connection.reader = tokio::spawn(read_loop(read_half, options.expects_http_response, options.maximum_payload_length, sink, map));
    Ok(connection)
}

async fn read_loop<T, F>(mut reader: ReadHalf<BoxStream>, expects_http: bool, maximum: usize, sink: mpsc::Sender<T>, map: F)
where
    F: Fn(ReaderEvent) -> T,
{
    let mut decoder = FrameDecoder::new(maximum);
    let mut preamble = HttpPreambleDecoder::default();
    let mut buffer = vec![0u8; 64 * 1024];
    let result: Result<(), TransportError> = async {
        loop {
            let count = reader.read(&mut buffer).await?;
            if count == 0 {
                if decoder.has_incomplete_frame() || (expects_http && !preamble.has_received_header()) {
                    return Err(TransportError::TruncatedFrame);
                }
                return Ok(());
            }
            let bytes = if expects_http {
                preamble.append(&buffer[..count]).map_err(|_| TransportError::HeaderTooLarge)?
            } else {
                &buffer[..count]
            };
            for frame in decoder.append(bytes)? {
                if sink.send(map(ReaderEvent::Frame(frame))).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
    .await;
    let _ = sink.send(map(ReaderEvent::Closed(result.err()))).await;
}

/// IPv4 literals bypass DNS. Resolved addresses are de-duplicated in order.
pub async fn resolve_ipv4(host: &str) -> Result<Vec<String>, TransportError> {
    if !valid_dns_name(host) {
        return Err(TransportError::InvalidHost);
    }
    if RelayIPv4Address::parse(host).is_ok() {
        return Ok(vec![host.to_string()]);
    }
    let addresses = tokio::net::lookup_host((host, 0)).await.map_err(|_| TransportError::DnsLookupFailed)?;
    let mut seen = Vec::<Ipv4Addr>::new();
    for address in addresses {
        if let SocketAddr::V4(v4) = address {
            if !seen.contains(v4.ip()) {
                seen.push(*v4.ip());
            }
        }
    }
    if seen.is_empty() {
        return Err(TransportError::NoIpv4Addresses);
    }
    Ok(seen.iter().map(ToString::to_string).collect())
}

/// Tries the command service's resolved IPv4 endpoints without changing the
/// caller's authoritative TLS identity, legacy SNI, or HTTP request bytes.
pub async fn open_command_connection<T, F>(options: OpenOptions, attempt_timeout: Duration, sink: mpsc::Sender<T>, map: F)
    -> Result<Connection, TransportError>
where
    T: Send + 'static,
    F: Fn(ReaderEvent) -> T + Clone + Send + Sync + 'static,
{
    let addresses = resolve_ipv4(&options.host).await?;
    let mut last_error = TransportError::NoIpv4Addresses;
    for address in addresses {
        let mut attempt = options.clone();
        attempt.host = address;
        attempt.timeout = attempt_timeout;
        match open(attempt, sink.clone(), map.clone()).await {
            Ok(connection) => return Ok(connection),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}
