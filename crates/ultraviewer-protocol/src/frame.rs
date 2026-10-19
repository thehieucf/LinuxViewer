//! Reconstructed from RemoteControl.P2P_TCPSocket, method 0x06000302.
//! This envelope alone does not implement the UltraViewer session protocol.

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("invalid frame length")]
    InvalidLength,
    #[error("frame too large")]
    FrameTooLarge,
}

pub const DEFAULT_MAXIMUM_PAYLOAD: usize = 16 * 1024 * 1024;

/// UInt32 little-endian length prefix followed by the payload.
pub fn encode(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > i32::MAX as usize {
        return Err(FrameError::FrameTooLarge);
    }
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Handles partial headers, partial bodies, and coalesced frames. The caller
/// chooses a local memory budget; it is not a recovered wire limit.
#[derive(Debug)]
pub struct FrameDecoder {
    maximum_payload_length: usize,
    buffer: Vec<u8>,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new(DEFAULT_MAXIMUM_PAYLOAD)
    }
}

impl FrameDecoder {
    pub fn new(maximum_payload_length: usize) -> Self {
        assert!(maximum_payload_length <= i32::MAX as usize);
        Self { maximum_payload_length, buffer: Vec::new() }
    }

    pub fn append(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        let mut frames = Vec::new();
        let mut cursor = 0;
        while cursor < bytes.len() {
            if self.buffer.len() < 4 {
                let count = (4 - self.buffer.len()).min(bytes.len() - cursor);
                self.buffer.extend_from_slice(&bytes[cursor..cursor + count]);
                cursor += count;
                if self.buffer.len() < 4 {
                    break;
                }
            }
            let length = u32::from_le_bytes([self.buffer[0], self.buffer[1], self.buffer[2], self.buffer[3]]);
            if length > i32::MAX as u32 {
                // A failed stream must be closed by its owner. Do not retain hostile bytes.
                self.reset();
                return Err(FrameError::InvalidLength);
            }
            if length as usize > self.maximum_payload_length {
                self.reset();
                return Err(FrameError::FrameTooLarge);
            }
            let total = 4 + length as usize;
            if self.buffer.len() == 4 && bytes.len() - cursor >= total - 4 {
                // Fast path: the complete body is available in this read.
                frames.push(bytes[cursor..cursor + total - 4].to_vec());
                cursor += total - 4;
                self.buffer.clear();
                continue;
            }
            let count = (total - self.buffer.len()).min(bytes.len() - cursor);
            if count > 0 {
                if self.buffer.capacity() < total {
                    self.buffer.reserve(total - self.buffer.len());
                }
                self.buffer.extend_from_slice(&bytes[cursor..cursor + count]);
                cursor += count;
            }
            if self.buffer.len() < total {
                break;
            }
            frames.push(self.buffer[4..].to_vec());
            self.buffer.clear();
        }
        Ok(frames)
    }

    pub fn has_incomplete_frame(&self) -> bool {
        !self.buffer.is_empty()
    }

    pub fn reset(&mut self) {
        self.buffer = Vec::new();
    }
}
