//! Byte-level fixtures ported from MacViewer's UltraViewerProtocol test suite.

use base64::Engine;
use ultraviewer_protocol::chat::{self, ChatEvent};
use ultraviewer_protocol::clipboard::{self, ViewerEncoding};
use ultraviewer_protocol::commands::{RemoteAction, RemoteControlCommands};
use ultraviewer_protocol::crypto::{LegacyAes, LegacyCommandCipher};
use ultraviewer_protocol::cursor::{CursorFile, CursorMessage, CursorMessageError};
use ultraviewer_protocol::frame::{self, FrameDecoder};
use ultraviewer_protocol::input::{HostInputMessage, KeyboardKind, KeyboardMessage, MouseButton, MouseMessage};
use ultraviewer_protocol::login::{HostLoginRequest, LoginContext, LoginCredential, PartnerLoginMessage};
use ultraviewer_protocol::lookup::{DirectDetails, LookupCredential, LookupReply, LookupRequest};
use ultraviewer_protocol::multipart::{self, Limits, MultipartError};
use ultraviewer_protocol::registration::{ClientRegistrationReply, ClientRegistrationRequest, RegistrationError};
use ultraviewer_protocol::relay::{self, DirectPeerEnvelope, HttpMethod, HttpPreambleDecoder, RelayEnvelope, RelayIPv4Address, RelayKeepalive};
use ultraviewer_protocol::screen::{ModernScreenFrameMessage, ModernScreenInitialization, ModernScreenLimits, ModernScreenRenderer, ScreenError, ScreenRegion};
use ultraviewer_protocol::zstd::{self as uvzstd, ZstdDecodeError, ZstdScreenDecoder};
use ultraviewer_protocol::{dictionary, G722Decoder};

fn hex(value: &str) -> Vec<u8> {
    (0..value.len()).step_by(2).map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap()).collect()
}

fn b64(value: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(value).unwrap()
}

// ---------------------------------------------------------------- crypto

#[test]
fn key_copy_overlap_matches_original_il() {
    assert_eq!(LegacyAes::derive_key(b"password").to_vec(), hex("5f4dcc3b5aa765d61d8327deb882cf5f4dcc3b5aa765d61d8327deb882cf9900"));
}

#[test]
fn aes_independent_openssl_vector() {
    let cipher = LegacyAes::new("password").unwrap();
    let plaintext = b"MacViewer protocol fixture";
    let iv: Vec<u8> = (0..16).collect();
    let expected = hex("000102030405060708090a0b0c0d0e0f714d2fe102258f61487d3b4c2497b3400c219660f0ef67fecff4f5405ae21add");
    assert_eq!(cipher.encrypt_with_iv(plaintext, &iv).unwrap(), expected);
    assert_eq!(cipher.decrypt(&expected).unwrap(), plaintext);
}

#[test]
fn aes_windows_library_fixture() {
    let cipher = LegacyAes::new("password").unwrap();
    let expected = hex("faa12d4ee98a5c7551b89ffc17b9b9bb68f46bb27a7a506df7533f7a06909bcd3eca1d95daf4532980a587df2543e244");
    let plaintext = b"MacViewer protocol fixture";
    assert_eq!(cipher.decrypt(&expected).unwrap(), plaintext);
    assert_eq!(cipher.encrypt_with_iv(plaintext, &expected[..16]).unwrap(), expected);
}

#[test]
fn aes_payloads_survive_fragmented_frames() {
    let cipher = LegacyAes::new("fixture-only").unwrap();
    let plaintexts = [vec![], vec![1], vec![42; 16], vec![7; 4097]];
    let mut decoder = FrameDecoder::default();
    let mut received = Vec::new();
    for plaintext in &plaintexts {
        let framed = frame::encode(&cipher.encrypt(plaintext)).unwrap();
        for byte in framed {
            for payload in decoder.append(&[byte]).unwrap() {
                received.push(cipher.decrypt(&payload).unwrap());
            }
        }
    }
    assert_eq!(received, plaintexts);
    assert!(!decoder.has_incomplete_frame());
}

#[test]
fn aes_fresh_iv_and_malformed_input() {
    let cipher = LegacyAes::new("fixture-only").unwrap();
    let first = cipher.encrypt(b"same plaintext");
    let second = cipher.encrypt(b"same plaintext");
    assert_ne!(first[..16], second[..16]);
    assert!(LegacyAes::new("mật-khẩu").is_err());
    for length in [0, 1, 15, 16, 17, 31, 33] {
        assert!(cipher.decrypt(&vec![0; length]).is_err());
    }
    assert!(cipher.encrypt_with_iv(b"", &[1]).is_err());
    let iv: Vec<u8> = (0..16).collect();
    let mut corrupt = cipher.encrypt_with_iv(b"fixture", &iv).unwrap();
    corrupt[15] ^= 1;
    assert!(cipher.decrypt(&corrupt).is_err());
}

#[test]
fn command_cipher_openssl_vectors() {
    let cipher = LegacyCommandCipher::new("fixture-⌘");
    let plaintext = b"8:123456789:synthetic-proof";
    let expected = hex("b63b9797064974fa31181fef729c755558492379dcb44c69e3fe4c6d0e29836c");
    assert_eq!(cipher.encrypt(plaintext), expected);
    assert_eq!(cipher.decrypt(&expected).unwrap(), plaintext);
    let desktop = LegacyCommandCipher::desktop_server_commands();
    let encrypted = hex("c842551785856d0b8f866afed8dcd8b1");
    assert_eq!(desktop.encrypt(b"8:fixture"), encrypted);
    assert_eq!(desktop.decrypt(&encrypted).unwrap(), b"8:fixture");
}

#[test]
fn command_cipher_round_trips_and_rejects_padding() {
    for password in ["", "compatibility-test", "🔑é⌘"] {
        let cipher = LegacyCommandCipher::new(password);
        for length in [0usize, 1, 7, 8, 9, 15, 16, 17, 1024] {
            let plaintext: Vec<u8> = (0..length).map(|i| i as u8).collect();
            let encrypted = cipher.encrypt(&plaintext);
            assert_eq!(encrypted.len(), (length / 8 + 1) * 8);
            assert_eq!(cipher.decrypt(&encrypted).unwrap(), plaintext);
        }
    }
    let cipher = LegacyCommandCipher::new("fixture-⌘");
    for invalid in [vec![], vec![1], vec![0; 7], vec![0; 9]] {
        assert!(cipher.decrypt(&invalid).is_err());
    }
    for invalid in ["e074f13ae60ec9e4", "22959c7b190b62a7", "da71e6936b3c4dba"] {
        assert!(cipher.decrypt(&hex(invalid)).is_err());
    }
}

// ---------------------------------------------------------------- relay / framing

#[test]
fn relay_heartbeats_match_wire_fixtures() {
    assert_eq!(frame::encode(&RelayKeepalive::relay_envelope()).unwrap(), vec![5, 0, 0, 0, 1, 0x70, 0x69, 0x6e, 0x67]);
    assert_eq!(
        frame::encode(&RelayKeepalive::command_envelope()).unwrap(),
        vec![10, 0, 0, 0, 1, 0, 0x2d, 0x93, 0x29, 0x71, 0xdd, 0x9d, 0xcb, 0x2d]
    );
}

#[test]
fn relay_routing_byte_order_and_round_trip() {
    let envelope = RelayEnvelope::Forward {
        address: RelayIPv4Address::parse("192.0.2.10").unwrap(),
        port: 12345,
        format_flag: false,
        payload: vec![0xaa, 0xbb],
    };
    let inner = vec![0, 0, 10, 2, 0, 192, 0x39, 0x30, 0xaa, 0xbb];
    assert_eq!(envelope.encode(), inner);
    assert_eq!(RelayEnvelope::decode(&inner).unwrap(), envelope);
    assert_eq!(RelayIPv4Address::parse("192.000.2.10").unwrap().to_string(), "192.0.2.10");
    let maximum = RelayEnvelope::Forward {
        address: RelayIPv4Address::parse("255.255.255.255").unwrap(),
        port: u16::MAX,
        format_flag: true,
        payload: vec![],
    };
    assert_eq!(maximum.encode(), vec![0, 1, 255, 255, 255, 255, 255, 255]);
    for invalid in ["", "1.2.3", "1.2.3.4.5", "1..3.4", "256.2.3.4", "-1.2.3.4", "1.2.3. 4", "1.2.3.4\n", "１.2.3.4", "0000.2.3.4"] {
        assert!(RelayIPv4Address::parse(invalid).is_err(), "{invalid}");
    }
    assert!(RelayEnvelope::decode(&[]).is_err());
    for count in 1..8 {
        assert!(RelayEnvelope::decode(&vec![0; count]).is_err());
    }
    assert!(RelayEnvelope::decode(&[2]).is_err());
}

#[test]
fn system_and_direct_envelopes() {
    let payload = [0x31, 0x7c, 0x7c, 0x7c];
    assert_eq!(relay::encode_system(&payload, Some(false)), [&[1u8, 0][..], &payload].concat());
    assert_eq!(RelayEnvelope::decode(&[1, 1, 0x31]).unwrap(), RelayEnvelope::System { payload: vec![1, 0x31] });
    let peer = DirectPeerEnvelope { format_flag: true, payload: payload.to_vec() };
    assert_eq!(peer.encode(), [&[1u8][..], &payload].concat());
    assert_eq!(DirectPeerEnvelope::decode(&peer.encode()).unwrap(), peer);
}

#[test]
fn http_preamble_and_decoder() {
    let expected = "POST /UVService/ HTTP/1.1\r\nUser-Agent: Mozilla/4.0 (compatible; MSIE5.01; Windows NT)\r\nContent-Type: application/octet-stream\r\nConnection: Keep-Alive\r\nContent-length: 8589934590\r\nHost: example.invalid:443\r\n\r\n";
    assert_eq!(relay::http_preamble("example.invalid:443", HttpMethod::Post).unwrap(), expected.as_bytes());
    for invalid in ["", "a\r\nX: injected", "a b", "a\t", "a\u{7f}", "é.example"] {
        assert!(relay::http_preamble(invalid, HttpMethod::Post).is_err());
    }
    let header = b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\r\n".to_vec();
    let body = vec![3, 0, 0, 0, 1, 2, 3, 13, 10, 13, 10];
    let wire = [header.clone(), body.clone()].concat();
    for split in 0..=wire.len() {
        let mut decoder = HttpPreambleDecoder::new(header.len());
        let mut result = decoder.append(&wire[..split]).unwrap().to_vec();
        result.extend_from_slice(decoder.append(&wire[split..]).unwrap());
        assert_eq!(result, body);
        assert_eq!(decoder.response_status_code(), Some(200));
    }
    let mut bad = HttpPreambleDecoder::new(4);
    assert!(bad.append(&[1, 2, 3, 4]).unwrap().is_empty());
    assert!(bad.append(&[5]).is_err());
}

#[test]
fn multipart_fixtures_and_limits() {
    let parts = vec![vec![0u8], vec![255, 0, 128, 58], vec![], vec![], vec![65]];
    let expected = vec![1, 0, 0, 0, 0, 4, 0, 0, 0, 255, 0, 128, 58, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 65];
    assert_eq!(multipart::encode(&parts).unwrap(), expected);
    assert_eq!(multipart::decode(&expected).unwrap(), parts);
    for bytes in [vec![0u8], vec![0, 0], vec![0, 0, 0]] {
        assert_eq!(multipart::decode(&bytes), Err(MultipartError::TruncatedLength));
    }
    assert_eq!(multipart::decode(&[3, 0, 0, 0, 1, 2]), Err(MultipartError::TruncatedPart));
    assert_eq!(multipart::decode(&[0, 0, 0, 128]), Err(MultipartError::InvalidLength));
    let limits = Limits::new(10, 2, 2);
    let exact = vec![2, 0, 0, 0, 1, 2, 0, 0, 0, 0];
    assert_eq!(multipart::decode_with(&exact, limits).unwrap(), vec![vec![1, 2], vec![]]);
    assert_eq!(multipart::encode_with(&[vec![1u8, 2], vec![2]], limits), Err(MultipartError::PayloadTooLarge));
}

// ---------------------------------------------------------------- registration / lookup / login

fn registration() -> ClientRegistrationRequest {
    ClientRegistrationRequest {
        device_identity: "0123456789ABCDEF0123456789ABCDEF".into(),
        password: "654321".into(),
        local_address: RelayIPv4Address::parse("192.0.2.10").unwrap(),
        computer_name_utf8_base64: "TWFj".into(),
        client_version: "6.6.124".into(),
        mac_address: "001122334455".into(),
        disk_identity: "disk:serial".into(),
        operating_system: "synthetic-os".into(),
        installer_referral: 0,
        custom_password: String::new(),
        uses_proxy: true,
        quick_support_metadata: String::new(),
        user_name_wire_value: "dean".into(),
        active_session_id: 1,
        process_session_id: 2,
        preferred_id_text: "0".into(),
        referral_id_text: "0".into(),
        automatic_update: -1,
        system_folder_creation_ticks: "638000000000000000".into(),
        program_files_creation_ticks: "638000000000000001".into(),
        system_volume_information_creation_ticks: "0".into(),
        license_context1: "license-A".into(),
        license_context2: "license-B".into(),
        captcha_token: String::new(),
    }
}

#[test]
fn registration_exact_fields() {
    let request = registration();
    let expected = "3:0123456789ABCDEF0123456789ABCDEF:654321:0:192.0.2.10:0:TWFj:6.6.124:001122334455:disk-serial:synthetic-os:0:::-1::dean:1:2:0:0:-1:638000000000000000:638000000000000001:0:license-A:license-B:";
    assert_eq!(request.text().unwrap(), expected);
    assert_eq!(expected.split(':').count(), 28);
    assert_eq!(request.encode_with_limit(expected.len() as isize).unwrap(), expected.as_bytes());
    assert_eq!(request.encode_with_limit(expected.len() as isize - 1), Err(RegistrationError::PayloadTooLarge));
    let mut injected = registration();
    injected.license_context1 = "extra:field".into();
    assert_eq!(injected.encode(), Err(RegistrationError::InvalidField(25)));
    injected.license_context1 = "opaque".into();
    injected.user_name_wire_value = "café".into();
    assert_eq!(injected.encode(), Err(RegistrationError::UnsupportedEncoding));
}

#[test]
fn registration_envelope_and_json() {
    let mut request = registration();
    request.uses_proxy = false;
    let cipher = LegacyCommandCipher::new("explicit-test-password");
    let envelope = request.relay_envelope(&cipher).unwrap();
    assert_eq!(&envelope[..2], &[1, 0]);
    assert_eq!(cipher.decrypt(&envelope[2..]).unwrap(), request.encode().unwrap());
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["localAddress"], "192.0.2.10");
    assert_eq!(json["computerNameUTF8Base64"], "TWFj");
    assert_eq!(json["activeSessionID"], 1);
    let decoded: ClientRegistrationRequest = serde_json::from_value(json).unwrap();
    assert_eq!(decoded, request);
}

#[test]
fn registration_reply_parse() {
    let text = "3:443:00111222333:203.0.113.7:unknown:1:ping-list:0:17/10/2026:relay.example:opaque-token:future:";
    let reply = ClientRegistrationReply::parse(text.as_bytes()).unwrap();
    assert_eq!(reply.public_port, 443);
    assert_eq!(reply.assigned_id_text(), "111222333");
    assert_eq!(reply.public_address.to_string(), "203.0.113.7");
    assert!(reply.requests_old_version_warning());
    assert_eq!(reply.ping_host_name_list(), "relay.example");
    assert_eq!(reply.status_date_text(), Some("17/10/2026"));
    let no_date = ClientRegistrationReply::parse(b"3:2112:42:192.0.2.2::0::-1:ignored::").unwrap();
    assert_eq!(no_date.status_date_text(), None);
    let valid = ["3", "443", "123456789", "203.0.113.10", "", "0", "", "-1", "", "", ""];
    for (index, value) in [(0, "8"), (1, "0"), (1, "65536"), (1, "-1"), (2, "0"), (2, "18446744073709551616"), (2, "1e5"), (3, "256.0.0.1"), (3, "localhost"), (7, "2147483648"), (7, "")] {
        let mut fields = valid.to_vec();
        fields[index] = value;
        assert!(ClientRegistrationReply::parse(fields.join(":").as_bytes()).is_err(), "{index} {value}");
    }
    assert_eq!(ClientRegistrationReply::parse(b"3:443:123:192.0.2.1"), Err(RegistrationError::MissingFields));
}

fn lookup() -> LookupRequest {
    LookupRequest {
        target_id: "peer-17".into(),
        credential: LookupCredential::Password("peer-pass".into()),
        client_version: "6.6.133".into(),
        license_context: "lic-a".into(),
        additional_license_context: "lic-b".into(),
        local_id: "local-42".into(),
        local_password: "self-pass".into(),
        target_session_id: -1,
        correlation_number: 42,
        is_reconnect: false,
        referral_id_text: "1.5".into(),
        payment_type_text: "free".into(),
        date_time_offset_minutes: 420,
        object_dictionary_count: 2,
        fixed_ping_list: String::new(),
        direct_details: None,
    }
}

#[test]
fn lookup_request_and_reply() {
    let expected = "8:peer-17:e3f326c385d2606a12a2d77260e3e81d:6.6.133:lic-a:lic-b:local-42:206a9ce1ea16f69943378fed8366ca11:-1:42:0:1.5:free:420:2::";
    assert_eq!(lookup().text().unwrap(), expected);
    let mut request = lookup();
    request.credential = LookupCredential::Verbatim("opaque-token".into());
    request.is_reconnect = true;
    request.direct_details = Some(DirectDetails {
        command_public_ip: "203.0.113.8".into(),
        command_public_source_port: 32100,
        local_ip: "192.0.2.5".into(),
        local_port: 12000,
        encryption_key: "key-123456".into(),
    });
    let text = request.text().unwrap();
    let fields: Vec<&str> = text.split(':').collect();
    assert_eq!(fields[16], "203.0.113.8|32100|192.0.2.5|12000|key-123456");
    let reply = LookupReply::parse(b"8:1:192.0.2.9:unused:203.0.113.9:54000:5900:5901:TmFtZQ==:55100:unused:198.51.100.2:-1:123:relay.example:x:x:x:x:x:secret:direct-info:42").unwrap();
    assert_eq!(reply.correlation_number(), Some(42));
    let peer = reply.peer_info().unwrap().unwrap();
    assert_eq!(peer.public_ip, "203.0.113.9");
    assert_eq!(peer.relay_ip, "198.51.100.2");
    assert_eq!(peer.target_tcp_port, 55100);
    assert_eq!(peer.matched_custom_password, -1);
    assert_eq!(peer.partner_secret_token.as_deref(), Some("secret"));
    assert_eq!(peer.direct_details_field.as_deref(), Some("direct-info"));
    assert_eq!(LookupReply::parse(b"8:2:42").unwrap().peer_info().unwrap(), None);
}

fn login_context() -> LoginContext {
    LoginContext {
        screen_pixel_width: 1920,
        screen_pixel_height: 1080,
        desktop_metric: 7,
        screen_twips_per_pixel_x_text: "15".into(),
        screen_twips_per_pixel_y_text: "15".into(),
        form_vertical_inset: 30,
        form_horizontal_inset: 16,
        local_id: "111222333".into(),
        display_name: "Mac: Viewer!".into(),
        control_mode: 1,
        clear_control_mode: false,
        local_user_sid: "S-1-5-21-1-2-3-1001".into(),
        connection_hash_key: "connection-key".into(),
        display_colors: 32,
        display_quality: 7,
        remote_wallpaper_disabled: -1,
        has_desktop_token: true,
        partner_secret_token: "peer-secret".into(),
        client_version: "6.0.0".into(),
        desktop_counter: 42,
    }
}

#[test]
fn partner_login_fixture_and_host_verification() {
    let message = PartnerLoginMessage {
        credential: LoginCredential::Password { value: "test-pass".into(), nonce: "12345678".into() },
        context: login_context(),
    };
    let expected = "login:12345678:c10f3394b41ec5b86060e0285e0dd306:1920:1080:7:15:15:30:16:111222333:Mac Viewer:1:85347ba7a403ffed52d311b7c5b6b549:S-1-5-21-1-2-3-1001:connection-key:32:7:-1:1:peer-secret:6.0.0:42";
    assert_eq!(message.text().unwrap(), expected);
    let host = HostLoginRequest::decode(expected.as_bytes()).unwrap();
    assert!(host.verify("test-pass").unwrap());
    assert!(!host.verify("other").unwrap());
    assert_eq!(host.claimed_id(), "111222333");
    assert_eq!(host.connection_hash_key(), "connection-key");
    for nonce in ["", "1234567", "123456789", "12:45678"] {
        let bad = PartnerLoginMessage {
            credential: LoginCredential::Password { value: "test".into(), nonce: nonce.into() },
            context: login_context(),
        };
        assert!(bad.encode().is_err());
    }
}

// ---------------------------------------------------------------- input

#[test]
fn mouse_and_keyboard_payloads() {
    assert_eq!(MouseMessage::Move { w_param: 1, x: -1920, y: 1080 }.text(), "8|||512|||1|||-1920|||1080");
    assert_eq!(MouseMessage::Button { button: MouseButton::LeftDown, w_param: 1, l_param: 0x0028_001e }.text(), "8|||513|||1|||2621470");
    assert_eq!(MouseMessage::wheel_w_param(-120, 8), -7_864_312);
    assert_eq!(MouseMessage::packed_position(-10, -20), -1_245_194);
    assert_eq!(MouseMessage::wheel_w_param(120, 0), 7_864_320);
    let key = KeyboardMessage { kind: KeyboardKind::KeyDown, virtual_key: 0x41, scan_code: 0x001e, character: 0x41, caps_lock_on: false };
    assert_eq!(key.encode(), vec![0x1e, 0x7c, 0x7c, 0x7c, 0x01, 0x41, 0x1e, 0x00, 0x41, 0x00, 0x00]);
    let system = KeyboardMessage { kind: KeyboardKind::SystemKeyUp, virtual_key: 0xa5, scan_code: 0x1234, character: 0x20ac, caps_lock_on: true };
    assert_eq!(system.encode(), vec![0x1e, 0x7c, 0x7c, 0x7c, 0x04, 0xa5, 0x34, 0x12, 0xac, 0x20, 0x01]);
    assert_eq!(HostInputMessage::decode(&system.encode()).unwrap(), Some(HostInputMessage::Keyboard(system)));
    assert_eq!(
        HostInputMessage::decode(b"8|||512|||1|||-1920|||1080").unwrap(),
        Some(HostInputMessage::Mouse(MouseMessage::Move { w_param: 1, x: -1920, y: 1080 }))
    );
    assert!(HostInputMessage::decode(b"8|||512|||1|||+1|||2").is_err());
}

// ---------------------------------------------------------------- chat / clipboard / cursor / commands

#[test]
fn chat_fixtures() {
    let text = "Aế🙂";
    let viewer = chat::viewer_to_host(text).unwrap();
    assert!(viewer.format_flag);
    assert_eq!(viewer.payload, vec![0x31, 0, 0x32, 0, 0x7c, 0, 0x7c, 0, 0x7c, 0, 0x41, 0, 0xbf, 0x1e, 0x3d, 0xd8, 0x42, 0xde]);
    let host = chat::host_to_viewer(text).unwrap();
    assert_eq!(host, vec![1, 0, 0, 0, 12, 8, 0, 0, 0, 0x41, 0, 0xbf, 0x1e, 0x3d, 0xd8, 0x42, 0xde]);
    assert_eq!(chat::decode_viewer_parts(&multipart::decode(&host).unwrap()).unwrap(), Some(ChatEvent::Message(text.into())));
    assert_eq!(chat::decode_host_payload(format!("12|||{text}").as_bytes()).unwrap(), Some(ChatEvent::Message(text.into())));
    assert_eq!(chat::viewer_to_host_typing(true).payload, vec![0x32, 0, 0x39, 0]);
    assert_eq!(chat::host_to_viewer_typing(false), vec![1, 0, 0, 0, 30]);
    assert!(chat::viewer_to_host("a|||b").is_err());
    for bytes in [vec![0x41u8], vec![0, 0xd8], vec![0, 0xdc], vec![0, 0xd8, 0x41, 0]] {
        assert!(chat::decode_viewer_parts(&[vec![12], bytes]).is_err());
    }
    let boundary = "a".repeat(chat::MAXIMUM_TEXT_BYTES / 2);
    assert_eq!(chat::host_to_viewer(&boundary).unwrap().len(), chat::MAXIMUM_TEXT_BYTES + 9);
}

fn raw_zstd_with_content_size(bytes: &[u8]) -> Vec<u8> {
    let mut data = vec![0x28, 0xb5, 0x2f, 0xfd, 0xa0];
    data.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    let block = ((bytes.len() << 3) | 1) as u32;
    data.extend_from_slice(&block.to_le_bytes()[..3]);
    data.extend_from_slice(bytes);
    data
}

#[test]
fn clipboard_fixtures() {
    let text = "A🙂\r\n\t中|||B";
    let utf16 = vec![0x41, 0, 0x3d, 0xd8, 0x42, 0xde, 0x0d, 0, 0x0a, 0, 0x09, 0, 0x2d, 0x4e, 0x7c, 0, 0x7c, 0, 0x7c, 0, 0x42, 0];
    let compressed = raw_zstd_with_content_size(&utf16);
    assert_eq!(compressed[..12], [0x28, 0xb5, 0x2f, 0xfd, 0xa0, 22, 0, 0, 0, 0xb1, 0, 0]);
    let controller = [b"33|||22|||".to_vec(), compressed.clone()].concat();
    assert_eq!(clipboard::decode_viewer_to_host(&controller, false).unwrap(), text);
    assert_eq!(clipboard::decode_normalized_viewer_to_host(&controller).unwrap(), text);
    assert_eq!(clipboard::decode_host_to_viewer(&[vec![33], vec![22, 0, 0, 0], compressed.clone()]).unwrap(), text);
    assert_eq!(clipboard::decode_host_to_viewer(&[vec![5], utf16.clone()]).unwrap(), text);
    let long = text.repeat(1000);
    let viewer = clipboard::viewer_to_host(&long, ViewerEncoding::Compressed).unwrap();
    assert!(!viewer.format_flag);
    assert!(viewer.payload.starts_with(b"33|||22000|||"));
    assert!(viewer.payload.len() < 1024);
    assert_eq!(clipboard::decode_viewer_to_host(&viewer.payload, false).unwrap(), long);
    let host = clipboard::host_to_viewer(&long).unwrap();
    let parts = multipart::decode(&host).unwrap();
    assert_eq!(parts[1], vec![0xf0, 0x55, 0, 0]);
    assert_eq!(clipboard::decode_host_to_viewer(&parts).unwrap(), long);
    let legacy = clipboard::viewer_to_host("A🙂\r\n\t中", ViewerEncoding::LegacyUnicode).unwrap();
    assert!(legacy.format_flag);
    assert_eq!(clipboard::decode_viewer_to_host(&legacy.payload, true).unwrap(), "A🙂\r\n\t中");
    for prefix in ["33||||||", "33|||2||", "33|||+2|||", "33|||-2|||"] {
        assert!(clipboard::decode_normalized_viewer_to_host(prefix.as_bytes()).is_err());
    }
    let empty = clipboard::viewer_to_host("", ViewerEncoding::Compressed).unwrap();
    assert_eq!(clipboard::decode_viewer_to_host(&empty.payload, empty.format_flag).unwrap(), "");
}

fn cursor_raw_zstd(bytes: &[u8]) -> Vec<u8> {
    let block = (bytes.len() << 3) | 1;
    [vec![0x28, 0xb5, 0x2f, 0xfd, 0x20, bytes.len() as u8, block as u8, (block >> 8) as u8, 0], bytes.to_vec()].concat()
}

fn cursor_file_fixture(offset: u32) -> Vec<u8> {
    let mut bytes: Vec<u8> = vec![0, 0, 2, 0, 1, 0, 32, 32, 0, 0, 3, 0, 5, 0, 4, 0, 0, 0];
    bytes.extend_from_slice(&offset.to_le_bytes());
    if offset > 22 && offset < 1024 {
        bytes.extend(std::iter::repeat(0).take(offset as usize - 22));
    }
    bytes.extend_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);
    bytes
}

#[test]
fn cursor_messages() {
    assert_eq!(CursorMessage::decode(&[vec![3], [(-12345i32).to_le_bytes().to_vec(), vec![0xff]].concat()]).unwrap(), CursorMessage::Selection { hash: -12345 });
    assert_eq!(CursorMessage::shape_request(-12345), b"4|||-12345");
    let file = cursor_file_fixture(22);
    let decoded = CursorMessage::decode(&[vec![4], (-7i32).to_le_bytes().to_vec(), (file.len() as i32).to_le_bytes().to_vec(), cursor_raw_zstd(&file)]).unwrap();
    let CursorMessage::Shape { hash, cursor } = &decoded else { panic!("shape expected") };
    assert_eq!(*hash, -7);
    assert_eq!(cursor.resource_data, vec![0xaa, 0xbb, 0xcc, 0xdd]);
    assert_eq!((cursor.width, cursor.height, cursor.hot_spot_x, cursor.hot_spot_y), (32, 32, 3, 5));
    assert_eq!(CursorMessage::decode(&[vec![28], vec![0x00, 0x80], vec![0xff, 0x7f]]).unwrap(), CursorMessage::Position { x: i16::MIN, y: i16::MAX });
    assert_eq!(CursorFile::decode(&cursor_file_fixture(278), 1 << 21).unwrap().resource_data, vec![0xaa, 0xbb, 0xcc, 0xdd]);
    let mut invalid = cursor_file_fixture(22);
    invalid[11] = 1;
    assert_eq!(CursorFile::decode(&invalid, 1 << 21), Err(CursorMessageError::InvalidCursorFile));
    assert_eq!(CursorMessage::decode(&[vec![3]]), Err(CursorMessageError::MissingPart(1)));
    assert_eq!(
        CursorMessage::decode(&[vec![4], 3i32.to_le_bytes().to_vec(), 27i32.to_le_bytes().to_vec(), cursor_raw_zstd(&file)]),
        Err(CursorMessageError::DecompressedLengthMismatch { expected: 27, received: 26 })
    );
}

#[test]
fn remote_commands_and_monitors() {
    let action = RemoteControlCommands::action(RemoteAction::ControlAltDelete);
    assert!(action.format_flag);
    assert_eq!(action.payload, vec![0x32, 0, 0x32, 0]);
    assert_eq!(RemoteControlCommands::quality(45).payload, b"31|||45");
    assert_eq!(RemoteControlCommands::end_session().payload, b"19");
    let monitors = RemoteControlCommands::monitors(b"0:0:1920:1080|-1280:0:0:1024|").unwrap();
    assert_eq!(monitors.len(), 2);
    assert_eq!(monitors[1].index, 2);
    assert_eq!(monitors[1].width(), 1280);
    let region = monitors[1].region(-1280, 0, 3200, 1080).unwrap();
    assert_eq!(region, ScreenRegion::new(0, 0, 1280, 1024));
    assert!(RemoteControlCommands::monitors(b"0:0:1920:1080").is_err());
    let request = RemoteControlCommands::decode_side_switch_request(b"54|||123456789|||pass word").unwrap().unwrap();
    assert_eq!(request.claimed_id, "123456789");
}

// ---------------------------------------------------------------- zstd / dictionary

const COMPRESSED_SCREEN_FIXTURE: [u8; 42] = [
    0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x58, 0x0d, 0x01, 0x00, 0xc8, 0x4d, 0x61, 0x63, 0x56, 0x69, 0x65, 0x77, 0x65, 0x72,
    0x20, 0x73, 0x63, 0x72, 0x65, 0x65, 0x6e, 0x20, 0x66, 0x69, 0x78, 0x74, 0x75, 0x72, 0x65, 0x0a, 0x01, 0x00, 0xa1,
    0x86, 0xfd, 0x39, 0x06,
];

#[test]
fn zstd_fixture_at_every_fragment_boundary() {
    let expected = "MacViewer screen fixture\n".repeat(100).into_bytes();
    for split in 0..=COMPRESSED_SCREEN_FIXTURE.len() {
        let mut decoder = ZstdScreenDecoder::new(128 << 20, None).unwrap();
        let mut decoded = decoder.append(&COMPRESSED_SCREEN_FIXTURE[..split]).unwrap();
        decoded.extend(decoder.append(&COMPRESSED_SCREEN_FIXTURE[split..]).unwrap());
        decoder.finish().unwrap();
        assert_eq!(decoded, expected);
    }
    let mut truncated = ZstdScreenDecoder::new(128 << 20, None).unwrap();
    truncated.append(&COMPRESSED_SCREEN_FIXTURE[..COMPRESSED_SCREEN_FIXTURE.len() - 1]).unwrap();
    assert_eq!(truncated.finish(), Err(ZstdDecodeError::IncompleteFrame));
    let mut bounded = ZstdScreenDecoder::new(100, None).unwrap();
    let mut failed = false;
    for byte in COMPRESSED_SCREEN_FIXTURE {
        if bounded.append(&[byte]).is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed);
    assert_eq!(bounded.append(&[1]), Err(ZstdDecodeError::ResetRequired));
    bounded.reset().unwrap();
    assert_eq!(bounded.append(&[0x28, 0xb5, 0x2f, 0xfd, 0x20, 0x03, 0x19, 0, 0, 65, 66, 67]).unwrap(), b"ABC");
}

#[test]
fn trained_dictionary_survives_mission_reset() {
    let compressed = b64("KLUv/QdYEL2gPS0BAOhNYWNWaWV3ZXIgZGljdGlvbmFyeSBmaXh0dXJlCgEA0Qz4ygnbOEGJ");
    let expected = "MacViewer dictionary fixture\n".repeat(100).into_bytes();
    let mut missing = ZstdScreenDecoder::new(128 << 20, None).unwrap();
    assert_eq!(missing.append(&compressed), Err(ZstdDecodeError::InvalidData("Dictionary mismatch".into())));
    let mut decoder = ZstdScreenDecoder::new(128 << 20, Some(dictionary::load().unwrap())).unwrap();
    for _ in 0..2 {
        let mut result = Vec::new();
        for byte in &compressed {
            result.extend(decoder.append(&[*byte]).unwrap());
        }
        decoder.finish().unwrap();
        assert_eq!(result, expected);
        decoder.reset().unwrap();
    }
}

#[test]
fn zstd_encoder_round_trip() {
    let input = b"LinuxViewer ".repeat(1000);
    let encoded = uvzstd::encode(&input, 1 << 20).unwrap();
    assert!(encoded.len() < input.len());
    assert_eq!(ZstdScreenDecoder::decode_once(&encoded, 1 << 20).unwrap(), input);
}

// ---------------------------------------------------------------- screen

fn words(values: &[i32]) -> Vec<u8> {
    values.iter().flat_map(|&v| (v as u16).to_le_bytes()).collect()
}

fn long(value: i32) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

fn raw_zstd(bytes: &[u8]) -> Vec<u8> {
    assert!(bytes.len() < 256);
    let block = (bytes.len() << 3) | 1;
    [vec![0x28, 0xb5, 0x2f, 0xfd, 0x20, bytes.len() as u8, block as u8, (block >> 8) as u8, 0], bytes.to_vec()].concat()
}

#[derive(Default)]
struct Parts {
    sequence: i32,
    rectangles: Vec<i32>,
    pixels: Vec<u8>,
    copy: Vec<i32>,
    scroll: Vec<i32>,
    jpeg: Vec<u8>,
    auxiliary: Vec<i32>,
    references: Vec<u8>,
    additions: Vec<u8>,
}

impl Parts {
    fn build(&self) -> Vec<Vec<u8>> {
        vec![
            vec![1],
            words(&self.rectangles),
            if self.pixels.is_empty() { vec![] } else { raw_zstd(&self.pixels) },
            vec![],
            long(if self.sequence == 0 { 1 } else { self.sequence }),
            words(&self.copy),
            words(&self.scroll),
            self.jpeg.clone(),
            words(&self.auxiliary),
            self.references.clone(),
            self.additions.clone(),
        ]
    }

    fn message(&self) -> ModernScreenFrameMessage {
        ModernScreenFrameMessage::decode(&self.build()).unwrap()
    }
}

const JPEG_FIXTURE: &str = "/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQH/2wBDAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQH/wAARCAACAAIDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD8KP8Ahr79rP8A6Oh/aJ/8PZ8Sv/mmooor+fz6xdPl/wC4z//Z";

fn renderer(width: usize, height: usize, tile: usize) -> ModernScreenRenderer {
    ModernScreenRenderer::new(width, height, tile, 0, 0, ModernScreenLimits::default(), None).unwrap()
}

#[test]
fn screen_parts_signedness_and_positions() {
    let parts = Parts {
        sequence: -7,
        rectangles: vec![65_535, 2, 3, 4],
        copy: vec![-1, -2, 3, 4],
        scroll: vec![-5, -6, 7, 8, 9, 10, 11, 12, 13],
        jpeg: vec![0xaa],
        auxiliary: vec![-14, 15, 16, 17],
        references: [words(&[-18, 19]), long(-20)].concat(),
        additions: [words(&[-21, 22, 23, 24]), long(-25)].concat(),
        ..Default::default()
    };
    let message = parts.message();
    assert_eq!(message.frame_number, -7);
    assert_eq!(message.main_rectangles, vec![ScreenRegion::new(65_535, 2, 3, 4)]);
    assert_eq!(message.copy_from_temporary, vec![ScreenRegion::new(-1, -2, 3, 4)]);
    let scroll = message.scroll.unwrap();
    assert_eq!(scroll.vertical_delta, -5);
    assert_eq!(scroll.capture, ScreenRegion::new(-6, 7, 8, 9));
    assert_eq!(scroll.copy_back, vec![ScreenRegion::new(10, 11, 12, 13)]);
    assert_eq!(message.cache_references[0].hash, -20);
    assert_eq!(message.cache_additions[0].region, ScreenRegion::new(-21, 22, 23, 24));
}

#[test]
fn screen_initialization_and_malformed_parts() {
    let mut init: Vec<Vec<u8>> = vec![vec![0], long(1920), long(1080)];
    let basic = ModernScreenInitialization::decode(&init).unwrap();
    assert_eq!((basic.width, basic.height, basic.tile_size), (1920, 1080, 16));
    while init.len() < 15 {
        init.push(vec![]);
    }
    init[5] = vec![0];
    init[6] = b"6.6.125".to_vec();
    init[7] = vec![1];
    init[8] = b"monitor".to_vec();
    init[9] = long(-1920);
    init[10] = long(100);
    init[11] = vec![0];
    init[12] = words(&[-1]);
    init[13] = vec![3];
    init[14] = vec![32];
    let full = ModernScreenInitialization::decode(&init).unwrap();
    assert_eq!((full.virtual_left, full.virtual_top, full.tile_size), (Some(-1920), Some(100), 32));
    assert_eq!(ModernScreenInitialization::decode(&init[..9]), Err(ScreenError::MissingPart(9)));
    assert_eq!(ModernScreenInitialization::decode(&init[..7]), Err(ScreenError::MissingPart(7)));
    for index in [5, 7, 11, 12] {
        let mut truncated = init.clone();
        truncated[index] = vec![];
        assert_eq!(ModernScreenInitialization::decode(&truncated), Err(ScreenError::MalformedPart(index)));
    }
    let parts = Parts::default().build();
    assert_eq!(ModernScreenFrameMessage::decode(&parts[..10]), Err(ScreenError::MissingPart(10)));
    for index in [1, 5, 6, 8, 9, 10] {
        let mut parts = Parts::default().build();
        parts[index] = vec![0];
        assert_eq!(ModernScreenFrameMessage::decode(&parts), Err(ScreenError::MalformedPart(index)));
    }
    assert!(ModernScreenRenderer::with_defaults(1, 1, 1, 0).is_err());
    assert_eq!(ModernScreenInitialization::decode(&[vec![0], long(8193), long(1)]), Err(ScreenError::SizeLimitExceeded));
}

#[test]
fn rgb555_expansion_matches_windows_gdi() {
    let raw = hex("00000100020003000400050006000700080009000a000b000c000d000e000f0010001100120013001400150016001700180019001a001b001c001d001e001f00");
    let expected = hex("0000000800001000001800002100002900003100003900004200004a00005200005a00006300006b00007300007b00008400008c00009400009c0000a50000ad0000b50000bd0000c60000ce0000d60000de0000e70000ef0000f70000ff0000");
    let mut r = renderer(32, 1, 16);
    r.apply(&Parts { rectangles: vec![0, 0, 32, 1], pixels: raw, ..Default::default() }.message()).unwrap();
    assert_eq!(r.bitmap().bgr_pixels(), expected);
}

#[test]
fn reset_baseline_accepts_signed_first_frame() {
    assert_eq!(ModernScreenRenderer::RESET_FRAME_NUMBER, 0x8000_0001u32 as i32);
    let mut r = ModernScreenRenderer::with_defaults(1, 1, 0, ModernScreenRenderer::RESET_FRAME_NUMBER).unwrap();
    let mut parts = Parts { rectangles: vec![0, 0, 1, 1], pixels: words(&[0x03e0]), ..Default::default() }.build();
    parts[4] = vec![0x02, 0x00, 0x00, 0x80];
    let first = ModernScreenFrameMessage::decode(&parts).unwrap();
    assert_eq!(first.frame_number, -2_147_483_646);
    r.apply(&first).unwrap();
    assert_eq!(r.bitmap().bgr_pixels(), vec![0, 255, 0]);
    assert_eq!(r.apply(&first), Err(ScreenError::SequenceMismatch { expected: -2_147_483_645, received: -2_147_483_646 }));
}

#[test]
fn raw_rectangles_respect_padded_rows_and_clipping() {
    let mut r = ModernScreenRenderer::new(3, 2, 16, 2, 0, ModernScreenLimits::default(), None).unwrap();
    let raw = words(&[0x7c00, 0x03e0, 0x001f, 0x7fff, 0x0000, 0x7c00]);
    r.apply(&Parts { rectangles: vec![1, 0, 3, 2], pixels: raw, ..Default::default() }.message()).unwrap();
    assert_eq!(r.bitmap().bgr_pixels(), hex("0000000000ff00ff00000000ffffff000000"));
}

#[test]
fn scroll_copies_from_main_before_copy_back() {
    let mut r = renderer(1, 3, 16);
    r.apply(&Parts { rectangles: vec![0, 0, 1, 3], pixels: words(&[0x7c00, 0x03e0, 0x001f]), ..Default::default() }.message()).unwrap();
    r.apply(&Parts { sequence: 2, scroll: vec![1, 0, 0, 1, 3, 0, 0, 1, 2], ..Default::default() }.message()).unwrap();
    assert_eq!(r.bitmap().bgr_pixels(), hex("00ff00ff0000ff0000"));
}

#[test]
fn jpeg_uses_bottom_up_atlas_and_column_major_cursor() {
    let mut r = renderer(4, 1, 1);
    r.apply(&Parts { jpeg: b64(JPEG_FIXTURE), auxiliary: vec![0, 0, 3, 1, 3, 0, 1, 1], ..Default::default() }.message()).unwrap();
    let output = r.bitmap().bgr_pixels();
    // Native flags1026 output: 18363b18363b / a1bfc4a1bfc4. TurboJPEG is the
    // native decoder family, so this port should match exactly.
    let native: [u8; 12] = [24, 54, 59, 161, 191, 196, 24, 54, 59, 161, 191, 196];
    for (actual, expected) in output.iter().zip(native) {
        assert!((i32::from(*actual) - i32::from(expected)).abs() <= 2, "{output:?}");
    }
}

#[test]
fn pipeline_cache_copy_jpeg_main_and_capture_ordering() {
    let mut r = renderer(3, 2, 1);
    r.apply(&Parts {
        rectangles: vec![0, 0, 1, 1],
        pixels: words(&[0x7c00]),
        additions: [words(&[0, 0, 1, 1]), long(10)].concat(),
        ..Default::default()
    }.message()).unwrap();
    r.apply(&Parts {
        sequence: 2,
        rectangles: vec![2, 1, 1, 1],
        pixels: words(&[0x7fff]),
        copy: vec![1, 0, 1, 1],
        jpeg: b64(JPEG_FIXTURE),
        auxiliary: vec![2, 0, 1, 2],
        references: [words(&[1, 0]), long(10)].concat(),
        additions: [words(&[2, 0, 1, 1]), long(20), words(&[2, 1, 1, 1]), long(10)].concat(),
        ..Default::default()
    }.message()).unwrap();
    let second = r.bitmap().bgr_pixels();
    assert_eq!(second[3..6], [0, 0, 255]);
    assert_eq!(second[15..18], [255, 255, 255]);
    let jpeg_pixel = second[6..9].to_vec();
    r.apply(&Parts {
        sequence: 3,
        copy: vec![0, 1, 1, 1, 1, 1, 1, 1],
        references: [words(&[0, 1]), long(20), words(&[1, 1]), long(10)].concat(),
        ..Default::default()
    }.message()).unwrap();
    let third = r.bitmap().bgr_pixels();
    let quantized: Vec<u8> = jpeg_pixel.iter().map(|c| (c & 0xf8) | (c >> 5)).collect();
    assert_eq!(third[9..12].to_vec(), quantized);
    assert_eq!(third[12..15], [0, 0, 255]);
    assert_eq!((r.cache_entry_count(), r.cache_byte_count()), (2, 4));
}

#[test]
fn cache_thousand_entry_limit_and_recency() {
    let mut r = renderer(1, 1, 16);
    let mut additions = Vec::new();
    for hash in 0..1000 {
        additions.extend(words(&[0, 0, 1, 1]));
        additions.extend(long(hash));
    }
    r.apply(&Parts { additions, ..Default::default() }.message()).unwrap();
    assert_eq!(r.cache_entry_count(), 1000);
    r.apply(&Parts {
        sequence: 2,
        references: [words(&[0, 0]), long(0)].concat(),
        additions: [words(&[0, 0, 1, 1]), long(1000)].concat(),
        ..Default::default()
    }.message()).unwrap();
    assert_eq!(r.cache_entry_count(), 1000);
    let evicted = Parts { sequence: 3, references: [words(&[0, 0]), long(1)].concat(), ..Default::default() }.message();
    assert_eq!(r.apply(&evicted), Err(ScreenError::CacheMiss(1)));
    assert_eq!(r.last_frame_number(), 2);
    r.apply(&Parts { sequence: 3, references: [words(&[0, 0]), long(0)].concat(), ..Default::default() }.message()).unwrap();
}

#[test]
fn screen_failures_are_atomic() {
    let limits = ModernScreenLimits { maximum_cache_bytes: 2, ..Default::default() };
    let mut r = ModernScreenRenderer::new(2, 1, 16, 0, 0, limits, None).unwrap();
    let initial = r.bitmap();
    let oversized = Parts {
        rectangles: vec![0, 0, 2, 1],
        pixels: words(&[0x7c00, 0x03e0]),
        additions: [words(&[0, 0, 1, 1]), long(1), words(&[1, 0, 1, 1]), long(2)].concat(),
        ..Default::default()
    }.message();
    assert_eq!(r.apply(&oversized), Err(ScreenError::CacheBudgetExceeded));
    assert_eq!(r.bitmap(), initial);
    assert_eq!((r.cache_entry_count(), r.last_frame_number()), (0, 0));
    let mismatch = Parts { rectangles: vec![0, 0, 2, 1], pixels: words(&[0x001f]), ..Default::default() }.message();
    assert_eq!(r.apply(&mismatch), Err(ScreenError::DecompressedLengthMismatch { expected: 4, received: 2 }));
    let invalid_jpeg = Parts { jpeg: vec![1, 2, 3], auxiliary: vec![0, 0, 1, 1], ..Default::default() }.message();
    assert_eq!(r.apply(&invalid_jpeg), Err(ScreenError::InvalidJpeg));
    let wrong_sequence = Parts { sequence: 2, ..Default::default() }.message();
    assert_eq!(r.apply(&wrong_sequence), Err(ScreenError::SequenceMismatch { expected: 1, received: 2 }));
    assert_eq!(r.bitmap(), initial);
    r.apply(&Parts { rectangles: vec![0, 0, 1, 1], pixels: words(&[0x001f]), ..Default::default() }.message()).unwrap();
    assert_eq!(r.bitmap().bgr_pixels(), hex("ff0000000000"));
}

#[test]
fn auxiliary_bounds_and_tile_size_updates() {
    // A 2 × 2 atlas cannot hold 25 tiles: far more than a rounded-down tail.
    let mut r = renderer(5, 5, 1);
    let overflow = Parts { jpeg: b64(JPEG_FIXTURE), auxiliary: vec![0, 0, 5, 5], ..Default::default() }.message();
    assert!(matches!(r.apply(&overflow), Err(ScreenError::AuxiliaryAtlasOutOfBounds { atlas_width: 2, atlas_height: 2, tile_size: 1, tiles: 25, .. })));
    assert_eq!(r.last_frame_number(), 0);
    r.set_tile_size(&[vec![48], vec![2]]).unwrap();
    assert_eq!(r.tile_size(), 2);
    assert_eq!(r.set_tile_size(&[vec![48], vec![0]]), Err(ScreenError::MalformedPart(1)));
}

/// 8 × 16 JPEG: upper half red, lower half blue (two 8 × 8 tiles).
const TWO_TILE_ATLAS: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQH/2wBDAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQH/wAARCAAQAAgDAREAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD8X6/ynP8Av4PyHr/1UD/zXz//2Q==";

#[test]
fn atlas_tail_tiles_outside_a_short_atlas_are_skipped() {
    // Windows rounds a multi-column atlas height down, losing its last tiles.
    // Three 8 × 8 tiles, but the atlas holds only two; the third keeps its pixels.
    let mut r = renderer(8, 24, 8);
    r.apply(&Parts { jpeg: b64(TWO_TILE_ATLAS), auxiliary: vec![0, 0, 8, 24], ..Default::default() }.message()).unwrap();
    let bgr = r.bitmap().bgr_pixels();
    let pixel = |y: usize| &bgr[y * 8 * 3..y * 8 * 3 + 3];
    // Bottom-up addressing: the first tile is the image's lower (blue) half.
    assert!(pixel(0)[0] > 240 && pixel(0)[2] < 16, "first tile {:?}", pixel(0));
    assert!(pixel(8)[2] > 240 && pixel(8)[0] < 16, "second tile {:?}", pixel(8));
    assert_eq!(pixel(16), &[0, 0, 0]);
    assert_eq!(r.last_frame_number(), 1);
}

#[test]
fn atlas_has_independent_budgets() {
    let limits = ModernScreenLimits::default();
    limits.check_atlas_geometry(32, 19_904).unwrap();
    assert_eq!(limits.check_geometry(32, 19_904), Err(ScreenError::SizeLimitExceeded));
    assert_eq!(limits.check_atlas_geometry(32, 65_536), Err(ScreenError::SizeLimitExceeded));
    assert_eq!(limits.check_atlas_geometry(0, 100), Err(ScreenError::InvalidGeometry));
}

// ---------------------------------------------------------------- audio

#[test]
fn g722_wraps_like_naudio() {
    let input: Vec<u8> = (0..=38).collect();
    let pcm = G722Decoder::new().unwrap().decode(&input).unwrap();
    assert_eq!(i16::from_le_bytes([pcm[152], pcm[153]]), -28446);
}

#[test]
fn g722_matches_windows_fixture() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/g722-windows.json");
    let envelope: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let fixtures = envelope["Fixtures"].as_array().unwrap();
    assert!(!fixtures.is_empty());
    for fixture in fixtures {
        let encoded = b64(fixture["EncodedBase64"].as_str().unwrap());
        let expected = b64(fixture["PCMBase64"].as_str().unwrap());
        assert_eq!(G722Decoder::new().unwrap().decode(&encoded).unwrap(), expected);
        let mut streaming = G722Decoder::new().unwrap();
        let mut combined = Vec::new();
        for chunk in fixture["SplitChunks"].as_array().unwrap() {
            let offset = chunk["EncodedOffset"].as_u64().unwrap() as usize;
            let length = chunk["EncodedLength"].as_u64().unwrap() as usize;
            let actual = streaming.decode(&encoded[offset..offset + length]).unwrap();
            assert_eq!(actual, b64(chunk["PCMBase64"].as_str().unwrap()));
            combined.extend(actual);
        }
        assert_eq!(combined, expected);
    }
}

#[test]
fn g722_stream_state_spans_packets() {
    let first: Vec<u8> = (0..=255).collect();
    let second: Vec<u8> = first.iter().rev().copied().collect();
    let mut streaming = G722Decoder::new().unwrap();
    let mut pcm = streaming.decode(&first).unwrap();
    assert!(streaming.decode(&[]).unwrap().is_empty());
    pcm.extend(streaming.decode(&second).unwrap());
    let complete = G722Decoder::new().unwrap().decode(&[first, second].concat()).unwrap();
    assert_eq!(pcm, complete);
}

#[test]
fn audio_messages() {
    use ultraviewer_protocol::ComputerAudioMessage;
    let encoded = vec![0, 0xff, 0x80, 42, 43];
    assert_eq!(ComputerAudioMessage::decode(&[vec![43], encoded.clone()]).unwrap(), ComputerAudioMessage::Encoded(encoded));
    assert_eq!(ComputerAudioMessage::decode(&[vec![42], vec![0x00, 0x4b, 0, 0]]).unwrap(), ComputerAudioMessage::Silence { pcm_byte_count: 19_200 });
    assert!(ComputerAudioMessage::decode(&[vec![42], vec![0xff; 4]]).is_err());
    assert!(ComputerAudioMessage::decode(&[vec![41], vec![]]).is_err());
}
