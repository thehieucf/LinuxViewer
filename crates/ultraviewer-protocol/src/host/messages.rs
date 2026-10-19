//! Plain system messages from the verified registration/data relay connections,
//! and the screen payloads a host sends to a modern Windows viewer.

use crate::multipart;
use crate::relay::{encode_system, RelayIPv4Address};
use crate::zstd::{self, ZstdEncodeError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HostRelayError {
    #[error("malformed relay message")]
    MalformedMessage,
    #[error("invalid relay address")]
    InvalidAddress,
    #[error("invalid relay hostname")]
    InvalidHostname,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayRequest {
    pub address: RelayIPv4Address,
    pub hostname: String,
    pub request_tag: String,
    pub request_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayAddress {
    pub port: u16,
    pub address: RelayIPv4Address,
}

fn fields(payload: &[u8]) -> Result<Vec<String>, HostRelayError> {
    if payload.is_empty()
        || payload.len() > 16_384
        || !payload.iter().all(|b| (32..=126).contains(b))
        || payload.iter().filter(|&&b| b == b':').count() >= 128
    {
        return Err(HostRelayError::MalformedMessage);
    }
    let text = std::str::from_utf8(payload).map_err(|_| HostRelayError::MalformedMessage)?;
    Ok(text.split(':').map(String::from).collect())
}

fn endpoint(address: &str, hostname: &str, tag: &str, hash: &str) -> Result<RelayRequest, HostRelayError> {
    let address = RelayIPv4Address::parse(address).map_err(|_| HostRelayError::InvalidAddress)?;
    let hostname = if hostname.is_empty() { crate::COMMAND_HOST.to_string() } else { hostname.to_lowercase() };
    let valid = (hostname == "ultraviewer.net" || hostname.ends_with(".ultraviewer.net"))
        && hostname.len() <= 253
        && hostname.bytes().all(|b| b == b'-' || b == b'.' || b.is_ascii_digit() || b.is_ascii_lowercase());
    if !valid {
        return Err(HostRelayError::InvalidHostname);
    }
    Ok(RelayRequest { address, hostname, request_tag: tag.into(), request_hash: hash.into() })
}

/// Server command 11 (legacy) or 17 (modern request-info) asking this host
/// to open a data relay for an incoming viewer.
pub fn request(payload: &[u8], default_addresses: &str, default_hostnames: &str) -> Result<RelayRequest, HostRelayError> {
    let fields = fields(payload)?;
    if fields.len() >= 4 && fields[0] == "11" {
        return endpoint(&fields[1], &fields[3], "", "");
    }
    // Modern request-info path: preserve the opaque request tag/hash in the
    // availability response. Direct/P2P context is deliberately unused.
    let valid = fields.len() >= 9
        && fields[0] == "17"
        && fields[1].len() <= 512
        && fields[2].len() <= 512
        && ["0", "1", "-1"].contains(&fields[5].as_str())
        && fields[6].parse::<i16>().is_ok()
        && fields[7].parse::<i32>().is_ok()
        && ["0", "1", "-1"].contains(&fields[8].as_str());
    if !valid {
        return Err(HostRelayError::MalformedMessage);
    }
    let addresses: Vec<&str> = if fields[3].is_empty() { default_addresses } else { &fields[3] }.split(';').collect();
    let hostnames: Vec<&str> = if fields[3].is_empty() { default_hostnames } else { &fields[4] }.split(';').collect();
    if addresses.len() > 64 || hostnames.len() > 64 {
        return Err(HostRelayError::MalformedMessage);
    }
    for (index, address) in addresses.iter().enumerate() {
        let hostname = hostnames.get(index).copied().unwrap_or("");
        if let Ok(request) = endpoint(address, hostname, &fields[1], &fields[2]) {
            return Ok(request);
        }
    }
    Err(HostRelayError::InvalidAddress)
}

/// Relay system reply `0:<port>:<address>` describing this host's public route.
pub fn address(payload: &[u8]) -> Result<RelayAddress, HostRelayError> {
    let fields = fields(payload)?;
    if fields.len() < 3 || fields[0] != "0" || !crate::util::is_digits(&fields[1]) {
        return Err(HostRelayError::MalformedMessage);
    }
    let port: u16 = fields[1].parse().map_err(|_| HostRelayError::MalformedMessage)?;
    if port == 0 {
        return Err(HostRelayError::MalformedMessage);
    }
    let address = RelayIPv4Address::parse(&fields[2]).map_err(|_| HostRelayError::InvalidAddress)?;
    Ok(RelayAddress { port, address })
}

pub fn address_request_envelope() -> Vec<u8> {
    encode_system(b"0", None)
}

/// Native array DataReceived converts flag=true text as UTF-16LE before
/// ManipulatePacket. Binary input remains on the false-format path.
pub fn peer_payload(payload: &[u8], unicode: bool) -> Result<Vec<u8>, HostRelayError> {
    if !unicode {
        return Ok(payload.to_vec());
    }
    if payload.len() > 2_097_152 || payload.len() % 2 != 0 {
        return Err(HostRelayError::MalformedMessage);
    }
    let units: Vec<u16> = payload.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let text = String::from_utf16(&units).map_err(|_| HostRelayError::MalformedMessage)?;
    if !text.chars().all(|c| c as u32 >= 32 || matches!(c as u32, 9 | 10 | 13)) {
        return Err(HostRelayError::MalformedMessage);
    }
    Ok(text.into_bytes())
}

/// Availability notice for a fresh non-P2P relay, sent encrypted on the
/// persistent command channel with format flag 0.
pub fn availability(request: &RelayRequest, public_address: &RelayAddress) -> Vec<u8> {
    format!(
        "17:{}:{}:{}:{}:{}:0:|0",
        request.request_tag, request.request_hash, request.address, public_address.port, public_address.address
    )
    .into_bytes()
}

/// Captured RGB555 pixels: tightly packed bottom-up rows, UInt16 little
/// endian, blue bits0...4, green5...9, red10...14. No row padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedScreenFrame {
    pub width: usize,
    pub height: usize,
    pub rgb555_bottom_up: Vec<u8>,
}

impl HostedScreenFrame {
    /// Converts top-down BGRA/BGRX pixels into this frame format.
    pub fn from_bgra_top_down(width: usize, height: usize, stride: usize, bgra: &[u8]) -> Option<Self> {
        if width == 0 || height == 0 || stride < width * 4 || bgra.len() < stride * (height - 1) + width * 4 {
            return None;
        }
        let mut out = vec![0u8; width * height * 2];
        for y in 0..height {
            let row = &bgra[y * stride..y * stride + width * 4];
            let target = (height - 1 - y) * width * 2;
            for (x, pixel) in row.chunks_exact(4).enumerate() {
                let word = (u16::from(pixel[2] >> 3) << 10) | (u16::from(pixel[1] >> 3) << 5) | u16::from(pixel[0] >> 3);
                out[target + x * 2..target + x * 2 + 2].copy_from_slice(&word.to_le_bytes());
            }
        }
        Some(Self { width, height, rgb555_bottom_up: out })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostedScreenError {
    #[error("invalid screen geometry")]
    InvalidGeometry,
    #[error("invalid pixel length (expected {expected}, received {received})")]
    InvalidPixelLength { expected: usize, received: usize },
    #[error("invalid version")]
    InvalidVersion,
    #[error("screen update exceeds the size limit")]
    SizeLimitExceeded,
}

pub const FIRST_FRAME_NUMBER: i32 = -2_147_483_646;
const MAXIMUM_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_DIMENSION: usize = 8192;

fn checked_pixel_count(width: usize, height: usize) -> Result<usize, HostedScreenError> {
    if !(1..=MAXIMUM_DIMENSION).contains(&width) || !(1..=MAXIMUM_DIMENSION).contains(&height) {
        return Err(HostedScreenError::InvalidGeometry);
    }
    let count = width * height * 2;
    if count > MAXIMUM_PAYLOAD_BYTES {
        return Err(HostedScreenError::SizeLimitExceeded);
    }
    Ok(count)
}

/// One monitor rectangle has a trailing separator because native InitMonitor
/// splits on '|' and iterates count-1 entries. Version must select the
/// recovered >=6.0.0 command1 drawing contract.
pub fn initialization(width: usize, height: usize, version: &str) -> Result<Vec<u8>, HostedScreenError> {
    checked_pixel_count(width, height)?;
    let components: Vec<&str> = version.split('.').collect();
    let valid = version.len() <= 64
        && components.len() == 3
        && components.iter().all(|c| crate::util::is_digits(c))
        && components[0].parse::<u32>().is_ok_and(|major| major >= 6);
    if !valid {
        return Err(HostedScreenError::InvalidVersion);
    }
    let monitor = format!("0:0:{width}:{height}|");
    let long = |v: i32| v.to_le_bytes().to_vec();
    let parts = vec![
        vec![0u8], long(width as i32), long(height as i32), vec![], vec![], vec![0], version.as_bytes().to_vec(), vec![1],
        monitor.into_bytes(), long(0), long(0),
    ];
    multipart::encode(&parts).map_err(|_| HostedScreenError::SizeLimitExceeded)
}

/// A single full-screen main rectangle, with no JPEG, scroll or cache data.
pub fn full_frame(frame: &HostedScreenFrame, sequence: i32) -> Result<Vec<u8>, HostedScreenError> {
    let byte_count = checked_pixel_count(frame.width, frame.height)?;
    if frame.rgb555_bottom_up.len() != byte_count {
        return Err(HostedScreenError::InvalidPixelLength { expected: byte_count, received: frame.rgb555_bottom_up.len() });
    }
    let maximum_compressed = MAXIMUM_PAYLOAD_BYTES - (44 + 1 + 8 + 4);
    let stride = frame.width * 2;
    let mut top_down = vec![0u8; byte_count];
    for row in 0..frame.height {
        let source = (frame.height - row - 1) * stride;
        top_down[row * stride..(row + 1) * stride].copy_from_slice(&frame.rgb555_bottom_up[source..source + stride]);
    }
    let mut rectangle = Vec::with_capacity(8);
    for value in [0u16, 0, frame.width as u16, frame.height as u16] {
        rectangle.extend_from_slice(&value.to_le_bytes());
    }
    let compressed = match zstd::encode(&top_down, maximum_compressed) {
        Ok(compressed) => compressed,
        Err(ZstdEncodeError::OutputLimitExceeded) => return Err(HostedScreenError::SizeLimitExceeded),
        Err(_) => return Err(HostedScreenError::SizeLimitExceeded),
    };
    let parts = vec![
        vec![1u8], rectangle, compressed, vec![], sequence.to_le_bytes().to_vec(), vec![], vec![], vec![], vec![], vec![], vec![],
    ];
    multipart::encode(&parts).map_err(|_| HostedScreenError::SizeLimitExceeded)
}

/// Reply to the viewer's raw ASCII20 keepalive, as the original host does.
pub fn keepalive_reply() -> Vec<u8> {
    multipart::encode(&[vec![20u8]]).expect("static payload")
}

pub fn control_permission(allowed: bool) -> Vec<u8> {
    multipart::encode(&[vec![49u8], vec![if allowed { 0 } else { 1 }]]).expect("static payload")
}

pub fn single_command(command: u8) -> Vec<u8> {
    multipart::encode(&[vec![command]]).expect("static payload")
}
