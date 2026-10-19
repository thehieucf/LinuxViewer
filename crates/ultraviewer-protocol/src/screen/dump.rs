//! Opt-in capture of screen updates that carry a JPEG atlas, for protocol
//! analysis: `LINUXVIEWER_DUMP_SCREEN=<directory>`. The files contain remote
//! screen pixels and are written only when that variable is set.

use std::fmt::Write as _;
use std::path::PathBuf;

use super::message::{ModernScreenFrameMessage, ScreenRegion};

/// The first few atlas frames, then only atlases wider than one tile column.
const MAXIMUM_SMALL_FRAMES: usize = 10;
const MAXIMUM_WIDE_FRAMES: usize = 12;

pub struct ScreenDump {
    directory: PathBuf,
    written: usize,
    small: usize,
    wide: usize,
}

impl ScreenDump {
    pub fn from_environment() -> Option<Self> {
        let directory = PathBuf::from(std::env::var_os("LINUXVIEWER_DUMP_SCREEN")?);
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::warn!("screen dump disabled: {}: {error}", directory.display());
            return None;
        }
        log::warn!("dumping screen atlases into {}", directory.display());
        Some(Self { directory, written: 0, small: 0, wide: 0 })
    }

    pub fn record(&mut self, width: usize, height: usize, tile_size: usize, message: &ModernScreenFrameMessage) {
        if message.auxiliary_rectangles.is_empty() {
            return;
        }
        let atlas = jpeg_size(&message.auxiliary_jpeg);
        if atlas.is_some_and(|(width, _)| width > tile_size) {
            if self.wide >= MAXIMUM_WIDE_FRAMES {
                return;
            }
            self.wide += 1;
        } else {
            if self.small >= MAXIMUM_SMALL_FRAMES {
                return;
            }
            self.small += 1;
        }
        self.written += 1;
        let name = format!("frame-{:03}", self.written);
        let list = |regions: &[ScreenRegion]| {
            let mut text = String::from("[");
            for (index, r) in regions.iter().enumerate() {
                let _ = write!(text, "{}[{},{},{},{}]", if index == 0 { "" } else { "," }, r.x, r.y, r.width, r.height);
            }
            text.push(']');
            text
        };
        let json = format!(
            "{{\"frame\":{},\"screen\":[{width},{height}],\"tile\":{tile_size},\"atlas\":{:?},\"jpegBytes\":{},\
             \"auxiliary\":{},\"main\":{},\"copyFromTemporary\":{},\"scroll\":{},\
             \"cacheReferences\":{},\"cacheAdditions\":{}}}\n",
            message.frame_number,
            atlas.map(|(w, h)| [w, h]).unwrap_or_default(),
            message.auxiliary_jpeg.len(),
            list(&message.auxiliary_rectangles),
            list(&message.main_rectangles),
            list(&message.copy_from_temporary),
            message.scroll.is_some(),
            message.cache_references.len(),
            message.cache_additions.len(),
        );
        let jpeg = self.directory.join(format!("{name}.jpg"));
        let metadata = self.directory.join(format!("{name}.json"));
        if let Err(error) = std::fs::write(&jpeg, &message.auxiliary_jpeg).and_then(|_| std::fs::write(&metadata, json)) {
            log::warn!("screen dump failed: {error}");
            (self.small, self.wide) = (MAXIMUM_SMALL_FRAMES, MAXIMUM_WIDE_FRAMES);
        } else {
            log::info!("dumped {name} (atlas {atlas:?})");
        }
    }
}

/// Width and height from the first JPEG start-of-frame marker.
fn jpeg_size(jpeg: &[u8]) -> Option<(usize, usize)> {
    let mut offset = 2;
    while offset + 9 <= jpeg.len() {
        if jpeg[offset] != 0xff {
            return None;
        }
        let marker = jpeg[offset + 1];
        let length = usize::from(u16::from_be_bytes([jpeg[offset + 2], jpeg[offset + 3]]));
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            let height = usize::from(u16::from_be_bytes([jpeg[offset + 5], jpeg[offset + 6]]));
            let width = usize::from(u16::from_be_bytes([jpeg[offset + 7], jpeg[offset + 8]]));
            return Some((width, height));
        }
        offset += 2 + length;
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_jpeg_dimensions_after_other_segments() {
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0, 4, 1, 2, 0xff, 0xc0, 0, 11, 8, 0xbf, 0x60, 0, 0x40, 3, 0, 0, 0];
        assert_eq!(super::jpeg_size(&jpeg), Some((64, 48992)));
    }
}
