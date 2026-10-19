//! Relay routing envelopes, keepalives and the streaming HTTP preamble.

use std::fmt;

use crate::crypto::LegacyCommandCipher;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid IPv4 address")]
pub struct InvalidAddress;

/// IPv4 routing address used by VTCPClient.Send (MethodDef 0x06000de1).
/// This address is separate from an UltraViewer partner ID.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RelayIPv4Address(u32);

impl RelayIPv4Address {
    pub fn parse(dotted_decimal: &str) -> Result<Self, InvalidAddress> {
        let parts: Vec<&str> = dotted_decimal.split('.').collect();
        if parts.len() != 4 {
            return Err(InvalidAddress);
        }
        let mut value = 0u32;
        for (index, part) in parts.iter().enumerate() {
            if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(InvalidAddress);
            }
            let octet: u8 = part.parse().map_err(|_| InvalidAddress)?;
            value |= u32::from(octet) << ((3 - index) * 8);
        }
        Ok(Self(value))
    }

    pub fn from_octets(octets: [u8; 4]) -> Self {
        Self(u32::from_be_bytes(octets))
    }

    fn from_wire(bytes: &[u8]) -> Self {
        Self(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn octets(&self) -> [u8; 4] {
        self.0.to_be_bytes()
    }

    /// Private (RFC 1918) and link-local IPv4 addresses.
    pub fn is_lan(&self) -> bool {
        let b = self.octets();
        b[0] == 10 || (b[0] == 172 && (16..=31).contains(&b[1])) || (b[0] == 192 && b[1] == 168) || (b[0] == 169 && b[1] == 254)
    }
}

impl fmt::Display for RelayIPv4Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let o = self.octets();
        write!(f, "{}.{}.{}.{}", o[0], o[1], o[2], o[3])
    }
}

impl fmt::Debug for RelayIPv4Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::str::FromStr for RelayIPv4Address {
    type Err = InvalidAddress;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Registration profiles store an address as a dotted-decimal JSON string.
impl serde::Serialize for RelayIPv4Address {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for RelayIPv4Address {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(|_| serde::de::Error::custom("Invalid IPv4 address"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("empty envelope")]
    EmptyEnvelope,
    #[error("truncated forward envelope")]
    TruncatedForward,
    #[error("unknown envelope kind {0}")]
    UnknownKind(u8),
}

/// Inner relay message, before LengthPrefixedFrame framing.
/// Recovered from VTCPClient.Send (0x06000de1) and dispatch (0x06000e37).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayEnvelope {
    Forward { address: RelayIPv4Address, port: u16, format_flag: bool, payload: Vec<u8> },
    /// The desktop/server protocol interprets these raw bytes. A leading optional
    /// format flag is retained because the supplied receive dispatcher retains it.
    System { payload: Vec<u8> },
}

impl RelayEnvelope {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            RelayEnvelope::Forward { address, port, format_flag, payload } => {
                encode_forward(*address, *port, *format_flag, payload)
            }
            RelayEnvelope::System { payload } => encode_system(payload, None),
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        let kind = *bytes.first().ok_or(EnvelopeError::EmptyEnvelope)?;
        match kind {
            0 => {
                if bytes.len() < 8 {
                    return Err(EnvelopeError::TruncatedForward);
                }
                Ok(RelayEnvelope::Forward {
                    address: RelayIPv4Address::from_wire(&bytes[2..6]),
                    port: u16::from(bytes[6]) | (u16::from(bytes[7]) << 8),
                    format_flag: bytes[1] > 0,
                    payload: bytes[8..].to_vec(),
                })
            }
            1 => Ok(RelayEnvelope::System { payload: bytes[1..].to_vec() }),
            other => Err(EnvelopeError::UnknownKind(other)),
        }
    }
}

pub fn encode_forward(address: RelayIPv4Address, port: u16, format_flag: bool, payload: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(8 + payload.len());
    result.push(0);
    result.push(u8::from(format_flag));
    // Original helper333 builds a*256^3+b*256^2+c*256+d, then
    // BitConverter.GetBytes(UInt32) writes it little-endian on Windows.
    let mut octets = address.octets();
    octets.reverse();
    result.extend_from_slice(&octets);
    result.extend_from_slice(&port.to_le_bytes());
    result.extend_from_slice(payload);
    result
}

/// `format_flag == None` matches Send's default final argument (false).
/// Supplying a Boolean matches its explicit include-flag variation.
pub fn encode_system(payload: &[u8], format_flag: Option<bool>) -> Vec<u8> {
    let mut result = Vec::with_capacity(2 + payload.len());
    result.push(1);
    if let Some(flag) = format_flag {
        result.push(u8::from(flag));
    }
    result.extend_from_slice(payload);
    result
}

/// Two distinct system channels; peer ASCII20 has its own routed envelope.
pub struct RelayKeepalive;

impl RelayKeepalive {
    pub fn relay_envelope() -> Vec<u8> {
        encode_system(b"ping", None)
    }

    pub fn command_envelope() -> Vec<u8> {
        let encrypted = LegacyCommandCipher::desktop_server_commands().encrypt(b"2");
        encode_system(&encrypted, Some(false))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("empty direct envelope")]
pub struct EmptyDirectEnvelope;

/// P2P inner bytes, before optional AES and outer length-prefix framing.
/// Recovered from P2PSend (0x06000e2c) and receive dispatch (0x06000304).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectPeerEnvelope {
    pub format_flag: bool,
    pub payload: Vec<u8>,
}

impl DirectPeerEnvelope {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + self.payload.len());
        out.push(u8::from(self.format_flag));
        out.extend_from_slice(&self.payload);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, EmptyDirectEnvelope> {
        let flag = *bytes.first().ok_or(EmptyDirectEnvelope)?;
        Ok(Self { format_flag: flag > 0, payload: bytes[1..].to_vec() })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid relay host")]
pub struct InvalidHost;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Post,
    Get,
}

/// Exact streaming HTTP request headers reconstructed from VTCPClient 0x06000e25.
/// The caller supplies the host; this does not discover or authenticate a server.
pub fn http_preamble(host: &str, method: HttpMethod) -> Result<Vec<u8>, InvalidHost> {
    // Local input validation prevents a user-provided host from adding headers.
    if host.is_empty() || !host.bytes().all(|b| (33..=126).contains(&b)) {
        return Err(InvalidHost);
    }
    let method = match method {
        HttpMethod::Post => "POST",
        HttpMethod::Get => "GET",
    };
    let lines = [
        format!("{method} /UVService/ HTTP/1.1"),
        "User-Agent: Mozilla/4.0 (compatible; MSIE5.01; Windows NT)".into(),
        "Content-Type: application/octet-stream".into(),
        "Connection: Keep-Alive".into(),
        "Content-length: 8589934590".into(),
        format!("Host: {host}"),
        String::new(),
        String::new(),
    ];
    Ok(lines.join("\r\n").into_bytes())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("relay HTTP header too large")]
pub struct HeaderTooLarge;

/// Bounded equivalent of VTCPClient's first-response CRLFCRLF stripping.
/// This recognizes the header boundary only; it does not validate HTTP status.
#[derive(Debug)]
pub struct HttpPreambleDecoder {
    maximum_header_bytes: usize,
    has_received_header: bool,
    response_status_code: Option<u16>,
    header: Vec<u8>,
}

impl Default for HttpPreambleDecoder {
    fn default() -> Self {
        Self::new(32_768)
    }
}

impl HttpPreambleDecoder {
    pub fn new(maximum_header_bytes: usize) -> Self {
        assert!(maximum_header_bytes >= 4);
        Self { maximum_header_bytes, has_received_header: false, response_status_code: None, header: Vec::new() }
    }

    pub fn has_received_header(&self) -> bool {
        self.has_received_header
    }

    /// Diagnostic only; unrecognized or non-HTTP headers leave this `None`.
    pub fn response_status_code(&self) -> Option<u16> {
        self.response_status_code
    }

    /// Returns only bytes following the first complete header. After that, all
    /// appended bytes pass through. Partial headers are buffered within the bound.
    pub fn append<'a>(&mut self, bytes: &'a [u8]) -> Result<&'a [u8], HeaderTooLarge> {
        if self.has_received_header {
            return Ok(bytes);
        }
        for (index, &byte) in bytes.iter().enumerate() {
            if self.header.len() >= self.maximum_header_bytes {
                self.header = Vec::new();
                return Err(HeaderTooLarge);
            }
            self.header.push(byte);
            if self.header.ends_with(b"\r\n\r\n") {
                self.has_received_header = true;
                let line_end = self.header.iter().position(|&b| b == 13 || b == 10).unwrap_or(self.header.len());
                let line = String::from_utf8_lossy(&self.header[..line_end]).into_owned();
                let fields: Vec<&str> = line.split(' ').filter(|f| !f.is_empty()).collect();
                if fields.len() >= 2
                    && fields[0].starts_with("HTTP/")
                    && fields[1].len() == 3
                    && fields[1].bytes().all(|b| b.is_ascii_digit())
                {
                    self.response_status_code = fields[1].parse().ok();
                }
                self.header = Vec::new();
                return Ok(&bytes[index + 1..]);
            }
        }
        Ok(&[])
    }

    pub fn reset(&mut self) {
        self.header = Vec::new();
        self.has_received_header = false;
        self.response_status_code = None;
    }
}
