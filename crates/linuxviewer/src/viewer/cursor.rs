//! Remote cursor presentation state only. Never moves the local pointer; the
//! owner decides how to draw it and when to send shape requests.

use std::collections::{HashMap, HashSet, VecDeque};

use gtk::{gdk, glib};
use ultraviewer_protocol::{CursorFile, CursorMessage};

const MAXIMUM_SHAPES: usize = 64;
const MAXIMUM_DECODED_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_ENCODED_BYTES: usize = 2 * 1024 * 1024;
const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

#[derive(Clone)]
pub struct Shape {
    pub texture: gdk::Texture,
    pub hot_spot: (f64, f64),
    bytes: usize,
}

#[derive(Clone)]
pub struct Snapshot {
    pub shape: Shape,
    /// Unscaled remote virtual-desktop coordinates.
    pub position: Option<(f64, f64)>,
}

pub struct CursorPresenter {
    snapshot: Snapshot,
    selected: Option<i32>,
    fallback: Shape,
    cache: HashMap<i32, Shape>,
    cache_bytes: usize,
    recency: VecDeque<i32>,
    pending: HashSet<i32>,
    pending_order: VecDeque<i32>,
}

impl CursorPresenter {
    pub fn new() -> Self {
        let fallback = arrow_shape();
        Self {
            snapshot: Snapshot { shape: fallback.clone(), position: None },
            selected: None,
            fallback,
            cache: HashMap::new(),
            cache_bytes: 0,
            recency: VecDeque::new(),
            pending: HashSet::new(),
            pending_order: VecDeque::new(),
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Returns a newly missing selection hash for `CursorMessage::shape_request`.
    pub fn consume(&mut self, message: CursorMessage) -> Option<i32> {
        match message {
            CursorMessage::Selection { hash } => {
                if hash == CursorMessage::IGNORED_SELECTION_HASH {
                    return None;
                }
                self.selected = Some(hash);
                if let Some(shape) = self.cache.get(&hash).cloned() {
                    self.touch(hash);
                    self.snapshot.shape = shape;
                    return None;
                }
                if self.pending.contains(&hash) {
                    return None;
                }
                while self.pending_order.len() >= MAXIMUM_SHAPES {
                    if let Some(oldest) = self.pending_order.pop_front() {
                        self.pending.remove(&oldest);
                    }
                }
                self.pending.insert(hash);
                self.pending_order.push_back(hash);
                Some(hash)
            }
            CursorMessage::Shape { hash, cursor } => {
                self.pending.remove(&hash);
                self.pending_order.retain(|&h| h != hash);
                let mut shape = decode(&cursor).unwrap_or_else(|| self.fallback.clone());
                if shape.bytes > MAXIMUM_DECODED_BYTES {
                    shape = self.fallback.clone();
                }
                if let Some(previous) = self.cache.remove(&hash) {
                    self.cache_bytes -= previous.bytes;
                }
                self.recency.retain(|&h| h != hash);
                while !self.recency.is_empty() && (self.cache.len() >= MAXIMUM_SHAPES || shape.bytes > MAXIMUM_DECODED_BYTES - self.cache_bytes) {
                    if let Some(oldest) = self.recency.pop_front() {
                        if let Some(removed) = self.cache.remove(&oldest) {
                            self.cache_bytes -= removed.bytes;
                        }
                    }
                }
                self.cache_bytes += shape.bytes;
                self.cache.insert(hash, shape.clone());
                self.recency.push_back(hash);
                if self.selected == Some(hash) {
                    self.snapshot.shape = shape;
                }
                None
            }
            CursorMessage::Position { x, y } => {
                self.snapshot.position = Some((f64::from(x), f64::from(y)));
                None
            }
        }
    }

    pub fn reset(&mut self) {
        self.cache.clear();
        self.recency.clear();
        self.pending.clear();
        self.pending_order.clear();
        self.cache_bytes = 0;
        self.selected = None;
        self.snapshot = Snapshot { shape: self.fallback.clone(), position: None };
    }

    fn touch(&mut self, hash: i32) {
        self.recency.retain(|&h| h != hash);
        self.recency.push_back(hash);
    }
}

fn word(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn long(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

fn texture_from_bgra(width: usize, height: usize, pixels: Vec<u8>) -> gdk::Texture {
    let bytes = glib::Bytes::from_owned(pixels);
    gdk::MemoryTexture::new(width as i32, height as i32, gdk::MemoryFormat::B8g8r8a8, &bytes, width * 4).upcast()
}

use gtk::prelude::*;

fn decode(cursor: &CursorFile) -> Option<Shape> {
    if cursor.file_data.len() > MAXIMUM_ENCODED_BYTES
        || cursor.resource_data.len() > MAXIMUM_ENCODED_BYTES
        || !(1..=256).contains(&cursor.width)
        || !(1..=256).contains(&cursor.height)
        || usize::from(cursor.hot_spot_x) >= cursor.width
        || usize::from(cursor.hot_spot_y) >= cursor.height
        || !resource_geometry_is_safe(cursor)
    {
        return None;
    }
    let hot_spot = (f64::from(cursor.hot_spot_x), f64::from(cursor.hot_spot_y));
    let bytes = cursor.width * cursor.height * 4;
    if !cursor.resource_data.starts_with(&PNG_SIGNATURE) {
        let image = decode_uncompressed_dib(&cursor.resource_data, cursor.width, cursor.height)?;
        return Some(Shape { texture: texture_from_bgra(cursor.width, cursor.height, image), hot_spot, bytes });
    }
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from(&cursor.resource_data)).ok()?;
    if texture.width() as usize != cursor.width || texture.height() as usize != cursor.height {
        return None;
    }
    Some(Shape { texture, hot_spot, bytes })
}

/// Check the underlying resource dimensions before trusting a directory entry.
fn resource_geometry_is_safe(cursor: &CursorFile) -> bool {
    let data = &cursor.resource_data;
    if data.starts_with(&PNG_SIGNATURE) {
        // IHDR width/height follow the 8-byte signature and 8-byte chunk header.
        if data.len() < 24 {
            return false;
        }
        let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]) as usize;
        let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]) as usize;
        return width == cursor.width && height == cursor.height;
    }
    if data.len() < 12 {
        return false;
    }
    let header = long(data, 0) as usize;
    if header == 12 {
        return usize::from(word(data, 4)) == cursor.width
            && usize::from(word(data, 6)) == cursor.height * 2
            && word(data, 8) == 1
            && [1, 4, 8, 24].contains(&word(data, 10));
    }
    if header < 40 || header > data.len() || data.len() < 40 {
        return false;
    }
    let width = long(data, 4) as i32;
    let doubled = long(data, 8) as i32;
    width as usize == cursor.width
        && (doubled == cursor.height as i32 * 2 || doubled == -(cursor.height as i32) * 2)
        && word(data, 12) == 1
        && [1, 4, 8, 16, 24, 32].contains(&word(data, 14))
        && [0, 3, 6].contains(&long(data, 16))
}

/// Packed CUR DIB: header, optional palette, DWORD-padded XOR rows, then
/// DWORD-padded 1bpp AND rows. Output is straight-alpha BGRA, top-down.
fn decode_uncompressed_dib(bytes: &[u8], width: usize, height: usize) -> Option<Vec<u8>> {
    let header = long(bytes, 0) as usize;
    if ![12, 40, 52, 56, 108, 124].contains(&header) || header > bytes.len() {
        return None;
    }
    let core = header == 12;
    let bits = usize::from(word(bytes, if core { 10 } else { 14 }));
    if ![1, 4, 8, 16, 24, 32].contains(&bits) || (!core && long(bytes, 16) != 0) {
        return None;
    }
    let top_down = !core && (long(bytes, 8) as i32) < 0;
    let declared = if core { 0 } else { long(bytes, 32) as usize };
    let colors = if bits <= 8 {
        let count = if declared == 0 { 1 << bits } else { declared };
        if count == 0 || count > 1 << bits {
            return None;
        }
        count
    } else {
        if declared > 256 {
            return None;
        }
        declared
    };
    let palette_stride = if core { 3 } else { 4 };
    let pixels_offset = header + colors * palette_stride;
    let stride = (width * bits).div_ceil(32) * 4;
    let mask_stride = width.div_ceil(32) * 4;
    let mask_offset = pixels_offset + stride * height;
    if pixels_offset > bytes.len() || mask_offset > bytes.len() || mask_stride * height > bytes.len() - mask_offset {
        return None;
    }
    // Windows interprets 32bpp all-zero alpha as legacy 0RGB + AND/XOR;
    // otherwise alpha is authoritative and the AND mask is ignored.
    let mut uses_alpha = false;
    if bits == 32 {
        'outer: for row in 0..height {
            for x in 0..width {
                if bytes[pixels_offset + row * stride + x * 4 + 3] != 0 {
                    uses_alpha = true;
                    break 'outer;
                }
            }
        }
    }
    let mut output = vec![0u8; width * height * 4];
    for y in 0..height {
        let source_y = if top_down { y } else { height - 1 - y };
        let row = pixels_offset + source_y * stride;
        let mask_row = mask_offset + source_y * mask_stride;
        for x in 0..width {
            let (blue, green, red, mut alpha) = match bits {
                1 | 4 | 8 => {
                    let index = match bits {
                        1 => usize::from((bytes[row + x / 8] >> (7 - x % 8)) & 1),
                        4 => usize::from((bytes[row + x / 2] >> if x % 2 == 0 { 4 } else { 0 }) & 15),
                        _ => usize::from(bytes[row + x]),
                    };
                    if index >= colors {
                        return None;
                    }
                    let entry = header + index * palette_stride;
                    (bytes[entry], bytes[entry + 1], bytes[entry + 2], 255u8)
                }
                16 => {
                    let value = word(bytes, row + x * 2);
                    let expand = |v: u16| ((v << 3) | (v >> 2)) as u8;
                    (expand(value & 31), expand((value >> 5) & 31), expand((value >> 10) & 31), 255u8)
                }
                _ => {
                    let offset = row + x * (bits / 8);
                    let alpha = if uses_alpha { bytes[offset + 3] } else { 255 };
                    (bytes[offset], bytes[offset + 1], bytes[offset + 2], alpha)
                }
            };
            if !uses_alpha && bytes[mask_row + x / 8] & (0x80 >> (x % 8)) != 0 {
                // AND=1/XOR!=0 needs desktop compositing (e.g. an inverted
                // I-beam); it cannot be a static image.
                if blue != 0 || green != 0 || red != 0 {
                    return None;
                }
                alpha = 0;
            }
            let offset = (y * width + x) * 4;
            if alpha != 0 {
                output[offset] = blue;
                output[offset + 1] = green;
                output[offset + 2] = red;
            }
            output[offset + 3] = alpha;
        }
    }
    Some(output)
}

/// A bounded standard arrow (black fill, white outline).
fn arrow_shape() -> Shape {
    const ARROW: [&str; 19] = [
        "W           ",
        "WW          ",
        "WBW         ",
        "WBBW        ",
        "WBBBW       ",
        "WBBBBW      ",
        "WBBBBBW     ",
        "WBBBBBBW    ",
        "WBBBBBBBW   ",
        "WBBBBBBBBW  ",
        "WBBBBBBBBBW ",
        "WBBBBBBWWWWW",
        "WBBBWBBW    ",
        "WBBWWBBW    ",
        "WBW  WBBW   ",
        "WW   WBBW   ",
        "W     WBBW  ",
        "      WBBW  ",
        "       WW   ",
    ];
    let (width, height) = (12usize, 19usize);
    let mut pixels = vec![0u8; width * height * 4];
    for (y, line) in ARROW.iter().enumerate() {
        for (x, c) in line.bytes().enumerate() {
            let offset = (y * width + x) * 4;
            match c {
                b'W' => pixels[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]),
                b'B' => pixels[offset..offset + 4].copy_from_slice(&[0, 0, 0, 255]),
                _ => {}
            }
        }
    }
    Shape { texture: texture_from_bgra(width, height, pixels), hot_spot: (0.0, 0.0), bytes: 0 }
}
