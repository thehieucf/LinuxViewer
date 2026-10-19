//! Legacy ciphers used by the UltraViewer desktop client.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes256;
use des::TdesEde3;
use md5::{Digest, Md5};
use rand::RngCore;
use sha1::Sha1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AesError {
    #[error("unsupported password encoding")]
    UnsupportedPasswordEncoding,
    #[error("invalid IV")]
    InvalidIv,
    #[error("malformed ciphertext")]
    MalformedCiphertext,
    #[error("invalid padding")]
    InvalidPadding,
}

/// Reconstruction of the supplied RemoteControl.AES implementation.
/// Only ASCII passwords are admitted; non-ASCII .NET fallback is unverified.
/// This is a compatibility primitive, not a complete authenticated channel.
#[derive(Clone)]
pub struct LegacyAes {
    cipher: Aes256,
}

impl LegacyAes {
    pub const BLOCK: usize = 16;

    pub fn new(password: &str) -> Result<Self, AesError> {
        if !password.bytes().all(|b| b < 128) {
            return Err(AesError::UnsupportedPasswordEncoding);
        }
        let key = Self::derive_key(password.as_bytes());
        Ok(Self { cipher: Aes256::new(GenericArray::from_slice(&key)) })
    }

    pub fn derive_key(ascii_password: &[u8]) -> [u8; 32] {
        let digest = Md5::digest(ascii_password);
        // Original Init copies the second digest at offset 15, overlapping byte 15.
        let mut key = [0u8; 32];
        key[0..16].copy_from_slice(&digest);
        key[15..31].copy_from_slice(&digest);
        key
    }

    /// A fresh random IV is included at the beginning of every encrypted payload.
    pub fn encrypt(&self, plaintext: &[u8]) -> Vec<u8> {
        let mut iv = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut iv);
        self.encrypt_with_iv(plaintext, &iv).expect("valid IV")
    }

    /// Deterministic entry point for independently generated test vectors.
    pub fn encrypt_with_iv(&self, plaintext: &[u8], iv: &[u8]) -> Result<Vec<u8>, AesError> {
        if iv.len() != Self::BLOCK {
            return Err(AesError::InvalidIv);
        }
        let mut output = iv.to_vec();
        output.extend(cbc_encrypt(|block| self.cipher.encrypt_block(block), iv, plaintext, 16));
        Ok(output)
    }

    pub fn decrypt(&self, payload: &[u8]) -> Result<Vec<u8>, AesError> {
        let block = Self::BLOCK;
        if payload.len() < 2 * block || (payload.len() - block) % block != 0 {
            return Err(AesError::MalformedCiphertext);
        }
        let plain = cbc_decrypt(|b| self.cipher.decrypt_block(b), &payload[..block], &payload[block..], 16);
        strip_pkcs7(plain, block).ok_or(AesError::InvalidPadding)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommandCipherError {
    #[error("malformed ciphertext")]
    MalformedCiphertext,
    #[error("invalid padding")]
    InvalidPadding,
}

/// VFunction.EncryptData/DecryptData (0x06001472/0x06001473), using an explicit
/// password. This legacy server-command cipher is distinct from P2P AES.
#[derive(Clone)]
pub struct LegacyCommandCipher {
    cipher: TdesEde3,
    iv: [u8; 8],
}

impl LegacyCommandCipher {
    /// Static command-cipher configuration observed at desktop VA 0x635a8c.
    /// This is a bundled protocol constant, not a partner/session credential.
    pub fn desktop_server_commands() -> Self {
        let password: String = [5u8, 10, 15, 20, 25].iter().map(|&b| b as char).collect();
        Self::new(&password)
    }

    pub fn new(password: &str) -> Self {
        // 0x06001485 hashes .NET Encoding.Unicode (UTF-16LE), then resizes the
        // 20-byte SHA1 digest to the 24-byte TripleDES key, zero-extending it.
        let utf16 = crate::util::utf16le(password);
        let mut key = [0u8; 24];
        key[..20].copy_from_slice(&Sha1::digest(&utf16));
        let mut iv = [0u8; 8];
        iv.copy_from_slice(&Sha1::digest([])[..8]);
        Self { cipher: TdesEde3::new(GenericArray::from_slice(&key)), iv }
    }

    /// CBC with PKCS7 padding; no IV, length or transport prefix is prepended.
    pub fn encrypt(&self, plaintext: &[u8]) -> Vec<u8> {
        cbc_encrypt(|block| self.cipher.encrypt_block(block), &self.iv, plaintext, 8)
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, CommandCipherError> {
        if ciphertext.is_empty() || ciphertext.len() % 8 != 0 {
            return Err(CommandCipherError::MalformedCiphertext);
        }
        let plain = cbc_decrypt(|b| self.cipher.decrypt_block(b), &self.iv, ciphertext, 8);
        strip_pkcs7(plain, 8).ok_or(CommandCipherError::InvalidPadding)
    }
}

fn cbc_encrypt<F, N>(mut encrypt: F, iv: &[u8], plaintext: &[u8], block: usize) -> Vec<u8>
where
    F: FnMut(&mut GenericArray<u8, N>),
    N: aes::cipher::generic_array::ArrayLength<u8>,
{
    let padding = block - plaintext.len() % block;
    let mut data = plaintext.to_vec();
    data.extend(std::iter::repeat(padding as u8).take(padding));
    let mut previous = iv.to_vec();
    for chunk in data.chunks_mut(block) {
        for (byte, prior) in chunk.iter_mut().zip(previous.iter()) {
            *byte ^= prior;
        }
        let array = GenericArray::from_mut_slice(chunk);
        encrypt(array);
        previous.copy_from_slice(chunk);
    }
    data
}

fn cbc_decrypt<F, N>(mut decrypt: F, iv: &[u8], ciphertext: &[u8], block: usize) -> Vec<u8>
where
    F: FnMut(&mut GenericArray<u8, N>),
    N: aes::cipher::generic_array::ArrayLength<u8>,
{
    let mut output = ciphertext.to_vec();
    let mut previous = iv.to_vec();
    for chunk in output.chunks_mut(block) {
        let current = chunk.to_vec();
        decrypt(GenericArray::from_mut_slice(chunk));
        for (byte, prior) in chunk.iter_mut().zip(previous.iter()) {
            *byte ^= prior;
        }
        previous = current;
    }
    output
}

/// Check every padding byte explicitly, matching .NET PKCS7 validation.
fn strip_pkcs7(mut plain: Vec<u8>, block: usize) -> Option<Vec<u8>> {
    let padding = *plain.last()? as usize;
    if padding == 0 || padding > block || padding > plain.len() {
        return None;
    }
    if !plain[plain.len() - padding..].iter().all(|&b| b as usize == padding) {
        return None;
    }
    plain.truncate(plain.len() - padding);
    Some(plain)
}
