//! Text clipboard application payloads. The caller owns consent, session/route
//! authentication, direction permissions, clipboard access and echo suppression.

use crate::commands::CommandMessage;
use crate::multipart::{self, Limits};
use crate::util::utf16le;
use crate::zstd::{self, ZstdScreenDecoder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerEncoding {
    /// Modern host: ASCII header followed by dictionary-free Zstd UTF-16LE.
    Compressed,
    /// Legacy host: UTF-16LE command5, including its delimiter prefix.
    LegacyUnicode,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
    #[error("malformed clipboard message")]
    MalformedMessage,
    #[error("unsupported clipboard command")]
    UnsupportedCommand,
    #[error("invalid clipboard text")]
    InvalidText,
    #[error("clipboard text contains an unsupported delimiter")]
    UnsupportedLegacyDelimiter,
    #[error("clipboard text exceeds the size limit")]
    SizeLimitExceeded,
    #[error("clipboard length mismatch (expected {expected}, received {received})")]
    DecompressedLengthMismatch { expected: usize, received: usize },
    #[error("clipboard compression failed")]
    Compression,
}

/// Local text-only limits, independent of the Windows implementation limits.
pub const MAXIMUM_TEXT_BYTES: usize = 1024 * 1024;
pub const MAXIMUM_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
const LEGACY_PREFIX: &str = "5|||";
const COMPRESSED_PREFIX: &[u8] = b"33|||";

/// Controller→host. Modern bytes are NOT LE32 multipart; only the ASCII
/// header is text. Keep the appended compressed bytes intact in transport.
pub fn viewer_to_host(text: &str, encoding: ViewerEncoding) -> Result<CommandMessage, ClipboardError> {
    let utf16 = encode_text(text)?;
    // The original host retries its ANSI fallback when modern33 expands to zero
    // bytes. Empty text uses the established legacy command5 path.
    if utf16.is_empty() {
        return Ok(CommandMessage { payload: utf16le(LEGACY_PREFIX), format_flag: true });
    }
    match encoding {
        ViewerEncoding::Compressed => {
            let compressed = zstd::encode(&utf16, MAXIMUM_PAYLOAD_BYTES - 32).map_err(|_| ClipboardError::Compression)?;
            let mut payload = format!("33|||{}|||", utf16.len()).into_bytes();
            payload.extend_from_slice(&compressed);
            Ok(CommandMessage { payload, format_flag: false })
        }
        ViewerEncoding::LegacyUnicode => {
            if text.contains("|||") {
                return Err(ClipboardError::UnsupportedLegacyDelimiter);
            }
            let mut payload = utf16le(LEGACY_PREFIX);
            payload.extend_from_slice(&utf16);
            Ok(CommandMessage { payload, format_flag: true })
        }
    }
}

/// Decode the original viewer wire bytes, after relay/direct framing.
pub fn decode_viewer_to_host(payload: &[u8], format_flag: bool) -> Result<String, ClipboardError> {
    check_payload(payload.len())?;
    if !format_flag {
        return decode_compressed_viewer(payload);
    }
    let text = decode_utf16(payload, MAXIMUM_PAYLOAD_BYTES)?;
    decode_legacy_viewer_text(&text)
}

/// For a host adapter that has already converted true-flag UTF-16LE to UTF-8.
pub fn decode_normalized_viewer_to_host(payload: &[u8]) -> Result<String, ClipboardError> {
    check_payload(payload.len())?;
    if payload.starts_with(COMPRESSED_PREFIX) {
        return decode_compressed_viewer(payload);
    }
    let text = std::str::from_utf8(payload).map_err(|_| ClipboardError::InvalidText)?;
    decode_legacy_viewer_text(text)
}

/// Host→controller modern command33, already LE32 multipart encoded.
pub fn host_to_viewer(text: &str) -> Result<Vec<u8>, ClipboardError> {
    let utf16 = encode_text(text)?;
    let compressed = zstd::encode(&utf16, MAXIMUM_PAYLOAD_BYTES - 32).map_err(|_| ClipboardError::Compression)?;
    multipart::encode_with(
        &[vec![33u8], (utf16.len() as u32).to_le_bytes().to_vec(), compressed],
        Limits::new(MAXIMUM_PAYLOAD_BYTES, MAXIMUM_PAYLOAD_BYTES, 3),
    )
    .map_err(|_| ClipboardError::SizeLimitExceeded)
}

/// Decodes modern multipart33 and the original uncompressed multipart5.
pub fn decode_host_to_viewer(parts: &[Vec<u8>]) -> Result<String, ClipboardError> {
    let mut total = 0usize;
    for part in parts {
        if part.len() > MAXIMUM_PAYLOAD_BYTES - total {
            return Err(ClipboardError::SizeLimitExceeded);
        }
        total += part.len();
    }
    let command = *parts.first().and_then(|p| p.first()).ok_or(ClipboardError::MalformedMessage)?;
    match command {
        33 => {
            if parts.len() < 3 || parts[1].len() < 4 {
                return Err(ClipboardError::MalformedMessage);
            }
            let expected = crate::util::read_u32(&parts[1], 0);
            if expected as usize > MAXIMUM_TEXT_BYTES {
                return Err(ClipboardError::SizeLimitExceeded);
            }
            decode_compressed_text(&parts[2], expected as usize)
        }
        5 => {
            if parts.len() < 2 {
                return Err(ClipboardError::MalformedMessage);
            }
            decode_utf16(&parts[1], MAXIMUM_TEXT_BYTES)
        }
        _ => Err(ClipboardError::UnsupportedCommand),
    }
}

fn decode_compressed_viewer(payload: &[u8]) -> Result<String, ClipboardError> {
    if !payload.starts_with(COMPRESSED_PREFIX) {
        return Err(ClipboardError::UnsupportedCommand);
    }
    let mut cursor = COMPRESSED_PREFIX.len();
    let first_digit = cursor;
    let mut expected = 0usize;
    while cursor < payload.len() && payload[cursor].is_ascii_digit() {
        if cursor - first_digit >= 10 {
            return Err(ClipboardError::MalformedMessage);
        }
        expected = expected * 10 + usize::from(payload[cursor] - b'0');
        if expected > MAXIMUM_TEXT_BYTES {
            return Err(ClipboardError::SizeLimitExceeded);
        }
        cursor += 1;
    }
    if cursor == first_digit || payload.len() - cursor < 3 || payload[cursor..cursor + 3] != *b"|||" {
        return Err(ClipboardError::MalformedMessage);
    }
    cursor += 3;
    decode_compressed_text(&payload[cursor..], expected)
}

fn decode_compressed_text(compressed: &[u8], expected: usize) -> Result<String, ClipboardError> {
    if expected % 2 != 0 {
        return Err(ClipboardError::InvalidText);
    }
    if compressed.is_empty() {
        return Err(ClipboardError::MalformedMessage);
    }
    // UVC ordinal50 uses a fresh one-shot context with no screen dictionary.
    let mut decoder = ZstdScreenDecoder::new(MAXIMUM_TEXT_BYTES, None).map_err(|_| ClipboardError::MalformedMessage)?;
    let decoded = decoder.append(compressed).map_err(|_| ClipboardError::MalformedMessage)?;
    decoder.finish().map_err(|_| ClipboardError::MalformedMessage)?;
    if decoded.len() != expected {
        return Err(ClipboardError::DecompressedLengthMismatch { expected, received: decoded.len() });
    }
    decode_utf16(&decoded, MAXIMUM_TEXT_BYTES)
}

fn decode_legacy_viewer_text(text: &str) -> Result<String, ClipboardError> {
    let content = text.strip_prefix(LEGACY_PREFIX).ok_or(ClipboardError::UnsupportedCommand)?;
    encode_text(content)?;
    Ok(content.to_string())
}

fn encode_text(text: &str) -> Result<Vec<u8>, ClipboardError> {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() > MAXIMUM_TEXT_BYTES / 2 {
        return Err(ClipboardError::SizeLimitExceeded);
    }
    if units.contains(&0) {
        return Err(ClipboardError::InvalidText);
    }
    Ok(utf16le(text))
}

fn decode_utf16(data: &[u8], maximum_bytes: usize) -> Result<String, ClipboardError> {
    if data.len() > maximum_bytes {
        return Err(ClipboardError::SizeLimitExceeded);
    }
    if data.len() % 2 != 0 {
        return Err(ClipboardError::InvalidText);
    }
    let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    if units.contains(&0) {
        return Err(ClipboardError::InvalidText);
    }
    String::from_utf16(&units).map_err(|_| ClipboardError::InvalidText)
}

fn check_payload(count: usize) -> Result<(), ClipboardError> {
    if count > MAXIMUM_PAYLOAD_BYTES {
        Err(ClipboardError::SizeLimitExceeded)
    } else {
        Ok(())
    }
}
