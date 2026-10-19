//! Native Zstandard reader/writer for the frame family observed in uvc.dll.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};

#[repr(C)]
struct InBuffer {
    src: *const c_void,
    size: usize,
    pos: usize,
}

#[repr(C)]
struct OutBuffer {
    dst: *mut c_void,
    size: usize,
    pos: usize,
}

enum DCtx {}
enum CCtx {}

const ZSTD_D_WINDOW_LOG_MAX: c_int = 100;
const ZSTD_RESET_SESSION_ONLY: c_int = 1;
const ZSTD_ERROR_DST_SIZE_TOO_SMALL: c_int = 70;

extern "C" {
    fn ZSTD_createDStream() -> *mut DCtx;
    fn ZSTD_freeDStream(zds: *mut DCtx) -> usize;
    fn ZSTD_initDStream(zds: *mut DCtx) -> usize;
    fn ZSTD_DCtx_loadDictionary(dctx: *mut DCtx, dict: *const c_void, dict_size: usize) -> usize;
    fn ZSTD_DCtx_setParameter(dctx: *mut DCtx, param: c_int, value: c_int) -> usize;
    fn ZSTD_DCtx_reset(dctx: *mut DCtx, reset: c_int) -> usize;
    fn ZSTD_decompressStream(zds: *mut DCtx, output: *mut OutBuffer, input: *mut InBuffer) -> usize;
    fn ZSTD_isError(code: usize) -> c_uint;
    fn ZSTD_getErrorName(code: usize) -> *const c_char;
    fn ZSTD_getErrorCode(code: usize) -> c_int;
    fn ZSTD_compressBound(src_size: usize) -> usize;
    fn ZSTD_createCCtx() -> *mut CCtx;
    fn ZSTD_freeCCtx(cctx: *mut CCtx) -> usize;
    fn ZSTD_compressCCtx(cctx: *mut CCtx, dst: *mut c_void, dst_capacity: usize, src: *const c_void,
                         src_size: usize, level: c_int) -> usize;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ZstdDecodeError {
    #[error("Zstandard allocation failed")]
    AllocationFailed,
    #[error("invalid Zstandard data: {0}")]
    InvalidData(String),
    #[error("Zstandard output limit exceeded")]
    OutputLimitExceeded,
    #[error("incomplete Zstandard frame")]
    IncompleteFrame,
    #[error("Zstandard decoder reset required")]
    ResetRequired,
}

fn check(code: usize) -> Result<(), ZstdDecodeError> {
    // SAFETY: pure functions over an error code.
    unsafe {
        if ZSTD_isError(code) != 0 {
            let name = CStr::from_ptr(ZSTD_getErrorName(code)).to_string_lossy().into_owned();
            return Err(ZstdDecodeError::InvalidData(name));
        }
    }
    Ok(())
}

/// Call `reset` before each independent drawing mission (ordinal 25 does this
/// in the inspected client). Keep the same object across fragments within a
/// frame. A failed decoder requires an explicit reset.
pub struct ZstdScreenDecoder {
    context: *mut DCtx,
    maximum_output_bytes: usize,
    frame_output_count: usize,
    last_hint: usize,
    started: bool,
    failed: bool,
    scratch: Vec<u8>,
}

// SAFETY: the context is exclusively owned and only used through &mut self.
unsafe impl Send for ZstdScreenDecoder {}

impl ZstdScreenDecoder {
    pub fn new(maximum_output_bytes: usize, dictionary: Option<&[u8]>) -> Result<Self, ZstdDecodeError> {
        assert!(maximum_output_bytes > 0);
        // SAFETY: plain allocation; checked for null below.
        let context = unsafe { ZSTD_createDStream() };
        if context.is_null() {
            return Err(ZstdDecodeError::AllocationFailed);
        }
        let decoder = Self {
            context,
            maximum_output_bytes,
            frame_output_count: 0,
            last_hint: 0,
            started: false,
            failed: false,
            scratch: vec![0; (128 * 1024).min(maximum_output_bytes)],
        };
        // SAFETY: valid context; dictionary bytes are copied by zstd.
        unsafe {
            check(ZSTD_initDStream(context))?;
            if let Some(dictionary) = dictionary {
                check(ZSTD_DCtx_loadDictionary(context, dictionary.as_ptr().cast(), dictionary.len()))?;
            }
            // The inspected UVC header rejects a window above 0x8000000 bytes.
            check(ZSTD_DCtx_setParameter(context, ZSTD_D_WINDOW_LOG_MAX, 27))?;
        }
        Ok(decoder)
    }

    pub fn has_incomplete_frame(&self) -> bool {
        self.started && self.last_hint != 0
    }

    pub fn reset(&mut self) -> Result<(), ZstdDecodeError> {
        // UVC ordinal25 preserves the DDict installed by ordinal22. initDStream
        // clears dictionaries, so reset only the session here.
        // SAFETY: valid context.
        check(unsafe { ZSTD_DCtx_reset(self.context, ZSTD_RESET_SESSION_ONLY) })?;
        self.frame_output_count = 0;
        self.last_hint = 0;
        self.started = false;
        self.failed = false;
        Ok(())
    }

    /// Bounded streaming output. Both one frame's expansion and one append's
    /// returned output are limited.
    pub fn append(&mut self, compressed: &[u8]) -> Result<Vec<u8>, ZstdDecodeError> {
        if self.failed {
            return Err(ZstdDecodeError::ResetRequired);
        }
        if compressed.is_empty() {
            return Ok(Vec::new());
        }
        let result = self.append_inner(compressed);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn append_inner(&mut self, compressed: &[u8]) -> Result<Vec<u8>, ZstdDecodeError> {
        let mut input = InBuffer { src: compressed.as_ptr().cast(), size: compressed.len(), pos: 0 };
        let mut result = Vec::new();
        loop {
            let prior_position = input.pos;
            let mut output = OutBuffer { dst: self.scratch.as_mut_ptr().cast(), size: self.scratch.len(), pos: 0 };
            // SAFETY: buffers are valid for the given sizes during the call.
            let hint = unsafe { ZSTD_decompressStream(self.context, &mut output, &mut input) };
            check(hint)?;
            let produced = output.pos;
            self.started = true;
            self.last_hint = hint;
            if produced > self.maximum_output_bytes - self.frame_output_count
                || produced > self.maximum_output_bytes - result.len()
            {
                return Err(ZstdDecodeError::OutputLimitExceeded);
            }
            result.extend_from_slice(&self.scratch[..produced]);
            self.frame_output_count += produced;
            if hint == 0 {
                self.frame_output_count = 0;
            }
            let needs_drain = produced == self.scratch.len() && hint != 0;
            if prior_position == input.pos && produced == 0 {
                break;
            }
            if !(input.pos < input.size || needs_drain) {
                break;
            }
        }
        Ok(result)
    }

    /// Call at the end of an independently framed compressed mission.
    pub fn finish(&self) -> Result<(), ZstdDecodeError> {
        if self.failed {
            return Err(ZstdDecodeError::ResetRequired);
        }
        if self.has_incomplete_frame() {
            return Err(ZstdDecodeError::IncompleteFrame);
        }
        Ok(())
    }

    /// One-shot decode with a fresh, dictionary-free context (UVC ordinal50).
    pub fn decode_once(compressed: &[u8], maximum_output_bytes: usize) -> Result<Vec<u8>, ZstdDecodeError> {
        let mut decoder = Self::new(maximum_output_bytes, None)?;
        let decoded = decoder.append(compressed)?;
        decoder.finish()?;
        Ok(decoded)
    }
}

impl Drop for ZstdScreenDecoder {
    fn drop(&mut self) {
        // SAFETY: context was created by ZSTD_createDStream and is freed once.
        unsafe {
            ZSTD_freeDStream(self.context);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ZstdEncodeError {
    #[error("invalid output limit")]
    InvalidOutputLimit,
    #[error("input limit exceeded")]
    InputLimitExceeded,
    #[error("output limit exceeded")]
    OutputLimitExceeded,
    #[error("allocation failed")]
    AllocationFailed,
    #[error("compression failed: {0}")]
    CompressionFailed(String),
}

pub const MAXIMUM_ENCODER_INPUT_BYTES: usize = 16 * 1024 * 1024;
pub const MAXIMUM_ENCODER_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// Independent standard Zstd frames, compressed at level 1 without a dictionary.
pub fn encode(bytes: &[u8], maximum_output_bytes: usize) -> Result<Vec<u8>, ZstdEncodeError> {
    if !(1..=MAXIMUM_ENCODER_OUTPUT_BYTES).contains(&maximum_output_bytes) {
        return Err(ZstdEncodeError::InvalidOutputLimit);
    }
    if bytes.len() > MAXIMUM_ENCODER_INPUT_BYTES {
        return Err(ZstdEncodeError::InputLimitExceeded);
    }
    let encode_check = |code: usize| -> Result<(), ZstdEncodeError> {
        // SAFETY: pure functions over an error code.
        unsafe {
            if ZSTD_isError(code) == 0 {
                return Ok(());
            }
            if ZSTD_getErrorCode(code) == ZSTD_ERROR_DST_SIZE_TOO_SMALL {
                return Err(ZstdEncodeError::OutputLimitExceeded);
            }
            Err(ZstdEncodeError::CompressionFailed(CStr::from_ptr(ZSTD_getErrorName(code)).to_string_lossy().into_owned()))
        }
    };
    // SAFETY: pure function.
    let bound = unsafe { ZSTD_compressBound(bytes.len()) };
    encode_check(bound)?;
    // SAFETY: allocation checked below and freed on every path.
    let context = unsafe { ZSTD_createCCtx() };
    if context.is_null() {
        return Err(ZstdEncodeError::AllocationFailed);
    }
    let mut result = vec![0u8; bound.min(maximum_output_bytes)];
    // SAFETY: both buffers are valid for their lengths.
    let count = unsafe {
        let count = ZSTD_compressCCtx(context, result.as_mut_ptr().cast(), result.len(), bytes.as_ptr().cast(), bytes.len(), 1);
        ZSTD_freeCCtx(context);
        count
    };
    encode_check(count)?;
    result.truncate(count);
    Ok(result)
}
