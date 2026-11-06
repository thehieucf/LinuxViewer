//! Tells the person when a newer LinuxViewer release is published: asks the
//! GitHub Releases API at startup and every 12 hours while the app runs.

use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use openssl::ssl::{SslConnector, SslMethod};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_openssl::SslStream;

use crate::runtime;
use crate::settings;
use crate::ui::window::MainWindow;

pub const RELEASES_PAGE: &str = "https://github.com/thehieucf/LinuxViewer/releases/latest";
const API_HOST: &str = "api.github.com";
const API_PATH: &str = "/repos/thehieucf/LinuxViewer/releases/latest";
const FIRST_CHECK: Duration = Duration::from_secs(10);
const CHECK_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
const MAXIMUM_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Without the tag's leading "v", e.g. "0.1.4".
    pub version: String,
    pub url: String,
}

#[derive(serde::Deserialize)]
struct ReleaseJson {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// "v1.2.3", "1.2.3" or "1.2.3-rc1" as numbers; a pre-release suffix is ignored.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// The body of an HTTP/1.1 200 response, de-chunked when needed.
fn response_body(response: &[u8]) -> Result<Vec<u8>, String> {
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").ok_or("incomplete HTTP response")?;
    let head = std::str::from_utf8(&response[..split]).map_err(|_| "invalid HTTP header")?;
    let body = &response[split + 4..];
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let code = status.split_whitespace().nth(1).unwrap_or_default();
    if code != "200" {
        return Err(format!("GitHub answered {status}"));
    }
    let chunked = lines.any(|line| {
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        name.trim().eq_ignore_ascii_case("transfer-encoding") && value.to_ascii_lowercase().contains("chunked")
    });
    if chunked {
        dechunk(body)
    } else {
        Ok(body.to_vec())
    }
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let end = body.windows(2).position(|w| w == b"\r\n").ok_or("truncated chunk size")?;
        let size_text = std::str::from_utf8(&body[..end]).map_err(|_| "invalid chunk size")?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| "invalid chunk size")?;
        body = &body[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

fn parse_release(body: &[u8]) -> Result<Option<Release>, String> {
    let release: ReleaseJson = serde_json::from_slice(body).map_err(|e| format!("unexpected release data: {e}"))?;
    if release.draft || release.prerelease || parse_version(&release.tag_name).is_none() {
        return Ok(None);
    }
    let url = if release.html_url.starts_with("https://github.com/") { release.html_url } else { RELEASES_PAGE.into() };
    Ok(Some(Release { version: release.tag_name.trim_start_matches(['v', 'V']).to_string(), url }))
}

/// The newest published release, verified over TLS against the system's CAs.
async fn fetch_latest() -> Result<Option<Release>, String> {
    let tcp = tokio::net::TcpStream::connect((API_HOST, 443)).await.map_err(|e| e.to_string())?;
    let mut builder = SslConnector::builder(SslMethod::tls_client()).map_err(|e| e.to_string())?;
    ultraviewer_protocol::trust_system_roots(&mut builder);
    let connector = builder.build();
    let ssl = connector.configure().and_then(|c| c.into_ssl(API_HOST)).map_err(|e| e.to_string())?;
    let mut stream = SslStream::new(ssl, tcp).map_err(|e| e.to_string())?;
    Pin::new(&mut stream).connect().await.map_err(|e| e.to_string())?;
    let request = format!(
        "GET {API_PATH} HTTP/1.1\r\nHost: {API_HOST}\r\nUser-Agent: LinuxViewer/{}\r\nAccept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\nConnection: close\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    );
    stream.write_all(request.as_bytes()).await.map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    (&mut stream).take(MAXIMUM_RESPONSE_BYTES).read_to_end(&mut response).await.map_err(|e| e.to_string())?;
    parse_release(&response_body(&response)?)
}

/// Checks once; a newer release is shown in the window and, the first time
/// for each version, as a desktop notification.
pub async fn check(main: &Rc<MainWindow>) {
    let result = runtime::run(async { tokio::time::timeout(Duration::from_secs(20), fetch_latest()).await }).await;
    let release = match result {
        Ok(Ok(release)) => release,
        Ok(Err(error)) => return log::info!("update check failed: {error}"),
        Err(_) => return log::info!("update check timed out"),
    };
    let Some(release) = release.filter(|r| is_newer(&r.version, env!("CARGO_PKG_VERSION"))) else { return };
    log::info!("LinuxViewer {} is available", release.version);
    main.show_update(&release);
    if settings::get().notified_update.as_deref() != Some(release.version.as_str()) {
        settings::update(|s| s.notified_update = Some(release.version.clone()));
        main.notify_update(&release);
    }
}

/// Checks shortly after startup, then every 12 hours, while enabled.
pub fn watch(main: &Rc<MainWindow>) {
    let weak = Rc::downgrade(main);
    glib::spawn_future_local(async move {
        let mut wait = FIRST_CHECK;
        loop {
            glib::timeout_future(wait).await;
            wait = CHECK_INTERVAL;
            let Some(main) = weak.upgrade() else { return };
            if settings::get().check_for_updates {
                check(&main).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.1.3"), Some((0, 1, 3)));
        assert_eq!(parse_version("1.10.0-rc1"), Some((1, 10, 0)));
        assert_eq!(parse_version("0.1"), None);
        assert_eq!(parse_version("0.1.2.3"), None);
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(is_newer("0.2.0", "0.1.3"));
        assert!(!is_newer("v0.1.3", "0.1.3"));
        assert!(!is_newer("v0.1.2", "0.1.3"));
        assert!(!is_newer("latest", "0.1.3"));
    }

    #[test]
    fn plain_and_chunked_responses() {
        let json = br#"{"tag_name":"v0.1.4","html_url":"https://github.com/thehieucf/LinuxViewer/releases/tag/v0.1.4","draft":false,"prerelease":false}"#;
        let mut plain = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n".to_vec();
        plain.extend_from_slice(json);
        let release = parse_release(&response_body(&plain).unwrap()).unwrap().unwrap();
        assert_eq!(release.version, "0.1.4");
        assert_eq!(release.url, "https://github.com/thehieucf/LinuxViewer/releases/tag/v0.1.4");

        let mut chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        let (a, b) = json.split_at(20);
        chunked.extend_from_slice(format!("{:x}\r\n", a.len()).as_bytes());
        chunked.extend_from_slice(a);
        chunked.extend_from_slice(format!("\r\n{:x};ext=1\r\n", b.len()).as_bytes());
        chunked.extend_from_slice(b);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");
        assert_eq!(response_body(&chunked).unwrap(), json.to_vec());

        assert!(response_body(b"HTTP/1.1 404 Not Found\r\n\r\n{}").is_err());
        let foreign = br#"{"tag_name":"v9.9.9","html_url":"https://example.com/x"}"#;
        assert_eq!(parse_release(foreign).unwrap().unwrap().url, RELEASES_PAGE);
        let pre = br#"{"tag_name":"v9.9.9","html_url":"https://github.com/x","prerelease":true}"#;
        assert_eq!(parse_release(pre).unwrap(), None);
    }
}
