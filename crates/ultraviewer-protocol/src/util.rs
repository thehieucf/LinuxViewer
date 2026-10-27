use md5::{Digest, Md5};

pub(crate) fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from(bytes[offset]) | (u16::from(bytes[offset + 1]) << 8)
}

pub(crate) fn read_i16(bytes: &[u8], offset: usize) -> i16 {
    read_u16(bytes, offset) as i16
}

pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

pub(crate) fn read_i32(bytes: &[u8], offset: usize) -> i32 {
    read_u32(bytes, offset) as i32
}

/// Lowercase hexadecimal MD5 of ASCII/UTF-8 bytes.
pub(crate) fn md5_hex(value: &str) -> String {
    hex_lower(&Md5::digest(value.as_bytes()))
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn is_ascii(value: &str) -> bool {
    value.bytes().all(|b| b < 128)
}

/// Printable ASCII excluding the colon delimiter.
pub(crate) fn is_colon_field(value: &str) -> bool {
    value.bytes().all(|b| (32..=126).contains(&b) && b != b':')
}

/// RFC 3986 query-component encoding: unreserved bytes pass through.
pub(crate) fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub(crate) fn is_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

pub(crate) fn utf16le(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// Version text "a.b.c" compared lexicographically with the requirement.
pub(crate) fn version_at_least(text: &str, required: [i64; 3]) -> bool {
    let fields: Vec<&str> = text.split('.').collect();
    if fields.len() != 3 {
        return false;
    }
    let mut values = [0i64; 3];
    for (index, field) in fields.iter().enumerate() {
        match field.parse::<i64>() {
            Ok(value) if value >= 0 => values[index] = value,
            _ => return false,
        }
    }
    values >= required
}
