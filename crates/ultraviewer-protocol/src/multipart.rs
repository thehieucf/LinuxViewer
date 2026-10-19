//! Application parts reconstructed from RemoteControl.dll helpers 0x06000850
//! and 0x06000852. This sits inside the relay/direct envelope; it has no count,
//! command interpretation, encryption or outer transport length of its own.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub maximum_payload_length: usize,
    pub maximum_part_length: usize,
    pub maximum_part_count: usize,
}

impl Default for Limits {
    /// These are local resource limits, not negotiated UltraViewer values.
    fn default() -> Self {
        Self {
            maximum_payload_length: 16 * 1024 * 1024,
            maximum_part_length: 16 * 1024 * 1024,
            maximum_part_count: 4096,
        }
    }
}

impl Limits {
    pub fn new(maximum_payload_length: usize, maximum_part_length: usize, maximum_part_count: usize) -> Self {
        assert!((4..=i32::MAX as usize).contains(&maximum_payload_length));
        assert!(maximum_part_length <= i32::MAX as usize);
        assert!(maximum_part_count > 0);
        Self { maximum_payload_length, maximum_part_length, maximum_part_count }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MultipartError {
    #[error("empty payload")]
    EmptyPayload,
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("part too large")]
    PartTooLarge,
    #[error("too many parts")]
    TooManyParts,
    #[error("invalid part length")]
    InvalidLength,
    #[error("truncated part length")]
    TruncatedLength,
    #[error("truncated part")]
    TruncatedPart,
}

/// Each part is UInt32LE length followed by those bytes. A Windows null
/// entry and an empty byte array both serialize as a zero-length part.
pub fn encode<P: AsRef<[u8]>>(parts: &[P]) -> Result<Vec<u8>, MultipartError> {
    encode_with(parts, Limits::default())
}

pub fn encode_with<P: AsRef<[u8]>>(parts: &[P], limits: Limits) -> Result<Vec<u8>, MultipartError> {
    if parts.is_empty() {
        return Err(MultipartError::EmptyPayload);
    }
    if parts.len() > limits.maximum_part_count {
        return Err(MultipartError::TooManyParts);
    }
    let mut total: usize = 0;
    for part in parts {
        let part = part.as_ref();
        if part.len() > limits.maximum_part_length {
            return Err(MultipartError::PartTooLarge);
        }
        total = total
            .checked_add(4)
            .and_then(|value| value.checked_add(part.len()))
            .filter(|value| *value <= limits.maximum_payload_length)
            .ok_or(MultipartError::PayloadTooLarge)?;
    }
    let mut result = Vec::with_capacity(total);
    for part in parts {
        let part = part.as_ref();
        result.extend_from_slice(&(part.len() as u32).to_le_bytes());
        result.extend_from_slice(part);
    }
    Ok(result)
}

/// Decodes a complete application payload. Truncated records are errors,
/// since transport framing has already established the payload boundary.
/// Empty payload rejection is local policy; empty individual parts are valid.
pub fn decode(payload: &[u8]) -> Result<Vec<Vec<u8>>, MultipartError> {
    decode_with(payload, Limits::default())
}

pub fn decode_with(payload: &[u8], limits: Limits) -> Result<Vec<Vec<u8>>, MultipartError> {
    if payload.is_empty() {
        return Err(MultipartError::EmptyPayload);
    }
    if payload.len() > limits.maximum_payload_length {
        return Err(MultipartError::PayloadTooLarge);
    }
    let mut parts = Vec::new();
    let mut cursor = 0;
    while cursor < payload.len() {
        if parts.len() >= limits.maximum_part_count {
            return Err(MultipartError::TooManyParts);
        }
        if payload.len() - cursor < 4 {
            return Err(MultipartError::TruncatedLength);
        }
        let length = crate::util::read_u32(payload, cursor);
        cursor += 4;
        // The Windows reader uses an overflow-checked UInt32-to-Int32 cast.
        if length > i32::MAX as u32 {
            return Err(MultipartError::InvalidLength);
        }
        let length = length as usize;
        if length > limits.maximum_part_length {
            return Err(MultipartError::PartTooLarge);
        }
        if length > payload.len() - cursor {
            return Err(MultipartError::TruncatedPart);
        }
        parts.push(payload[cursor..cursor + length].to_vec());
        cursor += length;
    }
    Ok(parts)
}
