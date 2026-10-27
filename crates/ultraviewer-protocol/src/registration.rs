//! Initial desktop command 3, reconstructed from frmMain.InitMyPassword at
//! 0x67cb00. This codec does not discover a server or authenticate a peer.

use serde::{Deserialize, Serialize};

use crate::crypto::LegacyCommandCipher;
use crate::relay::{encode_system, RelayIPv4Address};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegistrationError {
    #[error("unsupported encoding")]
    UnsupportedEncoding,
    #[error("invalid field {0}")]
    InvalidField(usize),
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("too many fields")]
    TooManyFields,
    #[error("missing fields")]
    MissingFields,
    #[error("invalid command")]
    InvalidCommand,
    #[error("invalid port")]
    InvalidPort,
    #[error("invalid ID")]
    InvalidId,
    #[error("invalid address")]
    InvalidAddress,
    #[error("invalid status")]
    InvalidStatus,
}

/// Windows-specific identity/configuration inputs are explicit. Callers must
/// choose their own truthful platform values; this codec invents none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientRegistrationRequest {
    /// Desktop global 0x7e20dc; derived from disk/OS identity at startup.
    pub device_identity: String,
    pub password: String,
    pub local_address: RelayIPv4Address,
    /// Output of VFunction.EncodeUTF8Base64(GetComputerName()).
    #[serde(rename = "computerNameUTF8Base64")]
    pub computer_name_utf8_base64: String,
    pub client_version: String,
    pub mac_address: String,
    /// Desktop global 0x7e211c. Colons become hyphens on serialization.
    pub disk_identity: String,
    /// GetOSVersionCombine: Build|Major|MajorRevision|Minor|MinorRevision|Revision.
    pub operating_system: String,
    pub installer_referral: i32,
    pub custom_password: String,
    /// VTCPNhanLenh.UsingProxy (DISPID 0x2a), independent of TLS.
    pub uses_proxy: bool,
    pub quick_support_metadata: String,
    /// Already encoded output of EncodeBase64IfNotASCII(Environ("USERNAME")).
    pub user_name_wire_value: String,
    #[serde(rename = "activeSessionID")]
    pub active_session_id: i32,
    #[serde(rename = "processSessionID")]
    pub process_session_id: i32,
    #[serde(rename = "preferredIDText")]
    pub preferred_id_text: String,
    #[serde(rename = "referralIDText")]
    pub referral_id_text: String,
    pub automatic_update: i16,
    pub system_folder_creation_ticks: String,
    pub program_files_creation_ticks: String,
    pub system_volume_information_creation_ticks: String,
    pub license_context1: String,
    pub license_context2: String,
    pub captcha_token: String,
}

impl ClientRegistrationRequest {
    /// The timestamp composite contributes three wire fields. There are 28
    /// fields, including the command and every empty field, in this branch.
    pub fn text_with_limit(&self, maximum_payload_bytes: isize) -> Result<String, RegistrationError> {
        let disk = self.disk_identity.replace(':', "-");
        let fields: Vec<String> = vec![
            "3".into(), self.device_identity.clone(), self.password.clone(), "0".into(),
            self.local_address.to_string(), "0".into(), self.computer_name_utf8_base64.clone(),
            self.client_version.clone(), self.mac_address.clone(), disk, self.operating_system.clone(),
            self.installer_referral.to_string(), String::new(), self.custom_password.clone(),
            if self.uses_proxy { "-1".into() } else { "0".into() },
            self.quick_support_metadata.clone(), self.user_name_wire_value.clone(),
            self.active_session_id.to_string(), self.process_session_id.to_string(),
            self.preferred_id_text.clone(), self.referral_id_text.clone(), self.automatic_update.to_string(),
            self.system_folder_creation_ticks.clone(), self.program_files_creation_ticks.clone(),
            self.system_volume_information_creation_ticks.clone(), self.license_context1.clone(),
            self.license_context2.clone(), self.captcha_token.clone(),
        ];
        let mut byte_count = fields.len() as isize - 1;
        if maximum_payload_bytes < byte_count {
            return Err(RegistrationError::PayloadTooLarge);
        }
        for (index, field) in fields.iter().enumerate() {
            validate_field(field, index)?;
            if field.len() as isize > maximum_payload_bytes - byte_count {
                return Err(RegistrationError::PayloadTooLarge);
            }
            byte_count += field.len() as isize;
        }
        Ok(fields.join(":"))
    }

    pub fn text(&self) -> Result<String, RegistrationError> {
        self.text_with_limit(65_536)
    }

    /// ASCII is an exact subset of the desktop's Windows ANSI conversion.
    pub fn encode(&self) -> Result<Vec<u8>, RegistrationError> {
        Ok(self.text()?.into_bytes())
    }

    pub fn encode_with_limit(&self, maximum_payload_bytes: isize) -> Result<Vec<u8>, RegistrationError> {
        Ok(self.text_with_limit(maximum_payload_bytes)?.into_bytes())
    }

    /// Inner command envelope: 01 00 followed by configured 3DES ciphertext.
    /// The second byte is the explicit TCP-v2 format flag, observed at the
    /// native wrapper call. It is not part of the encrypted plaintext.
    pub fn relay_envelope(&self, cipher: &LegacyCommandCipher) -> Result<Vec<u8>, RegistrationError> {
        Ok(encode_system(&cipher.encrypt(&self.encode()?), Some(false)))
    }

    pub fn framed(&self, cipher: &LegacyCommandCipher) -> Result<Vec<u8>, RegistrationError> {
        crate::frame::encode(&self.relay_envelope(cipher)?).map_err(|_| RegistrationError::PayloadTooLarge)
    }
}

/// Plain server command 3 after the outer relay kind byte has been stripped.
/// A parsed registration reply establishes no partner authentication claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRegistrationReply {
    pub public_port: u16,
    pub assigned_id: u64,
    pub public_address: RelayIPv4Address,
    pub status: i32,
    /// All original fields, including empty/unknown fields and future tails.
    pub fields: Vec<String>,
}

impl ClientRegistrationReply {
    pub fn assigned_id_text(&self) -> String {
        self.assigned_id.to_string()
    }
    pub fn ping_list(&self) -> &str {
        &self.fields[6]
    }
    /// The desktop only consumes field 9 when its socket uses TLS.
    pub fn ping_host_name_list(&self) -> &str {
        &self.fields[9]
    }
    pub fn opaque_server_token(&self) -> &str {
        &self.fields[10]
    }
    pub fn requests_old_version_warning(&self) -> bool {
        self.fields[5] == "1"
    }
    /// The desktop parses dd/MM/yyyy only when status is not -1.
    pub fn status_date_text(&self) -> Option<&str> {
        if self.status == -1 {
            None
        } else {
            Some(&self.fields[8])
        }
    }

    pub fn parse(plaintext: &[u8]) -> Result<Self, RegistrationError> {
        Self::parse_with(plaintext, 65_536, 128)
    }

    /// Budgets are local defenses, not recovered protocol limits. The native
    /// branch indexes field 10 unconditionally, so at least 11 are required.
    pub fn parse_with(plaintext: &[u8], maximum_payload_bytes: usize, maximum_field_count: usize) -> Result<Self, RegistrationError> {
        if plaintext.len() > maximum_payload_bytes {
            return Err(RegistrationError::PayloadTooLarge);
        }
        if !plaintext.iter().all(|b| (32..=126).contains(b)) {
            return Err(RegistrationError::UnsupportedEncoding);
        }
        if maximum_field_count == 0 {
            return Err(RegistrationError::TooManyFields);
        }
        let mut field_count = 1;
        for &byte in plaintext {
            if byte == b':' {
                if field_count >= maximum_field_count {
                    return Err(RegistrationError::TooManyFields);
                }
                field_count += 1;
            }
        }
        if field_count < 11 {
            return Err(RegistrationError::MissingFields);
        }
        let text = std::str::from_utf8(plaintext).map_err(|_| RegistrationError::UnsupportedEncoding)?;
        let fields: Vec<String> = text.split(':').map(String::from).collect();
        if fields[0] != "3" {
            return Err(RegistrationError::InvalidCommand);
        }
        if !crate::util::is_digits(&fields[1]) {
            return Err(RegistrationError::InvalidPort);
        }
        let port: u16 = fields[1].parse().map_err(|_| RegistrationError::InvalidPort)?;
        if port == 0 {
            return Err(RegistrationError::InvalidPort);
        }
        if !crate::util::is_digits(&fields[2]) {
            return Err(RegistrationError::InvalidId);
        }
        let id: u64 = fields[2].parse().map_err(|_| RegistrationError::InvalidId)?;
        if id == 0 {
            return Err(RegistrationError::InvalidId);
        }
        let address = RelayIPv4Address::parse(&fields[3]).map_err(|_| RegistrationError::InvalidAddress)?;
        let status: i32 = fields[7].parse().map_err(|_| RegistrationError::InvalidStatus)?;
        if status.to_string() != fields[7] {
            return Err(RegistrationError::InvalidStatus);
        }
        Ok(Self { public_port: port, assigned_id: id, public_address: address, status, fields })
    }
}

/// Server command 60, sent instead of the command-3 reply when the service
/// wants a person to solve a captcha before it assigns an ID. Recovered from
/// the desktop dispatcher at 0x676947: `60:<esc>:<extra>` opens the captcha
/// page in an embedded browser, whose `window.external.FromJS_CaptchaCorrect`
/// callback (RemoteControl VChatBrowser) supplies the token that the next
/// registration carries in its last field (desktop global 0x7e21dc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptchaChallenge {
    /// Opaque server session (base64), passed back as `esc`.
    pub session: String,
    /// Optional opaque value, passed back as `extra`.
    pub extra: String,
}

impl CaptchaChallenge {
    pub const PAGE: &'static str = "https://www.ultraviewer.net/api/enter_captcha.aspx";

    pub fn parse(plaintext: &[u8]) -> Result<Self, RegistrationError> {
        if plaintext.len() > 4096 {
            return Err(RegistrationError::PayloadTooLarge);
        }
        let text = std::str::from_utf8(plaintext).map_err(|_| RegistrationError::UnsupportedEncoding)?;
        let mut fields = text.split(':');
        if fields.next() != Some("60") {
            return Err(RegistrationError::InvalidCommand);
        }
        let session = fields.next().unwrap_or_default();
        if session.is_empty() || !crate::util::is_colon_field(session) {
            return Err(RegistrationError::InvalidField(1));
        }
        let extra = fields.next().unwrap_or_default();
        if !crate::util::is_colon_field(extra) {
            return Err(RegistrationError::InvalidField(2));
        }
        Ok(Self { session: session.into(), extra: extra.into() })
    }

    /// Page address for `language` (the desktop sends its UI language, e.g. "vi" or "en").
    pub fn url(&self, language: &str) -> String {
        use crate::util::percent_encode;
        format!(
            "{}?lang={}&version={}&extra={}&esc={}",
            Self::PAGE,
            percent_encode(language),
            percent_encode(crate::CLIENT_VERSION),
            percent_encode(&self.extra),
            percent_encode(&self.session)
        )
    }

    /// The page's token must fit a registration field.
    pub fn valid_token(token: &str) -> bool {
        !token.is_empty() && token.len() <= 4096 && crate::util::is_colon_field(token)
    }
}

fn validate_field(value: &str, index: usize) -> Result<(), RegistrationError> {
    if !crate::util::is_ascii(value) {
        return Err(RegistrationError::UnsupportedEncoding);
    }
    if !crate::util::is_colon_field(value) {
        return Err(RegistrationError::InvalidField(index));
    }
    Ok(())
}
