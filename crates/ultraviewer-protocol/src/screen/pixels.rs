//! Internal pixel kernels and the uvc.dll ordinal 24 row-copy primitive.

use std::sync::OnceLock;

fn rgb555_table() -> &'static [u32; 32_768] {
    static TABLE: OnceLock<Box<[u32; 32_768]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Box::new([0u32; 32_768]);
        for (value, entry) in table.iter_mut().enumerate() {
            let expand = |five: usize| ((five << 3) | (five >> 2)) as u32;
            let blue = expand(value & 31);
            let green = expand((value >> 5) & 31);
            let red = expand((value >> 10) & 31);
            *entry = blue | (green << 8) | (red << 16) | 0xff00_0000;
        }
        table
    })
}

/// Expands little-endian RGB555 words into opaque BGRA bytes.
#[inline]
pub(crate) fn expand_row(source: &[u8], destination: &mut [u8], width: usize) {
    let table = rgb555_table();
    for (input, output) in source[..width * 2].chunks_exact(2).zip(destination[..width * 4].chunks_exact_mut(4)) {
        let value = (u16::from(input[0]) | (u16::from(input[1]) << 8)) & 0x7fff;
        output.copy_from_slice(&table[value as usize].to_le_bytes());
    }
}

/// Packs BGRA bytes into little-endian RGB555 words.
#[inline]
pub(crate) fn pack_row(source: &[u8], destination: &mut [u8], width: usize) {
    for (pixel, output) in source[..width * 4].chunks_exact(4).zip(destination[..width * 2].chunks_exact_mut(2)) {
        let value = u16::from(pixel[0] >> 3) | (u16::from(pixel[1] >> 3) << 5) | (u16::from(pixel[2] >> 3) << 10);
        output.copy_from_slice(&value.to_le_bytes());
    }
}

pub(crate) fn opaque_black(width: usize, height: usize) -> Vec<u8> {
    let mut pixels = vec![0u8; width * height * 4];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    pixels
}

/// Compatibility/export view only. Display uses the original BGRA storage.
pub fn bgr24_from_bgra(pixels: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(pixels.len() / 4 * 3);
    for pixel in pixels.chunks_exact(4) {
        result.extend_from_slice(&pixel[..3]);
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RectangleBufferError {
    #[error("invalid geometry")]
    InvalidGeometry,
    #[error("invalid stride")]
    InvalidStride,
    #[error("invalid destination length")]
    InvalidDestinationLength,
    #[error("truncated source")]
    TruncatedSource,
    #[error("size overflow")]
    SizeOverflow,
}

/// Raw row-copy operation recovered from uvc.dll ordinal 24 (VA 0x100016b0).
/// Destination rows are stored bottom-up, including caller-supplied row padding.
/// Pixel channels are opaque: this type does not assume RGB/BGR/alpha ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenRectangleBuffer {
    pub width: usize,
    pub height: usize,
    pub bytes_per_pixel: usize,
    pub row_stride: usize,
    pub pixels: Vec<u8>,
}

impl ScreenRectangleBuffer {
    pub fn new(width: usize, height: usize, bytes_per_pixel: usize, row_stride: usize, pixels: Vec<u8>) -> Result<Self, RectangleBufferError> {
        if width == 0 || height == 0 || bytes_per_pixel == 0 {
            return Err(RectangleBufferError::InvalidGeometry);
        }
        let minimum = width.checked_mul(bytes_per_pixel).ok_or(RectangleBufferError::SizeOverflow)?;
        if row_stride < minimum {
            return Err(RectangleBufferError::InvalidStride);
        }
        let size = height.checked_mul(row_stride).ok_or(RectangleBufferError::SizeOverflow)?;
        if pixels.len() != size {
            return Err(RectangleBufferError::InvalidDestinationLength);
        }
        Ok(Self { width, height, bytes_per_pixel, row_stride, pixels })
    }

    /// Copies a tightly packed, top-down source rectangle at top-left coordinates.
    /// Clips against destination bounds and preserves all untouched bytes/padding.
    pub fn copy_rectangle(&mut self, source: &[u8], source_width: i64, source_height: i64, x: i64, y: i64) -> Result<(), RectangleBufferError> {
        if source_width <= 0 || source_height <= 0 {
            return Err(RectangleBufferError::InvalidGeometry);
        }
        let source_stride = (source_width as usize).checked_mul(self.bytes_per_pixel).ok_or(RectangleBufferError::SizeOverflow)?;
        let required = source_stride.checked_mul(source_height as usize).ok_or(RectangleBufferError::SizeOverflow)?;
        if source.len() < required {
            return Err(RectangleBufferError::TruncatedSource);
        }
        let (width, height) = (self.width as i64, self.height as i64);
        if !(x < width && y < height && x > -source_width && y > -source_height) {
            return Ok(());
        }
        let source_x = if x < 0 { -x } else { 0 } as usize;
        let source_y = if y < 0 { -y } else { 0 } as usize;
        let destination_x = x.max(0) as usize;
        let destination_y = y.max(0) as usize;
        let copy_width = (source_width as usize - source_x).min(self.width - destination_x);
        let copy_height = (source_height as usize - source_y).min(self.height - destination_y);
        let copy_bytes = copy_width * self.bytes_per_pixel;
        for row in 0..copy_height {
            let source_offset = (source_y + row) * source_stride + source_x * self.bytes_per_pixel;
            let destination_offset = (self.height - 1 - (destination_y + row)) * self.row_stride + destination_x * self.bytes_per_pixel;
            self.pixels[destination_offset..destination_offset + copy_bytes]
                .copy_from_slice(&source[source_offset..source_offset + copy_bytes]);
        }
        Ok(())
    }
}
