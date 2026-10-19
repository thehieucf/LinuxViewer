//! A headless viewer for testing hosts: connects to an UltraViewer ID,
//! reports frames, chats, and optionally sends one pointer move.
//!
//! Run with: VIEWER_MAC=02A1B2C3D4E5 cargo run -p ultraviewer-protocol --example viewer_demo -- <id> <password>
//! Set VIEWER_MOVE=x,y to send a pointer move after the first frame.

use std::time::Duration;

use md5::{Digest, Md5};
use rand::Rng;
use ultraviewer_protocol::input::MouseMessage;
use ultraviewer_protocol::registration::ClientRegistrationRequest;
use ultraviewer_protocol::session::{self, SessionConfiguration, SessionEvent};
use ultraviewer_protocol::RelayIPv4Address;

fn primary_ipv4() -> RelayIPv4Address {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    socket.connect("1.1.1.1:80").unwrap();
    match socket.local_addr().unwrap().ip() {
        std::net::IpAddr::V4(v4) => RelayIPv4Address::from_octets(v4.octets()),
        _ => panic!("no IPv4"),
    }
}

fn registration(mac: String) -> ClientRegistrationRequest {
    let mut rng = rand::thread_rng();
    let identity: [u8; 16] = rng.gen();
    ClientRegistrationRequest {
        device_identity: Md5::digest(identity).iter().map(|b| format!("{b:02X}")).collect(),
        password: (0..8).map(|_| char::from(b'0' + rng.gen_range(0..10))).collect(),
        local_address: primary_ipv4(),
        computer_name_utf8_base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, "viewer-demo"),
        client_version: ultraviewer_protocol::CLIENT_VERSION.into(),
        mac_address: mac,
        disk_identity: String::new(),
        operating_system: "0|6|0|0|0|0".into(),
        installer_referral: 0,
        custom_password: String::new(),
        uses_proxy: false,
        quick_support_metadata: String::new(),
        user_name_wire_value: "demo".into(),
        active_session_id: 0,
        process_session_id: 0,
        preferred_id_text: "0".into(),
        referral_id_text: "0".into(),
        automatic_update: 0,
        system_folder_creation_ticks: "0".into(),
        program_files_creation_ticks: "0".into(),
        system_volume_information_creation_ticks: "0".into(),
        license_context1: String::new(),
        license_context2: String::new(),
        captcha_token: String::new(),
    }
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().collect();
    let (id, password) = (args[1].clone(), args[2].clone());
    let seconds: u64 = std::env::var("VIEWER_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
    let movement = std::env::var("VIEWER_MOVE").ok().and_then(|text| {
        let (x, y) = text.split_once(',')?;
        Some((x.parse::<i32>().ok()?, y.parse::<i32>().ok()?))
    });
    let mac = std::env::var("VIEWER_MAC").unwrap_or_else(|_| "02A1B2C3D4E5".into());
    let mut streams = session::connect(SessionConfiguration::new(registration(mac), 1920, 1080, "viewer-demo"), &id, &password).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut frames = 0;
    let mut moved = false;
    loop {
        tokio::select! {
            event = streams.events.recv() => match event {
                Some(SessionEvent::Accepted(peer)) => {
                    println!("ACCEPTED {}x{} version {} input={} name={:?}", peer.width, peer.height, peer.version, peer.allows_remote_input, peer.display_name);
                    let _ = streams.handle.send_chat("viewer-demo xin chào".into()).await;
                }
                Some(SessionEvent::Closed(error)) => { println!("CLOSED {error:?}"); break; }
                Some(SessionEvent::ComputerAudio(_)) => {}
                Some(other) => println!("EVENT {other:?}"),
                None => break,
            },
            changed = streams.frames.changed() => {
                if changed.is_err() { break; }
                let update = streams.frames.borrow_and_update().clone();
                if let Some(update) = update {
                    frames += 1;
                    if frames <= 3 || frames % 10 == 0 {
                        let b = &update.bitmap;
                        let center = ((b.height / 2) * b.width + b.width / 2) * 4;
                        println!("FRAME #{frames} {}x{} center BGRA {:?}", b.width, b.height, &b.bgra[center..center + 4]);
                    }
                    if let (Some((x, y)), false) = (movement, moved) {
                        moved = true;
                        let result = streams.handle.send(MouseMessage::Move { w_param: 0, x, y }.encode()).await;
                        println!("MOVE sent to {x},{y}: {result:?}");
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => { println!("DONE frames={frames}"); streams.handle.close().await; break; }
        }
    }
}
