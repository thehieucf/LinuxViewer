//! Remote cursor shape/selection/position messages.

use crate::util::{read_i16, read_i32, read_u16, read_u32};
use crate::zstd::ZstdScreenDecoder;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CursorMessageError {
    #[error("unsupported cursor command {0}")]
    UnsupportedCommand(u8),
    #[error("missing cursor part {0}")]
    MissingPart(usize),
    #[error("malformed cursor part {0}")]
    MalformedPart(usize),
    #[error("cursor size limit exceeded")]
    SizeLimitExceeded,
    #[error("cursor length mismatch")]
    DecompressedLengthMismatch { expected: usize, received: usize },
    #[error("invalid cursor file")]
    InvalidCursorFile,
}

/// One CUR directory entry and its original image resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorFile {
    pub file_data: Vec<u8>,
    pub resource_data: Vec<u8>,
    pub image_count: u16,
    pub width: usize,
    pub height: usize,
    pub hot_spot_x: u16,
    pub hot_spot_y: u16,
}

impl CursorFile {
    pub fn decode(data: &[u8], maximum_bytes: usize) -> Result<Self, CursorMessageError> {
        if maximum_bytes == 0 || data.len() > maximum_bytes {
            return Err(CursorMessageError::SizeLimitExceeded);
        }
        let bytes = data;
        if bytes.len() < 22 || read_u16(bytes, 0) != 0 || read_u16(bytes, 2) != 2 {
            return Err(CursorMessageError::InvalidCursorFile);
        }
        let count = read_u16(bytes, 4);
        let directory_end = 6 + usize::from(count) * 16;
        if count == 0 || directory_end > bytes.len() {
            return Err(CursorMessageError::InvalidCursorFile);
        }
        let width = if bytes[6] == 0 { 256 } else { usize::from(bytes[6]) };
        let height = if bytes[7] == 0 { 256 } else { usize::from(bytes[7]) };
        let hot_spot_x = read_u16(bytes, 10);
        let hot_spot_y = read_u16(bytes, 12);
        let size = read_u32(bytes, 14) as usize;
        let offset = read_u32(bytes, 18) as usize;
        if bytes[9] != 0
            || usize::from(hot_spot_x) >= width
            || usize::from(hot_spot_y) >= height
            || size == 0
            || offset < directory_end
            || offset > bytes.len()
            || size > bytes.len() - offset
        {
            return Err(CursorMessageError::InvalidCursorFile);
        }
        Ok(Self {
            file_data: data.to_vec(),
            resource_data: bytes[offset..offset + size].to_vec(),
            image_count: count,
            width,
            height,
            hot_spot_x,
            hot_spot_y,
        })
    }
}

/// Already decrypted multipart cursor messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorMessage {
    Selection { hash: i32 },
    Shape { hash: i32, cursor: CursorFile },
    /// Signed virtual-desktop coordinates, before subtracting virtualLeft/Top.
    Position { x: i16, y: i16 },
}

impl CursorMessage {
    /// This command-3 hash exits the inspected desktop branch without updating
    /// its cursor.
    pub const IGNORED_SELECTION_HASH: i32 = 0xb84d1a70u32 as i32;
    pub const MAXIMUM_BYTES: usize = 2 * 1024 * 1024;

    pub fn decode(parts: &[Vec<u8>]) -> Result<Self, CursorMessageError> {
        Self::decode_with(parts, true, Self::MAXIMUM_BYTES)
    }

    pub fn decode_with(parts: &[Vec<u8>], modern: bool, maximum_bytes: usize) -> Result<Self, CursorMessageError> {
        if maximum_bytes == 0 {
            return Err(CursorMessageError::SizeLimitExceeded);
        }
        let mut total = 0usize;
        for part in parts {
            if part.len() > maximum_bytes - total {
                return Err(CursorMessageError::SizeLimitExceeded);
            }
            total += part.len();
        }
        let command = *parts.first().and_then(|p| p.first()).ok_or(CursorMessageError::MissingPart(0))?;
        match command {
            3 => Ok(Self::Selection { hash: int32(parts, 1)? }),
            4 => {
                let hash = int32(parts, 1)?;
                let data = if modern {
                    let expected = int32(parts, 2)?;
                    if expected <= 0 || expected as usize > maximum_bytes {
                        return Err(CursorMessageError::SizeLimitExceeded);
                    }
                    let compressed = part(parts, 3, 1)?;
                    // UVC ordinal50 creates a fresh decoder and has no dictionary.
                    let data = ZstdScreenDecoder::decode_once(compressed, maximum_bytes)
                        .map_err(|_| CursorMessageError::MalformedPart(3))?;
                    if data.len() != expected as usize {
                        return Err(CursorMessageError::DecompressedLengthMismatch { expected: expected as usize, received: data.len() });
                    }
                    data
                } else {
                    part(parts, 2, 1)?.to_vec()
                };
                Ok(Self::Shape { hash, cursor: CursorFile::decode(&data, maximum_bytes)? })
            }
            28 => Ok(Self::Position { x: int16(parts, 1)?, y: int16(parts, 2)? }),
            other => Err(CursorMessageError::UnsupportedCommand(other)),
        }
    }

    /// Native cache-miss request: decimal ASCII through SendData with false
    /// format flag, before the separate relay routing/framing layer.
    pub fn shape_request(hash: i32) -> Vec<u8> {
        format!("4|||{hash}").into_bytes()
    }
}

fn part(parts: &[Vec<u8>], index: usize, minimum: usize) -> Result<&[u8], CursorMessageError> {
    let part = parts.get(index).ok_or(CursorMessageError::MissingPart(index))?;
    if part.len() < minimum {
        return Err(CursorMessageError::MalformedPart(index));
    }
    Ok(part)
}

fn int16(parts: &[Vec<u8>], index: usize) -> Result<i16, CursorMessageError> {
    Ok(read_i16(part(parts, index, 2)?, 0))
}

fn int32(parts: &[Vec<u8>], index: usize) -> Result<i32, CursorMessageError> {
    Ok(read_i32(part(parts, index, 4)?, 0))
}
