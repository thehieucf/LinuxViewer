//! Recovered desktop screen command layouts for targets at least version 6.0.0.
//! This accepts an already decrypted and split multipart payload, not TCP bytes.

use crate::util::{read_i16, read_i32, read_u16};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScreenError {
    #[error("Windows sent an unsupported screen update ({0}).")]
    UnsupportedCommand(u8),
    #[error("Windows sent an incomplete screen update (missing part {0}).")]
    MissingPart(usize),
    #[error("Windows sent an incomplete screen update (part {0}).")]
    MalformedPart(usize),
    #[error("Windows sent an invalid screen rectangle.")]
    InvalidGeometry,
    #[error("The Windows screen update exceeds the supported size.")]
    SizeLimitExceeded,
    #[error("The Windows screen color mode is not supported.")]
    UnsupportedColorMode,
    #[error("Windows screen updates arrived out of order (expected {expected}, received {received}).")]
    SequenceMismatch { expected: i64, received: i32 },
    #[error("The Windows screen update has an invalid pixel count (expected {expected}, received {received}).")]
    DecompressedLengthMismatch { expected: usize, received: usize },
    #[error("A Windows screen update refers to a missing image tile ({0}).")]
    CacheMiss(i32),
    #[error("The Windows screen image cache is full.")]
    CacheBudgetExceeded,
    #[error("Windows sent a screen image that could not be decoded.")]
    InvalidJpeg,
    #[error("Windows sent a screen image that could not be decoded.")]
    UnsupportedJpeg,
    #[error("A Windows screen update contains an invalid image tile (atlas {atlas_width}×{atlas_height}, tile {tile_size}, \
             {tiles} tiles in {rows} rows, screen {screen_width}×{screen_height}).")]
    AuxiliaryAtlasOutOfBounds {
        atlas_width: usize,
        atlas_height: usize,
        tile_size: usize,
        tiles: usize,
        rows: usize,
        screen_width: usize,
        screen_height: usize,
    },
    #[error("The Windows screen update could not be decompressed: {0}")]
    Decompression(#[from] crate::zstd::ZstdDecodeError),
}

/// Implementation limits, not protocol constants recovered from the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModernScreenLimits {
    pub maximum_dimension: usize,
    pub maximum_pixels: usize,
    pub maximum_payload_bytes: usize,
    pub maximum_decompressed_bytes: usize,
    pub maximum_records_per_part: usize,
    pub maximum_cache_bytes: usize,
    pub maximum_auxiliary_tiles: usize,
    pub maximum_atlas_dimension: usize,
}

impl Default for ModernScreenLimits {
    fn default() -> Self {
        Self {
            maximum_dimension: 8192,
            maximum_pixels: 16 * 1024 * 1024,
            maximum_payload_bytes: 64 * 1024 * 1024,
            maximum_decompressed_bytes: 128 * 1024 * 1024,
            maximum_records_per_part: 16_384,
            maximum_cache_bytes: 64 * 1024 * 1024,
            maximum_auxiliary_tiles: 1_048_576,
            maximum_atlas_dimension: 65_535,
        }
    }
}

impl ModernScreenLimits {
    pub(crate) fn validate(&self) -> Result<(), ScreenError> {
        let values = [
            self.maximum_dimension, self.maximum_pixels, self.maximum_payload_bytes, self.maximum_decompressed_bytes,
            self.maximum_records_per_part, self.maximum_cache_bytes, self.maximum_auxiliary_tiles,
            self.maximum_atlas_dimension,
        ];
        if values.iter().all(|&v| v > 0) {
            Ok(())
        } else {
            Err(ScreenError::SizeLimitExceeded)
        }
    }

    /// Atlas tiles are packed into a narrow, tall JPEG. Its dimensions are
    /// independent of screen geometry.
    pub fn check_atlas_geometry(&self, width: i64, height: i64) -> Result<(), ScreenError> {
        if width <= 0 || height <= 0 {
            return Err(ScreenError::InvalidGeometry);
        }
        if width as usize > self.maximum_atlas_dimension
            || height as usize > self.maximum_atlas_dimension
            || width as usize > self.maximum_pixels / height as usize
        {
            return Err(ScreenError::SizeLimitExceeded);
        }
        Ok(())
    }

    pub fn check_geometry(&self, width: i64, height: i64) -> Result<(), ScreenError> {
        if width <= 0 || height <= 0 {
            return Err(ScreenError::InvalidGeometry);
        }
        if width as usize > self.maximum_dimension
            || height as usize > self.maximum_dimension
            || width as usize > self.maximum_pixels / height as usize
        {
            return Err(ScreenError::SizeLimitExceeded);
        }
        Ok(())
    }

    pub(crate) fn check_parts(&self, parts: &[Vec<u8>]) -> Result<(), ScreenError> {
        self.validate()?;
        let mut total = 0usize;
        for part in parts {
            if part.len() > self.maximum_payload_bytes - total {
                return Err(ScreenError::SizeLimitExceeded);
            }
            total += part.len();
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenRegion {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl ScreenRegion {
    pub fn new(x: i64, y: i64, width: i64, height: i64) -> Self {
        Self { x, y, width, height }
    }
}

/// The known screen fields of command 0. Unknown init parts remain uninterpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModernScreenInitialization {
    pub width: usize,
    pub height: usize,
    pub tile_size: usize,
    pub target_version_bytes: Option<Vec<u8>>,
    pub monitor_information_bytes: Option<Vec<u8>>,
    pub virtual_left: Option<i32>,
    pub virtual_top: Option<i32>,
}

impl ModernScreenInitialization {
    pub fn decode(parts: &[Vec<u8>]) -> Result<Self, ScreenError> {
        Self::decode_with(parts, &ModernScreenLimits::default())
    }

    pub fn decode_with(parts: &[Vec<u8>], limits: &ModernScreenLimits) -> Result<Self, ScreenError> {
        limits.check_parts(parts)?;
        expect_command(parts, 0)?;
        let width = int32_part(parts, 1)? as i64;
        let height = int32_part(parts, 2)? as i64;
        limits.check_geometry(width, height)?;
        // Native optional branches consume these slots' first values whenever
        // present, regardless of the previous screen geometry.
        for index in [5usize, 11] {
            if parts.len() > index && parts[index].is_empty() {
                return Err(ScreenError::MalformedPart(index));
            }
        }
        if parts.len() > 6 {
            if parts.len() <= 7 {
                return Err(ScreenError::MissingPart(7));
            }
            if parts[7].is_empty() {
                return Err(ScreenError::MalformedPart(7));
            }
        }
        if parts.len() > 12 && parts[12].len() < 2 {
            return Err(ScreenError::MalformedPart(12));
        }
        let (mut left, mut top) = (None, None);
        if parts.len() > 8 {
            left = Some(int32_part(parts, 9)?);
            top = Some(int32_part(parts, 10)?);
        }
        let tile_size = if parts.len() > 14 {
            match parts[14].first() {
                Some(&value) if value > 0 => value as usize,
                _ => return Err(ScreenError::MalformedPart(14)),
            }
        } else {
            16
        };
        Ok(Self {
            width: width as usize,
            height: height as usize,
            tile_size,
            target_version_bytes: (parts.len() > 6).then(|| parts[6].clone()),
            monitor_information_bytes: (parts.len() > 8).then(|| parts[8].clone()),
            virtual_left: left,
            virtual_top: top,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scroll {
    pub vertical_delta: i64,
    pub capture: ScreenRegion,
    pub copy_back: Vec<ScreenRegion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheReference {
    pub x: i64,
    pub y: i64,
    pub hash: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheAddition {
    pub region: ScreenRegion,
    pub hash: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModernScreenFrameMessage {
    pub frame_number: i32,
    pub main_rectangles: Vec<ScreenRegion>,
    pub compressed_pixels: Vec<u8>,
    pub copy_from_temporary: Vec<ScreenRegion>,
    pub scroll: Option<Scroll>,
    pub auxiliary_jpeg: Vec<u8>,
    pub auxiliary_rectangles: Vec<ScreenRegion>,
    pub cache_references: Vec<CacheReference>,
    pub cache_additions: Vec<CacheAddition>,
}

impl ModernScreenFrameMessage {
    pub fn decode(parts: &[Vec<u8>]) -> Result<Self, ScreenError> {
        Self::decode_with(parts, &ModernScreenLimits::default())
    }

    /// At least eleven slots are consumed by the original modern branch.
    /// Part 3, extra header bytes, and trailing slots are ignored by that branch.
    pub fn decode_with(parts: &[Vec<u8>], limits: &ModernScreenLimits) -> Result<Self, ScreenError> {
        limits.check_parts(parts)?;
        expect_command(parts, 1)?;
        if parts.len() <= 10 {
            return Err(ScreenError::MissingPart(parts.len()));
        }
        let main = regions(&parts[1], 1, false, limits)?;
        let copy = regions(&parts[5], 5, true, limits)?;
        let auxiliary = regions(&parts[8], 8, true, limits)?;
        let scroll = if parts[6].is_empty() {
            None
        } else {
            let bytes = &parts[6];
            if bytes.len() < 10 || (bytes.len() - 10) % 8 != 0 || (bytes.len() - 10) / 8 > limits.maximum_records_per_part {
                return Err(ScreenError::MalformedPart(6));
            }
            let capture = region(bytes, 2, true, limits)?;
            Some(Scroll {
                vertical_delta: i64::from(read_i16(bytes, 0)),
                capture,
                copy_back: regions(&bytes[10..], 6, true, limits)?,
            })
        };
        records(&parts[9], 9, 8, limits)?;
        let references = parts[9]
            .chunks_exact(8)
            .map(|r| CacheReference { x: i64::from(read_i16(r, 0)), y: i64::from(read_i16(r, 2)), hash: read_i32(r, 4) })
            .collect();
        records(&parts[10], 10, 12, limits)?;
        let mut additions = Vec::with_capacity(parts[10].len() / 12);
        for record in parts[10].chunks_exact(12) {
            additions.push(CacheAddition { region: region(record, 0, true, limits)?, hash: read_i32(record, 8) });
        }
        Ok(Self {
            frame_number: int32_part(parts, 4)?,
            main_rectangles: main,
            compressed_pixels: parts[2].clone(),
            copy_from_temporary: copy,
            scroll,
            auxiliary_jpeg: parts[7].clone(),
            auxiliary_rectangles: auxiliary,
            cache_references: references,
            cache_additions: additions,
        })
    }
}

pub(crate) fn expect_command(parts: &[Vec<u8>], expected: u8) -> Result<(), ScreenError> {
    let first = parts.first().ok_or(ScreenError::MissingPart(0))?;
    let command = *first.first().ok_or(ScreenError::MalformedPart(0))?;
    if command != expected {
        return Err(ScreenError::UnsupportedCommand(command));
    }
    Ok(())
}

pub(crate) fn int32_part(parts: &[Vec<u8>], index: usize) -> Result<i32, ScreenError> {
    let part = parts.get(index).ok_or(ScreenError::MissingPart(index))?;
    // Minimum consumed length is recovered; trailing bytes are ignored by native code.
    if part.len() < 4 {
        return Err(ScreenError::MalformedPart(index));
    }
    Ok(read_i32(part, 0))
}

fn records(data: &[u8], part: usize, size: usize, limits: &ModernScreenLimits) -> Result<(), ScreenError> {
    if data.len() % size != 0 {
        return Err(ScreenError::MalformedPart(part));
    }
    if data.len() / size > limits.maximum_records_per_part {
        return Err(ScreenError::SizeLimitExceeded);
    }
    Ok(())
}

fn regions(data: &[u8], part: usize, signed: bool, limits: &ModernScreenLimits) -> Result<Vec<ScreenRegion>, ScreenError> {
    records(data, part, 8, limits)?;
    (0..data.len()).step_by(8).map(|offset| region(data, offset, signed, limits)).collect()
}

fn region(bytes: &[u8], offset: usize, signed: bool, limits: &ModernScreenLimits) -> Result<ScreenRegion, ScreenError> {
    let read = |at: usize| -> i64 {
        if signed {
            i64::from(read_i16(bytes, at))
        } else {
            i64::from(read_u16(bytes, at))
        }
    };
    let result = ScreenRegion::new(read(offset), read(offset + 2), read(offset + 4), read(offset + 6));
    limits.check_geometry(result.width, result.height)?;
    Ok(result)
}
