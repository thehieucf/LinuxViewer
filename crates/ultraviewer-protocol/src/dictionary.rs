//! The supplied desktop loads this trained Zstandard dictionary at startup.
//! It is data, not Windows executable code. Native Zstandard consumes its bytes.

use sha2::{Digest, Sha256};
use std::sync::OnceLock;

static DICTIONARY: &[u8] = include_bytes!("../resources/8bit.dat");

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the bundled UltraViewer screen dictionary is not the expected resource")]
pub struct UnexpectedResource;

pub fn load() -> Result<&'static [u8], UnexpectedResource> {
    static VERIFIED: OnceLock<bool> = OnceLock::new();
    let valid = *VERIFIED.get_or_init(|| {
        DICTIONARY.len() == 112_640
            && crate::util::hex_lower(&Sha256::digest(DICTIONARY))
                == "9aa45789d9ff9d329a623a685d57b67e33c777019de9ce1f273f21860e4bfe6b"
    });
    if valid {
        Ok(DICTIONARY)
    } else {
        Err(UnexpectedResource)
    }
}
