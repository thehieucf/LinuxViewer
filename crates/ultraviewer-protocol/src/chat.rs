//! Chat application messages for an authenticated peer route.

use crate::commands::CommandMessage;
use crate::multipart::{self, Limits};
use crate::util::utf16le;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    Message(String),
    Typing(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChatError {
    #[error("chat text too large")]
    TextTooLarge,
    #[error("invalid chat encoding")]
    InvalidEncoding,
    #[error("invalid chat text")]
    InvalidText,
    #[error("chat text contains an unsupported delimiter")]
    UnsupportedDelimiter,
    #[error("malformed chat message")]
    MalformedMessage,
    #[error("chat payload could not be encoded")]
    Encoding,
}

/// Local resource budget in bytes, checked for both UTF-8 and UTF-16LE.
pub const MAXIMUM_TEXT_BYTES: usize = 32 * 1024;

fn chat_limits() -> Limits {
    Limits::new(MAXIMUM_TEXT_BYTES + 9, MAXIMUM_TEXT_BYTES, 2)
}

/// Form1.SendString: UTF-16LE "12|||<text>", relay format flag true.
pub fn viewer_to_host(text: &str) -> Result<CommandMessage, ChatError> {
    validate(text)?;
    // The Windows host uses an unlimited delimiter split and reads index1.
    if text.contains("|||") {
        return Err(ChatError::UnsupportedDelimiter);
    }
    Ok(CommandMessage { payload: utf16le(&format!("12|||{text}")), format_flag: true })
}

/// The Windows viewer sends these through the same Unicode SendString path.
pub fn viewer_to_host_typing(is_typing: bool) -> CommandMessage {
    CommandMessage { payload: utf16le(if is_typing { "29" } else { "30" }), format_flag: true }
}

/// clsRemoteClient.SendData: multipart [[12], UTF-16LE text].
pub fn host_to_viewer(text: &str) -> Result<Vec<u8>, ChatError> {
    validate(text)?;
    multipart::encode_with(&[vec![12u8], utf16le(text)], chat_limits()).map_err(|_| ChatError::Encoding)
}

pub fn host_to_viewer_typing(is_typing: bool) -> Vec<u8> {
    multipart::encode_with(&[vec![if is_typing { 29u8 } else { 30 }]], chat_limits()).expect("static payload")
}

/// Decode text already normalized from the relay's ANSI/Unicode flag into
/// UTF-8 by the host. Unrelated commands return None; malformed chat errors.
pub fn decode_host_payload(payload: &[u8]) -> Result<Option<ChatEvent>, ChatError> {
    if payload == b"29" {
        return Ok(Some(ChatEvent::Typing(true)));
    }
    if payload == b"30" {
        return Ok(Some(ChatEvent::Typing(false)));
    }
    let Some(body) = payload.strip_prefix(b"12|||") else { return Ok(None) };
    if body.len() > MAXIMUM_TEXT_BYTES {
        return Err(ChatError::TextTooLarge);
    }
    let text = std::str::from_utf8(body).map_err(|_| ChatError::InvalidEncoding)?;
    validate(text)?;
    if text.contains("|||") {
        return Err(ChatError::UnsupportedDelimiter);
    }
    Ok(Some(ChatEvent::Message(text.to_string())))
}

/// Decode the host-to-viewer multipart form after bounded multipart parsing.
pub fn decode_viewer_parts(parts: &[Vec<u8>]) -> Result<Option<ChatEvent>, ChatError> {
    let Some(&command) = parts.first().and_then(|p| p.first()) else { return Ok(None) };
    if ![12, 29, 30].contains(&command) {
        return Ok(None);
    }
    if parts[0].len() != 1 {
        return Err(ChatError::MalformedMessage);
    }
    if command != 12 {
        if parts.len() != 1 {
            return Err(ChatError::MalformedMessage);
        }
        return Ok(Some(ChatEvent::Typing(command == 29)));
    }
    if parts.len() != 2 {
        return Err(ChatError::MalformedMessage);
    }
    let bytes = &parts[1];
    if bytes.len() > MAXIMUM_TEXT_BYTES {
        return Err(ChatError::TextTooLarge);
    }
    let text = strict_utf16le(bytes)?;
    validate(&text)?;
    Ok(Some(ChatEvent::Message(text)))
}

fn validate(text: &str) -> Result<(), ChatError> {
    if text.len() > MAXIMUM_TEXT_BYTES || text.encode_utf16().count() > MAXIMUM_TEXT_BYTES / 2 {
        return Err(ChatError::TextTooLarge);
    }
    // Preserve line breaks and tabs; reject BSTR terminators and other controls.
    if !text.chars().all(|c| c as u32 >= 32 || matches!(c as u32, 9 | 10 | 13)) {
        return Err(ChatError::InvalidText);
    }
    Ok(())
}

pub(crate) fn strict_utf16le(bytes: &[u8]) -> Result<String, ChatError> {
    if bytes.len() % 2 != 0 {
        return Err(ChatError::InvalidEncoding);
    }
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    // Unpaired surrogates are invalid; from_utf16 rejects them.
    String::from_utf16(&units).map_err(|_| ChatError::InvalidEncoding)
}
