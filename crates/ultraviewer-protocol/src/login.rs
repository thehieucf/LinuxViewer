//! Partner authentication text reconstructed from desktop Form1.Login, 0x6d6a50,
//! and the host-side `login:` request parsed by frmMain.ManipulatePacket.

use md5::{Digest, Md5};

use crate::util::{hex_lower, is_ascii, is_colon_field, md5_hex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LoginError {
    #[error("invalid nonce")]
    InvalidNonce,
    #[error("unsupported encoding")]
    UnsupportedEncoding,
    #[error("invalid field")]
    InvalidField,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginCredential {
    /// PasswordType == 0: an eight-digit client nonce and two MD5 proofs.
    Password { value: String, nonce: String },
    /// PasswordType != 0: the desktop sends this value in both proof fields.
    Verbatim(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginContext {
    pub screen_pixel_width: i32,
    pub screen_pixel_height: i32,
    pub desktop_metric: i32,
    pub screen_twips_per_pixel_x_text: String,
    pub screen_twips_per_pixel_y_text: String,
    pub form_vertical_inset: i32,
    pub form_horizontal_inset: i32,
    pub local_id: String,
    pub display_name: String,
    pub control_mode: i16,
    pub clear_control_mode: bool,
    pub local_user_sid: String,
    pub connection_hash_key: String,
    pub display_colors: i32,
    pub display_quality: i32,
    pub remote_wallpaper_disabled: i16,
    pub has_desktop_token: bool,
    pub partner_secret_token: String,
    pub client_version: String,
    pub desktop_counter: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartnerLoginMessage {
    pub credential: LoginCredential,
    pub context: LoginContext,
}

impl PartnerLoginMessage {
    pub fn text(&self) -> Result<String, LoginError> {
        let (nonce, first_proof, second_proof) = match &self.credential {
            LoginCredential::Password { value, nonce } => {
                if nonce.len() != 8 || !nonce.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(LoginError::InvalidNonce);
                }
                if !is_ascii(value) {
                    return Err(LoginError::UnsupportedEncoding);
                }
                (nonce.clone(), md5_hex(&(nonce.clone() + value)), md5_hex(&(" ".repeat(32) + value)))
            }
            LoginCredential::Verbatim(value) => {
                validate_field(value)?;
                (" ".repeat(32), value.clone(), value.clone())
            }
        };
        // Native helper 0x6cb240 removes these ASCII characters and takes Left(20).
        if !is_ascii(&self.context.display_name) {
            return Err(LoginError::UnsupportedEncoding);
        }
        const REMOVED: &str = "!#$&?:/\\*,.@<>;'\"=";
        let display_name: String =
            self.context.display_name.chars().filter(|c| !REMOVED.contains(*c)).take(20).collect();
        let c = &self.context;
        let fields = [
            "login".to_string(), nonce, first_proof,
            c.screen_pixel_width.to_string(), c.screen_pixel_height.to_string(),
            c.desktop_metric.to_string(), c.screen_twips_per_pixel_x_text.clone(), c.screen_twips_per_pixel_y_text.clone(),
            c.form_vertical_inset.to_string(), c.form_horizontal_inset.to_string(),
            c.local_id.clone(), display_name,
            (if c.clear_control_mode { 0 } else { c.control_mode }).to_string(), second_proof,
            c.local_user_sid.clone(), c.connection_hash_key.clone(),
            c.display_colors.to_string(), c.display_quality.to_string(),
            c.remote_wallpaper_disabled.to_string(), if c.has_desktop_token { "1".into() } else { "0".into() },
            c.partner_secret_token.clone(), c.client_version.clone(), c.desktop_counter.to_string(),
        ];
        for field in &fields {
            validate_field(field)?;
        }
        Ok(fields.join(":"))
    }

    /// ASCII is an exact subset of the original Windows ANSI conversion.
    pub fn encode(&self) -> Result<Vec<u8>, LoginError> {
        Ok(self.text()?.into_bytes())
    }
}

fn validate_field(value: &str) -> Result<(), LoginError> {
    if !is_ascii(value) {
        return Err(LoginError::UnsupportedEncoding);
    }
    if !is_colon_field(value) {
        return Err(LoginError::InvalidField);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HostLoginError {
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("unsupported encoding")]
    UnsupportedEncoding,
    #[error("invalid field count")]
    InvalidFieldCount,
    #[error("field {0} too large")]
    FieldTooLarge(usize),
    #[error("invalid command")]
    InvalidCommand,
    #[error("invalid nonce")]
    InvalidNonce,
    #[error("invalid proof")]
    InvalidProof,
    #[error("invalid field {0}")]
    InvalidField(usize),
    #[error("invalid configured password")]
    InvalidConfiguredPassword,
}

/// The ASCII `login:` application payload received by frmMain.ManipulatePacket
/// at 0x64c280. Claims remain untrusted until password verification and local
/// approval. This implements only the ordinary random-password branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLoginRequest {
    /// Includes empty fields and opaque desktop metadata, in original order.
    pub fields: Vec<String>,
    pub screen_pixel_width: i32,
    pub screen_pixel_height: i32,
    pub control_mode: i16,
    pub display_colors: i32,
    pub display_quality: i32,
}

impl HostLoginRequest {
    pub fn nonce(&self) -> &str {
        &self.fields[1]
    }
    pub fn claimed_id(&self) -> &str {
        &self.fields[10]
    }
    pub fn display_name(&self) -> &str {
        &self.fields[11]
    }
    pub fn connection_hash_key(&self) -> &str {
        &self.fields[15]
    }
    pub fn client_version(&self) -> &str {
        &self.fields[21]
    }

    pub fn decode(payload: &[u8]) -> Result<Self, HostLoginError> {
        Self::decode_with(payload, 16_384, 4_096)
    }

    /// Budgets and the requirement for exactly 23 current-version fields are
    /// local policy. The original accepts older, shorter login variants too.
    pub fn decode_with(payload: &[u8], maximum_payload_bytes: usize, maximum_field_bytes: usize) -> Result<Self, HostLoginError> {
        if payload.len() > maximum_payload_bytes {
            return Err(HostLoginError::PayloadTooLarge);
        }
        if !payload.iter().all(|b| (32..=126).contains(b)) {
            return Err(HostLoginError::UnsupportedEncoding);
        }
        if payload.iter().filter(|&&b| b == b':').count() != 22 {
            return Err(HostLoginError::InvalidFieldCount);
        }
        let text = std::str::from_utf8(payload).map_err(|_| HostLoginError::UnsupportedEncoding)?;
        let fields: Vec<String> = text.split(':').map(String::from).collect();
        for (index, field) in fields.iter().enumerate() {
            if field.len() > maximum_field_bytes {
                return Err(HostLoginError::FieldTooLarge(index));
            }
        }
        if fields[0] != "login" {
            return Err(HostLoginError::InvalidCommand);
        }
        if fields[1].len() != 8 || !fields[1].bytes().all(|b| b.is_ascii_digit()) {
            return Err(HostLoginError::InvalidNonce);
        }
        if fields[2].len() != 32 || !fields[2].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(HostLoginError::InvalidProof);
        }
        if !crate::util::is_digits(&fields[10]) || fields[10].parse::<u64>().map_or(true, |id| id == 0) {
            return Err(HostLoginError::InvalidField(10));
        }
        let int32 = |index: usize| -> Result<i32, HostLoginError> {
            fields[index].parse().map_err(|_| HostLoginError::InvalidField(index))
        };
        let width = int32(3)?;
        let height = int32(4)?;
        if width <= 0 {
            return Err(HostLoginError::InvalidField(3));
        }
        if height <= 0 {
            return Err(HostLoginError::InvalidField(4));
        }
        let mode: i16 = fields[12].parse().map_err(|_| HostLoginError::InvalidField(12))?;
        let display_colors = int32(16)?;
        let display_quality = int32(17)?;
        Ok(Self { fields, screen_pixel_width: width, screen_pixel_height: height, control_mode: mode, display_colors, display_quality })
    }

    /// Checks field2 == lowercase MD5(ASCII(nonce + locally configured password)).
    /// This result alone does not grant screen capture or input permission.
    pub fn verify(&self, random_password: &str) -> Result<bool, HostLoginError> {
        if random_password.is_empty()
            || random_password.len() > 256
            || !random_password.bytes().all(|b| (32..=126).contains(&b))
        {
            return Err(HostLoginError::InvalidConfiguredPassword);
        }
        let expected = hex_lower(&Md5::digest(format!("{}{}", self.nonce(), random_password).as_bytes()));
        // Both operands are exactly 32 ASCII bytes. No early mismatch return.
        let mut difference = 0u8;
        for (actual, expected) in self.fields[2].bytes().zip(expected.bytes()) {
            difference |= actual ^ expected;
        }
        Ok(difference == 0)
    }
}
