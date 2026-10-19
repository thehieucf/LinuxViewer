//! Minimal TurboJPEG binding. The Windows client decodes screen atlases with
//! TurboJPEG flags 1026 (bottom-up, no realloc) into 32-bit pixels.

use std::ffi::{c_int, c_uchar, c_ulong, c_void};

type Handle = *mut c_void;

const TJPF_BGRA: c_int = 8;
const TJFLAG_BOTTOMUP: c_int = 2;
const TJFLAG_NOREALLOC: c_int = 1024;

extern "C" {
    fn tjInitDecompress() -> Handle;
    fn tjDestroy(handle: Handle) -> c_int;
    fn tjDecompressHeader3(handle: Handle, jpeg: *const c_uchar, size: c_ulong, width: *mut c_int,
                           height: *mut c_int, subsamp: *mut c_int, colorspace: *mut c_int) -> c_int;
    fn tjDecompress2(handle: Handle, jpeg: *const c_uchar, size: c_ulong, dst: *mut c_uchar, width: c_int,
                     pitch: c_int, height: c_int, pixel_format: c_int, flags: c_int) -> c_int;
}

pub(crate) struct JpegDecoder {
    handle: Handle,
}

// SAFETY: a TurboJPEG handle may move between threads; it is used by one owner.
unsafe impl Send for JpegDecoder {}

impl JpegDecoder {
    pub(crate) fn new() -> Option<Self> {
        // SAFETY: allocation, checked for null.
        let handle = unsafe { tjInitDecompress() };
        (!handle.is_null()).then_some(Self { handle })
    }

    /// Reads the JPEG dimensions without decoding pixels.
    pub(crate) fn header(&mut self, jpeg: &[u8]) -> Option<(usize, usize)> {
        let (mut width, mut height, mut subsamp, mut colorspace) = (0, 0, 0, 0);
        // SAFETY: buffer valid for its length; out-pointers are locals.
        let status = unsafe {
            tjDecompressHeader3(self.handle, jpeg.as_ptr(), jpeg.len() as c_ulong, &mut width, &mut height,
                                &mut subsamp, &mut colorspace)
        };
        if status != 0 || width <= 0 || height <= 0 {
            return None;
        }
        Some((width as usize, height as usize))
    }

    /// Decodes into tightly packed BGRA rows stored bottom-up, as native code does.
    pub(crate) fn decode_bgra_bottom_up(&mut self, jpeg: &[u8], width: usize, height: usize) -> Option<Vec<u8>> {
        let mut pixels = vec![0u8; width.checked_mul(height)?.checked_mul(4)?];
        // SAFETY: destination holds exactly pitch*height bytes.
        let status = unsafe {
            tjDecompress2(self.handle, jpeg.as_ptr(), jpeg.len() as c_ulong, pixels.as_mut_ptr(), width as c_int,
                          (width * 4) as c_int, height as c_int, TJPF_BGRA, TJFLAG_BOTTOMUP | TJFLAG_NOREALLOC)
        };
        // Status -1 with a warning still yields usable pixels in TurboJPEG 2.1+,
        // but the native caller treats any failure as a rejected mission.
        (status == 0).then_some(pixels)
    }
}

impl Drop for JpegDecoder {
    fn drop(&mut self) {
        // SAFETY: handle created by tjInitDecompress and destroyed once.
        unsafe {
            tjDestroy(self.handle);
        }
    }
}
