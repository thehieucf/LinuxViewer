//! Colon-delimited command8 plaintext, before the separate command cipher and
//! transport envelope. No operation here connects or authenticates a peer.

use crate::util::{is_ascii, md5_hex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LookupError {
    #[error("unsupported encoding")]
    UnsupportedEncoding,
    #[error("invalid field")]
    InvalidField,
    #[error("invalid reply")]
    InvalidReply,
    #[error("invalid number in field {0}")]
    InvalidNumber(usize),
    #[error("payload too large")]
    PayloadTooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupCredential {
    Password(String),
    /// The native PasswordType != 0 branch sends this value verbatim.
    Verbatim(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectDetails {
    pub command_public_ip: String,
    pub command_public_source_port: i32,
    pub local_ip: String,
    pub local_port: i32,
    /// The native controller creates this separately from ConnectionHashKey.
    pub encryption_key: String,
}

impl DirectDetails {
    fn text(&self) -> Result<String, LookupError> {
        let fields = [
            self.command_public_ip.clone(),
            self.command_public_source_port.to_string(),
            self.local_ip.clone(),
            self.local_port.to_string(),
            self.encryption_key.clone(),
        ];
        for field in &fields {
            validate_field(field)?;
            if field.contains('|') {
                return Err(LookupError::InvalidField);
            }
        }
        Ok(fields.join("|"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupRequest {
    pub target_id: String,
    pub credential: LookupCredential,
    pub client_version: String,
    /// Desktop global0x7e2100: license-context text, exact meaning unresolved.
    pub license_context: String,
    /// Desktop global0x7e2104: license-context text, exact meaning unresolved.
    pub additional_license_context: String,
    pub local_id: String,
    /// Main form's txtPassword.Text, used only to form the local MD5 proof.
    pub local_password: String,
    pub target_session_id: i32,
    /// Original desktop's Form hwnd, serialized as a signed Long.
    pub correlation_number: i32,
    pub is_reconnect: bool,
    pub referral_id_text: String,
    pub payment_type_text: String,
    pub date_time_offset_minutes: i32,
    pub object_dictionary_count: i32,
    pub fixed_ping_list: String,
    pub direct_details: Option<DirectDetails>,
}

impl LookupRequest {
    pub fn text(&self) -> Result<String, LookupError> {
        let peer_proof = match &self.credential {
            LookupCredential::Password(value) => {
                validate_ascii(value)?;
                md5_hex(&(" ".repeat(32) + value))
            }
            LookupCredential::Verbatim(value) => {
                validate_field(value)?;
                value.clone()
            }
        };
        validate_ascii(&self.local_password)?;
        let direct = match &self.direct_details {
            Some(details) => details.text()?,
            None => String::new(),
        };
        let fields = [
            "8".to_string(), self.target_id.clone(), peer_proof, self.client_version.clone(),
            self.license_context.clone(), self.additional_license_context.clone(), self.local_id.clone(),
            md5_hex(&(self.local_id.clone() + &self.local_password)), self.target_session_id.to_string(),
            self.correlation_number.to_string(), if self.is_reconnect { "-1".into() } else { "0".into() },
            self.referral_id_text.clone(), self.payment_type_text.clone(),
            self.date_time_offset_minutes.to_string(), self.object_dictionary_count.to_string(),
            self.fixed_ping_list.clone(), direct,
        ];
        for field in &fields {
            validate_field(field)?;
        }
        let result = fields.join(":");
        if result.len() > 65_536 {
            return Err(LookupError::PayloadTooLarge);
        }
        Ok(result)
    }

    pub fn encode(&self) -> Result<Vec<u8>, LookupError> {
        Ok(self.text()?.into_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupReply {
    /// Preserve unknown fields and optional suffixes. The native dispatcher
    /// uses the last field as a collection key; its fixed index is unproved.
    pub fields: Vec<String>,
    pub status: i32,
}

impl LookupReply {
    pub fn parse(plaintext: &[u8]) -> Result<Self, LookupError> {
        if plaintext.len() > 65_536 {
            return Err(LookupError::PayloadTooLarge);
        }
        if !plaintext.iter().all(|b| (32..=126).contains(b)) {
            return Err(LookupError::UnsupportedEncoding);
        }
        let text = std::str::from_utf8(plaintext).map_err(|_| LookupError::UnsupportedEncoding)?;
        let fields: Vec<String> = text.split(':').map(String::from).collect();
        if fields.len() < 2 || fields.len() > 128 || fields[0] != "8" {
            return Err(LookupError::InvalidReply);
        }
        let status: i32 = fields[1].parse().map_err(|_| LookupError::InvalidReply)?;
        Ok(Self { fields, status })
    }

    pub fn correlation_number(&self) -> Option<i32> {
        self.fields.last().and_then(|f| f.parse().ok())
    }

    /// Status1 enters native peer-context setup. It is a lookup result,
    /// before transport connection, Login, and application acceptance.
    pub fn peer_info(&self) -> Result<Option<PeerInfo>, LookupError> {
        if self.status != 1 {
            return Ok(None);
        }
        if self.fields.len() < 15 {
            return Err(LookupError::InvalidReply);
        }
        let number = |index: usize| -> Result<i32, LookupError> {
            self.fields[index].parse().map_err(|_| LookupError::InvalidNumber(index))
        };
        let matched: i16 = self.fields[12].parse().map_err(|_| LookupError::InvalidNumber(12))?;
        Ok(Some(PeerInfo {
            lan_ip: self.fields[2].clone(),
            public_ip: self.fields[4].clone(),
            public_port: number(5)?,
            lan_port: number(6)?,
            command_port: number(7)?,
            computer_name_field: self.fields[8].clone(),
            target_tcp_port: number(9)?,
            relay_ip: self.fields[11].clone(),
            matched_custom_password: matched,
            target_session_id: number(13)?,
            relay_hostname: self.fields[14].clone(),
            partner_secret_token: self.fields.get(20).cloned().filter(|_| self.fields.len() > 20),
            direct_details_field: self.fields.get(21).cloned().filter(|_| self.fields.len() > 21),
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub lan_ip: String,
    pub public_ip: String,
    pub public_port: i32,
    pub lan_port: i32,
    pub command_port: i32,
    /// Native DecodeUTF8Base64 consumes field8; the codec preserves it raw.
    pub computer_name_field: String,
    /// Native TargetTCPPort, reply field9: used with publicIP for relay
    /// forwarding and public P2P targeting.
    pub target_tcp_port: i32,
    /// Native TargetTCPServerIP, reply field11: relay Connect host.
    pub relay_ip: String,
    pub matched_custom_password: i16,
    pub target_session_id: i32,
    pub relay_hostname: String,
    pub partner_secret_token: Option<String>,
    /// Native code reads field21; its detailed subformat remains separate.
    pub direct_details_field: Option<String>,
}

fn validate_ascii(value: &str) -> Result<(), LookupError> {
    if is_ascii(value) {
        Ok(())
    } else {
        Err(LookupError::UnsupportedEncoding)
    }
}

fn validate_field(value: &str) -> Result<(), LookupError> {
    validate_ascii(value)?;
    if crate::util::is_colon_field(value) {
        Ok(())
    } else {
        Err(LookupError::InvalidField)
    }
}
