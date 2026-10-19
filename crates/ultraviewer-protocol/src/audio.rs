//! Computer-audio messages and the stateful 64 kbps G.722 decoder.

enum G722State {}

extern "C" {
    fn MacViewerG722Create() -> *mut G722State;
    fn MacViewerG722Destroy(decoder: *mut G722State);
    fn MacViewerG722Decode(decoder: *mut G722State, encoded: *const u8, encoded_length: usize, pcm: *mut i16,
                           pcm_capacity: usize) -> usize;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum G722Error {
    #[error("G.722 allocation failed")]
    AllocationFailed,
    #[error("G.722 packet too large")]
    PacketTooLarge,
    #[error("G.722 decoder failure")]
    DecoderFailure,
}

/// Stateful 64kbps G.722 decoder used by computer-audio command43. One encoded
/// byte produces two signed16 samples. The caller treats the byte stream as
/// 48kHz stereo, the native playback format. Keep one decoder through pause,
/// resume and silence; create a new one only for a new authenticated session.
pub struct G722Decoder {
    state: *mut G722State,
}

// SAFETY: the C state is exclusively owned and used through &mut self.
unsafe impl Send for G722Decoder {}

impl G722Decoder {
    pub const MAXIMUM_ENCODED_PACKET_BYTES: usize = 64 * 1024;
    pub const MAXIMUM_PCM_PACKET_BYTES: usize = Self::MAXIMUM_ENCODED_PACKET_BYTES * 4;

    pub fn new() -> Result<Self, G722Error> {
        // SAFETY: allocation checked for null.
        let state = unsafe { MacViewerG722Create() };
        if state.is_null() {
            return Err(G722Error::AllocationFailed);
        }
        Ok(Self { state })
    }

    /// Returns signed16 little-endian interleaved PCM bytes.
    pub fn decode(&mut self, encoded: &[u8]) -> Result<Vec<u8>, G722Error> {
        if encoded.len() > Self::MAXIMUM_ENCODED_PACKET_BYTES {
            return Err(G722Error::PacketTooLarge);
        }
        if encoded.is_empty() {
            return Ok(Vec::new());
        }
        let mut samples = vec![0i16; encoded.len() * 2];
        // SAFETY: input/output buffers are valid for the given lengths.
        let produced = unsafe {
            MacViewerG722Decode(self.state, encoded.as_ptr(), encoded.len(), samples.as_mut_ptr(), samples.len())
        };
        if produced != samples.len() {
            return Err(G722Error::DecoderFailure);
        }
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        Ok(bytes)
    }
}

impl Drop for G722Decoder {
    fn drop(&mut self) {
        // SAFETY: created by MacViewerG722Create, destroyed once.
        unsafe { MacViewerG722Destroy(self.state) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AudioMessageError {
    #[error("malformed audio message")]
    MalformedMessage,
    #[error("unsupported audio command")]
    UnsupportedCommand,
    #[error("audio packet too large")]
    PacketTooLarge,
    #[error("invalid silence length")]
    InvalidSilenceLength,
}

/// Host→viewer multipart43 contains raw G.722 bytes; multipart42 supplies an
/// Int32LE count of zero PCM bytes. Silence does not advance G.722 state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputerAudioMessage {
    Encoded(Vec<u8>),
    Silence { pcm_byte_count: usize },
}

impl ComputerAudioMessage {
    pub fn decode(parts: &[Vec<u8>]) -> Result<Self, AudioMessageError> {
        if parts.len() < 2 {
            return Err(AudioMessageError::MalformedMessage);
        }
        let command = *parts[0].first().ok_or(AudioMessageError::MalformedMessage)?;
        let mut total = 0usize;
        for part in parts {
            if part.len() > G722Decoder::MAXIMUM_PCM_PACKET_BYTES - total {
                return Err(AudioMessageError::PacketTooLarge);
            }
            total += part.len();
        }
        match command {
            43 => {
                if parts[1].len() > G722Decoder::MAXIMUM_ENCODED_PACKET_BYTES {
                    return Err(AudioMessageError::PacketTooLarge);
                }
                Ok(Self::Encoded(parts[1].clone()))
            }
            42 => {
                if parts[1].len() < 4 {
                    return Err(AudioMessageError::MalformedMessage);
                }
                let count = crate::util::read_i32(&parts[1], 0);
                if count < 0 || count as usize > G722Decoder::MAXIMUM_PCM_PACKET_BYTES {
                    return Err(AudioMessageError::InvalidSilenceLength);
                }
                Ok(Self::Silence { pcm_byte_count: count as usize })
            }
            _ => Err(AudioMessageError::UnsupportedCommand),
        }
    }
}
