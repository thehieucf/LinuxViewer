//! Implements the recovered >=6.0.0 desktop pipeline using the 16bpp temporary
//! converter mode (DisplayColors != 1). Initialization and image decoding do
//! not establish a remote session.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use super::message::{ModernScreenFrameMessage, ModernScreenLimits, ScreenError, ScreenRegion};
use super::pixels::{bgr24_from_bgra, expand_row, opaque_black, pack_row, ScreenRectangleBuffer};
use crate::jpeg::JpegDecoder;
use crate::zstd::ZstdScreenDecoder;

/// A completed software-rendered frame: top-down, tightly packed opaque BGRA
/// (byte order B, G, R, A), shared without copying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModernScreenBitmap {
    pub width: usize,
    pub height: usize,
    pub bgra: Arc<Vec<u8>>,
}

impl ModernScreenBitmap {
    /// Compatibility/export view: tightly packed top-down BGR24.
    pub fn bgr_pixels(&self) -> Vec<u8> {
        bgr24_from_bgra(&self.bgra)
    }

    pub fn row_stride(&self) -> usize {
        self.width * 4
    }
}

#[derive(Debug)]
struct CachedRectangle {
    width: i64,
    height: i64,
    /// Tightly packed top-down RGB555, little endian.
    pixels: Vec<u8>,
}

struct TileCopy {
    source_x: usize,
    source_y: usize,
    destination_x: usize,
    destination_y: usize,
    width: usize,
    height: usize,
}

struct Atlas {
    width: usize,
    height: usize,
    /// BGRA32 rows stored bottom-up, exactly as native TurboJPEG flags 1026.
    pixels: Vec<u8>,
}

pub struct ModernScreenRenderer {
    pub width: usize,
    pub height: usize,
    tile_size: usize,
    last_frame_number: i32,
    pub limits: ModernScreenLimits,
    /// Top-down opaque BGRA32, separate from the converter buffer.
    main_pixels: Vec<u8>,
    /// Bottom-up RGB555 with DWORD row stride.
    temporary: ScreenRectangleBuffer,
    cache: HashMap<i32, CachedRectangle>,
    /// Oldest first; native maximum is 1000.
    cache_order: VecDeque<i32>,
    cache_bytes: usize,
    decompressor: ZstdScreenDecoder,
    jpeg: Option<JpegDecoder>,
    damage: Option<(usize, usize, usize, usize)>,
}

impl ModernScreenRenderer {
    /// Form1.ResetLastMsgNumber, VA 0x6eb050: push 0x80000001. OnConnected and
    /// inbound command 26 invoke this reset.
    pub const RESET_FRAME_NUMBER: i32 = -2_147_483_647;

    /// `last_frame_number` is explicit: the desktop requires incoming ==
    /// previous+1 in modern live mode. displayColors == 1 is rejected because
    /// that mode needs the custom palette.
    pub fn new(width: usize, height: usize, tile_size: usize, display_colors: i32, last_frame_number: i32,
               limits: ModernScreenLimits, dictionary: Option<&[u8]>) -> Result<Self, ScreenError> {
        limits.validate()?;
        limits.check_geometry(width as i64, height as i64)?;
        if display_colors == 1 {
            return Err(ScreenError::UnsupportedColorMode);
        }
        if !(1..=255).contains(&tile_size) {
            return Err(ScreenError::InvalidGeometry);
        }
        let pixel_count = product(width, height)?;
        product(pixel_count, 4)?;
        let raw_row = product(width, 2)?;
        let stride = (raw_row + 3) & !3;
        let temporary = ScreenRectangleBuffer::new(width, height, 2, stride, vec![0; product(stride, height)?])
            .map_err(|_| ScreenError::SizeLimitExceeded)?;
        let decompressor = ZstdScreenDecoder::new(limits.maximum_decompressed_bytes, dictionary)?;
        Ok(Self {
            width,
            height,
            tile_size,
            last_frame_number,
            limits,
            main_pixels: opaque_black(width, height),
            temporary,
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            cache_bytes: 0,
            decompressor,
            jpeg: None,
            damage: None,
        })
    }

    pub fn with_defaults(width: usize, height: usize, display_colors: i32, last_frame_number: i32) -> Result<Self, ScreenError> {
        Self::new(width, height, 16, display_colors, last_frame_number, ModernScreenLimits::default(), None)
    }

    pub fn tile_size(&self) -> usize {
        self.tile_size
    }

    pub fn last_frame_number(&self) -> i32 {
        self.last_frame_number
    }

    pub fn cache_entry_count(&self) -> usize {
        self.cache.len()
    }

    pub fn cache_byte_count(&self) -> usize {
        self.cache_bytes
    }

    /// Top-down BGRA pixels of the current main surface.
    pub fn pixels(&self) -> &[u8] {
        &self.main_pixels
    }

    pub fn bitmap(&self) -> ModernScreenBitmap {
        ModernScreenBitmap { width: self.width, height: self.height, bgra: Arc::new(self.main_pixels.clone()) }
    }

    /// Bounding box (x, y, width, height) of pixels changed since the last call.
    pub fn take_damage(&mut self) -> Option<(usize, usize, usize, usize)> {
        self.damage.take()
    }

    /// Command 48 carries the new tile size in the first byte of part 1.
    pub fn set_tile_size(&mut self, parts: &[Vec<u8>]) -> Result<(), ScreenError> {
        self.limits.check_parts(parts)?;
        let command = parts.first().and_then(|p| p.first()).ok_or(ScreenError::MalformedPart(0))?;
        if *command != 48 {
            return Err(ScreenError::UnsupportedCommand(*command));
        }
        let part = parts.get(1).ok_or(ScreenError::MissingPart(1))?;
        match part.first() {
            Some(&value) if value > 0 => {
                self.tile_size = value as usize;
                Ok(())
            }
            _ => Err(ScreenError::MalformedPart(1)),
        }
    }

    /// The original clears its ordered cache on color-change/clear missions.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.cache_order.clear();
        self.cache_bytes = 0;
    }

    /// Atomic local application: every failure is detected before the first
    /// mutation, so malformed/unsupported frames leave the bitmap, temporary
    /// pixels, sequence, and cache unchanged.
    pub fn apply(&mut self, message: &ModernScreenFrameMessage) -> Result<(), ScreenError> {
        let expected = i64::from(self.last_frame_number) + 1;
        if i64::from(message.frame_number) != expected {
            return Err(ScreenError::SequenceMismatch { expected, received: message.frame_number });
        }
        let raw_pixels = self.decode_main_pixels(message)?;
        for reference in &message.cache_references {
            if !self.cache.contains_key(&reference.hash) {
                return Err(ScreenError::CacheMiss(reference.hash));
            }
        }
        let auxiliary = if message.auxiliary_rectangles.is_empty() {
            None
        } else {
            let atlas = self.decode_auxiliary_jpeg(&message.auxiliary_jpeg)?;
            let copies = self.plan_auxiliary(&atlas, &message.auxiliary_rectangles)?;
            Some((atlas, copies))
        };
        {
            let mut running = self.cache_bytes;
            let mut added = HashSet::new();
            for addition in &message.cache_additions {
                // Ordinal 15's out-of-bounds extraction is not established. Do not
                // emulate unsafe reads or manufacture missing cache pixels.
                if !self.is_inside(&addition.region) {
                    return Err(ScreenError::InvalidGeometry);
                }
                if !self.cache.contains_key(&addition.hash) && added.insert(addition.hash) {
                    let size = (addition.region.width * addition.region.height * 2) as usize;
                    if size > self.limits.maximum_cache_bytes - running {
                        return Err(ScreenError::CacheBudgetExceeded);
                    }
                    running += size;
                }
            }
        }

        // 0x06000c65 IL_01c9..032c: capture main→converter, then converter→main.
        if let Some(scroll) = &message.scroll {
            self.copy_main_to_temporary(&scroll.capture, scroll.capture.x, scroll.capture.y - scroll.vertical_delta);
            for region in &scroll.copy_back {
                self.copy_temporary_to_main(region.x, region.y, region);
            }
        }

        // IL_032c..0443: cache references write the converter, not the main DIB.
        for reference in &message.cache_references {
            let item = &self.cache[&reference.hash];
            self.temporary
                .copy_rectangle(&item.pixels, item.width, item.height, reference.x, reference.y)
                .map_err(|_| ScreenError::InvalidGeometry)?;
            if let Some(position) = self.cache_order.iter().position(|&h| h == reference.hash) {
                self.cache_order.remove(position);
            }
            self.cache_order.push_back(reference.hash);
        }

        // IL_0443..0512: expose selected converter rectangles to the main DIB.
        for region in &message.copy_from_temporary {
            self.copy_temporary_to_main(region.x, region.y, region);
        }

        // IL_0512..0886: JPEG atlas tiles write full color directly to main.
        if let Some((atlas, copies)) = auxiliary {
            for copy in copies {
                for row in 0..copy.height {
                    let source = ((copy.source_y + row) * atlas.width + copy.source_x) * 4;
                    let destination = ((copy.destination_y + row) * self.width + copy.destination_x) * 4;
                    self.main_pixels[destination..destination + copy.width * 4]
                        .copy_from_slice(&atlas.pixels[source..source + copy.width * 4]);
                }
                self.mark(copy.destination_x as i64, copy.destination_y as i64, copy.width as i64, copy.height as i64);
            }
        }

        // IL_0995..0d8a: each packed raw rectangle stages at converter (0,0).
        let mut offset = 0;
        for rectangle in &message.main_rectangles {
            let count = (rectangle.width * rectangle.height * 2) as usize;
            self.temporary
                .copy_rectangle(&raw_pixels[offset..offset + count], rectangle.width, rectangle.height, 0, 0)
                .map_err(|_| ScreenError::InvalidGeometry)?;
            self.copy_temporary_to_main(0, 0, rectangle);
            offset += count;
        }

        // IL_0d8a..0f3d: cache captures occur after every drawing operation.
        for addition in &message.cache_additions {
            let region = addition.region;
            self.copy_main_to_temporary(&region, region.x, region.y);
            let captured = self.extract_temporary(&region);
            if !self.cache.contains_key(&addition.hash) {
                self.cache_bytes += captured.len();
                self.cache.insert(addition.hash, CachedRectangle { width: region.width, height: region.height, pixels: captured });
                self.cache_order.push_back(addition.hash);
            }
        }
        // Preserve native trim timing and insertion/reference order.
        while self.cache_order.len() > 1000 {
            if let Some(hash) = self.cache_order.pop_front() {
                if let Some(removed) = self.cache.remove(&hash) {
                    self.cache_bytes -= removed.pixels.len();
                }
            }
        }
        self.last_frame_number = message.frame_number;
        Ok(())
    }

    fn decode_main_pixels(&mut self, message: &ModernScreenFrameMessage) -> Result<Vec<u8>, ScreenError> {
        let mut expected = 0usize;
        for region in &message.main_rectangles {
            self.limits.check_geometry(region.width, region.height)?;
            let count = product(product(region.width as usize, region.height as usize)?, 2)?;
            if count > self.limits.maximum_decompressed_bytes - expected {
                return Err(ScreenError::SizeLimitExceeded);
            }
            expected += count;
        }
        self.decompressor.reset()?; // UVC #25 before #16, for every drawing mission.
        let decoded = self.decompressor.append(&message.compressed_pixels)?;
        self.decompressor.finish()?;
        if decoded.len() != expected {
            return Err(ScreenError::DecompressedLengthMismatch { expected, received: decoded.len() });
        }
        Ok(decoded)
    }

    /// Clips source and destination together, preserving their coordinate offset.
    fn clipped_copy(&self, source_x: i64, source_y: i64, destination_x: i64, destination_y: i64, width: i64, height: i64)
        -> Option<(i64, i64, i64, i64, i64, i64)> {
        let (w, h) = (self.width as i64, self.height as i64);
        let left = 0.max(-source_x).max(-destination_x);
        let top = 0.max(-source_y).max(-destination_y);
        let right = width.min(w - source_x).min(w - destination_x);
        let bottom = height.min(h - source_y).min(h - destination_y);
        if left >= right || top >= bottom {
            return None;
        }
        Some((source_x + left, source_y + top, destination_x + left, destination_y + top, right - left, bottom - top))
    }

    fn copy_main_to_temporary(&mut self, source: &ScreenRegion, destination_x: i64, destination_y: i64) {
        let Some((sx, sy, dx, dy, w, h)) = self.clipped_copy(source.x, source.y, destination_x, destination_y, source.width, source.height) else {
            return;
        };
        let (sx, sy, dx, dy, w, h) = (sx as usize, sy as usize, dx as usize, dy as usize, w as usize, h as usize);
        let stride = self.temporary.row_stride;
        for row in 0..h {
            let source_offset = ((sy + row) * self.width + sx) * 4;
            let destination_offset = (self.height - 1 - (dy + row)) * stride + dx * 2;
            pack_row(&self.main_pixels[source_offset..], &mut self.temporary.pixels[destination_offset..], w);
        }
    }

    fn copy_temporary_to_main(&mut self, source_x: i64, source_y: i64, destination: &ScreenRegion) {
        let Some((sx, sy, dx, dy, w, h)) = self.clipped_copy(source_x, source_y, destination.x, destination.y, destination.width, destination.height) else {
            return;
        };
        self.mark(dx, dy, w, h);
        let (sx, sy, dx, dy, w, h) = (sx as usize, sy as usize, dx as usize, dy as usize, w as usize, h as usize);
        let stride = self.temporary.row_stride;
        for row in 0..h {
            let source_offset = (self.height - 1 - (sy + row)) * stride + sx * 2;
            let destination_offset = ((dy + row) * self.width + dx) * 4;
            expand_row(&self.temporary.pixels[source_offset..], &mut self.main_pixels[destination_offset..], w);
        }
    }

    fn extract_temporary(&self, region: &ScreenRegion) -> Vec<u8> {
        let row_bytes = region.width as usize * 2;
        let mut result = vec![0u8; row_bytes * region.height as usize];
        for row in 0..region.height as usize {
            let start = (self.height - 1 - region.y as usize - row) * self.temporary.row_stride + region.x as usize * 2;
            result[row * row_bytes..(row + 1) * row_bytes].copy_from_slice(&self.temporary.pixels[start..start + row_bytes]);
        }
        result
    }

    fn decode_auxiliary_jpeg(&mut self, jpeg: &[u8]) -> Result<Atlas, ScreenError> {
        if jpeg.len() > self.limits.maximum_payload_bytes || jpeg.len() < 4 || jpeg[0] != 0xff || jpeg[1] != 0xd8 {
            return Err(ScreenError::InvalidJpeg);
        }
        if self.jpeg.is_none() {
            self.jpeg = JpegDecoder::new();
        }
        let decoder = self.jpeg.as_mut().ok_or(ScreenError::UnsupportedJpeg)?;
        let (width, height) = decoder.header(jpeg).ok_or(ScreenError::InvalidJpeg)?;
        self.limits.check_atlas_geometry(width as i64, height as i64)?;
        let pixels = decoder.decode_bgra_bottom_up(jpeg, width, height).ok_or(ScreenError::InvalidJpeg)?;
        Ok(Atlas { width, height, pixels })
    }

    /// Windows 6.6 sizes a multi-column atlas as ⌊rows / columns⌋ rounded down
    /// to whole tiles, so its last one or two tiles can fall outside the
    /// JPEG; the native client reads past them unchecked. Those few tiles are
    /// skipped here (keeping their previous pixels); more indicate a layout
    /// mismatch and reject the frame.
    fn plan_auxiliary(&self, atlas: &Atlas, rectangles: &[ScreenRegion]) -> Result<Vec<TileCopy>, ScreenError> {
        if !rectangles.iter().all(|r| self.is_inside(r)) {
            return Err(ScreenError::InvalidGeometry);
        }
        let plan = plan_tiles(self.tile_size, atlas, rectangles, self.limits.maximum_auxiliary_tiles)?;
        let tolerated = atlas.width / self.tile_size + 2;
        if plan.skipped == 0 {
            return Ok(plan.copies);
        }
        if plan.skipped <= tolerated && plan.skipped < plan.copies.len() {
            log::debug!("screen atlas {}x{}: skipped {} tile(s) outside the image", atlas.width, atlas.height, plan.skipped);
            return Ok(plan.copies);
        }
        let tiles = plan.copies.len() + plan.skipped;
        let rows: usize = rectangles.iter().map(|r| (r.width as usize).div_ceil(self.tile_size) * r.height as usize).sum();
        log::error!("Atlas bounds: atlas={}x{} tile={} skipped={} rectangles={:?}",
                    atlas.width, atlas.height, self.tile_size, plan.skipped, rectangles);
        Err(ScreenError::AuxiliaryAtlasOutOfBounds {
            atlas_width: atlas.width,
            atlas_height: atlas.height,
            tile_size: self.tile_size,
            tiles,
            rows,
            screen_width: self.width,
            screen_height: self.height,
        })
    }

    fn is_inside(&self, region: &ScreenRegion) -> bool {
        let (w, h) = (self.width as i64, self.height as i64);
        region.x >= 0 && region.y >= 0 && region.width > 0 && region.height > 0
            && region.width <= w && region.height <= h
            && region.x <= w - region.width && region.y <= h - region.height
    }

    fn mark(&mut self, x: i64, y: i64, width: i64, height: i64) {
        if width <= 0 || height <= 0 {
            return;
        }
        let (x, y, w, h) = (x as usize, y as usize, width as usize, height as usize);
        self.damage = Some(match self.damage {
            None => (x, y, w, h),
            Some((ox, oy, ow, oh)) => {
                let left = ox.min(x);
                let top = oy.min(y);
                let right = (ox + ow).max(x + w);
                let bottom = (oy + oh).max(y + h);
                (left, top, right - left, bottom - top)
            }
        });
    }
}

struct TilePlan {
    copies: Vec<TileCopy>,
    /// Tiles whose atlas position lies outside the decoded image.
    skipped: usize,
}

/// Packs tiles in the recovered order: columns of `tile` pixels, each filled
/// bottom-up in memory until the next tile would exceed the atlas height.
fn plan_tiles(tile: usize, atlas: &Atlas, rectangles: &[ScreenRegion], maximum_tiles: usize) -> Result<TilePlan, ScreenError> {
    let mut copies = Vec::new();
    let mut skipped = 0usize;
    let mut source_column = 0usize;
    let mut source_y = 0usize;
    for rectangle in rectangles {
        let (rw, rh) = (rectangle.width as usize, rectangle.height as usize);
        // Recovered loop order: destination x outside, destination y inside.
        for dx in (0..rw).step_by(tile) {
            for dy in (0..rh).step_by(tile) {
                if copies.len() + skipped >= maximum_tiles {
                    return Err(ScreenError::SizeLimitExceeded);
                }
                let tile_width = tile.min(rw - dx);
                let tile_height = tile.min(rh - dy);
                if source_y + tile_height > atlas.height {
                    source_y = 0;
                    source_column += 1;
                }
                let source_x = product(source_column, tile)?;
                if tile_width > atlas.width
                    || tile_height > atlas.height
                    || source_x > atlas.width - tile_width
                    || source_y > atlas.height - tile_height
                {
                    skipped += 1;
                } else {
                    copies.push(TileCopy {
                        source_x,
                        source_y,
                        destination_x: rectangle.x as usize + dx,
                        destination_y: rectangle.y as usize + dy,
                        width: tile_width,
                        height: tile_height,
                    });
                }
                source_y += tile_height;
            }
        }
    }
    Ok(TilePlan { copies, skipped })
}

fn product(a: usize, b: usize) -> Result<usize, ScreenError> {
    a.checked_mul(b).filter(|v| *v <= isize::MAX as usize).ok_or(ScreenError::SizeLimitExceeded)
}
