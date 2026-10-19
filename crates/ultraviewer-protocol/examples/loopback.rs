//! End-to-end check through the public UltraViewer service: a host registers
//! and shares a synthetic screen; a viewer on the same machine connects to
//! that ID with its password, receives frames, sends input and chats.
//!
//! Run with: cargo run -p ultraviewer-protocol --example loopback

use std::net::UdpSocket;
use std::time::Duration;

use md5::{Digest, Md5};
use rand::Rng;
use ultraviewer_protocol::host::{self, HostConfiguration, HostEvent, HostedScreenFrame};
use ultraviewer_protocol::input::{HostInputMessage, MouseMessage};
use ultraviewer_protocol::registration::ClientRegistrationRequest;
use ultraviewer_protocol::session::{self, SessionConfiguration, SessionEvent};
use ultraviewer_protocol::RelayIPv4Address;

fn primary_ipv4() -> RelayIPv4Address {
    // Connecting a UDP socket selects a route without sending packets.
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    socket.connect("1.1.1.1:80").unwrap();
    match socket.local_addr().unwrap().ip() {
        std::net::IpAddr::V4(v4) => RelayIPv4Address::from_octets(v4.octets()),
        _ => panic!("no IPv4"),
    }
}

fn hardware_address() -> String {
    for entry in std::fs::read_dir("/sys/class/net").unwrap().flatten() {
        if entry.file_name() == "lo" {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(entry.path().join("address")) {
            let hex: String = text.trim().split(':').collect::<String>().to_uppercase();
            if hex.len() == 12 && hex != "000000000000" {
                return hex;
            }
        }
    }
    panic!("no MAC address")
}

fn registration(label: &str, mac: String) -> ClientRegistrationRequest {
    let mut rng = rand::thread_rng();
    let identity: [u8; 16] = rng.gen();
    ClientRegistrationRequest {
        device_identity: Md5::digest(identity).iter().map(|b| format!("{b:02X}")).collect(),
        password: (0..8).map(|_| char::from(b'0' + rng.gen_range(0..10))).collect(),
        local_address: primary_ipv4(),
        computer_name_utf8_base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, label),
        client_version: ultraviewer_protocol::CLIENT_VERSION.into(),
        mac_address: mac,
        disk_identity: String::new(),
        operating_system: "0|6|0|0|0|0".into(),
        installer_referral: 0,
        custom_password: String::new(),
        uses_proxy: false,
        quick_support_metadata: String::new(),
        user_name_wire_value: "linuxviewer".into(),
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

fn synthetic_frame(phase: u8) -> HostedScreenFrame {
    let (width, height) = (320usize, 200usize);
    let mut bgra = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let p = (y * width + x) * 4;
            bgra[p] = (x as u8).wrapping_add(phase);
            bgra[p + 1] = y as u8;
            bgra[p + 2] = phase;
            bgra[p + 3] = 255;
        }
    }
    HostedScreenFrame::from_bgra_top_down(width, height, width * 4, &bgra).unwrap()
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    let host_registration = registration("LinuxViewer-host", hardware_address());
    let password = host_registration.password.clone();
    let mut hosting = host::start(HostConfiguration::new(host_registration)).unwrap();
    let id = loop {
        match tokio::time::timeout(Duration::from_secs(30), hosting.events.recv()).await.expect("host registration timeout") {
            Some(HostEvent::Registered(id)) => break id,
            Some(HostEvent::Status(status)) => println!("[host] {status}"),
            Some(HostEvent::Stopped(error)) => panic!("host stopped: {error:?}"),
            other => println!("[host] {other:?}"),
        }
    };
    println!("[host] registered as {id}");
    tokio::time::sleep(Duration::from_secs(3)).await;

    let mut viewer = session::connect(SessionConfiguration::new(registration("LinuxViewer-viewer", std::env::var("VIEWER_MAC").unwrap_or_else(|_| hardware_address())), 1920, 1080, "LinuxViewer"), &id, &password).unwrap();
    let host_handle = hosting.handle.clone();
    let host_task = tokio::spawn(async move {
        let mut frames = 0u8;
        let mut inputs = 0;
        while let Some(event) = hosting.events.recv().await {
            match event {
                HostEvent::Request(request) => {
                    println!("[host] incoming request from {} ({:?}); approving with control", request.viewer_id, request.display_name);
                    host_handle.approve(request.id, synthetic_frame(0), true).await.unwrap();
                }
                HostEvent::Connected(request) => println!("[host] viewer {} is streaming", request.viewer_id),
                HostEvent::CaptureRequested(ticket) => {
                    frames = frames.wrapping_add(1);
                    host_handle.submit_frame(synthetic_frame(frames.wrapping_mul(16)), ticket);
                }
                HostEvent::Input { payload, .. } => {
                    inputs += 1;
                    println!("[host] input {:?}", HostInputMessage::decode(&payload));
                }
                HostEvent::Chat { session, text } => {
                    println!("[host] chat from viewer: {text}");
                    host_handle.send_chat(format!("echo: {text}"), session).await.unwrap();
                }
                HostEvent::ViewerEnded(reason) => {
                    println!("[host] viewer ended: {reason:?}");
                    break;
                }
                HostEvent::Stopped(error) => {
                    println!("[host] stopped: {error:?}");
                    break;
                }
                other => println!("[host] {other:?}"),
            }
        }
        inputs
    });

    let mut accepted = false;
    let mut frames_seen = 0;
    let mut chat_echo = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        tokio::select! {
            event = viewer.events.recv() => match event {
                Some(SessionEvent::Accepted(peer)) => {
                    println!("[viewer] accepted: {}x{} version {} monitors {:?}", peer.width, peer.height, peer.version, peer.monitors);
                    accepted = true;
                    viewer.handle.send(MouseMessage::Move { w_param: 0, x: 10, y: 20 }.encode()).await.unwrap();
                    viewer.handle.send_chat("xin chào từ LinuxViewer".into()).await.unwrap();
                }
                Some(SessionEvent::Chat(text)) => {
                    println!("[viewer] chat: {text}");
                    chat_echo = true;
                }
                Some(SessionEvent::Closed(error)) => {
                    println!("[viewer] closed: {error:?}");
                    break;
                }
                Some(other) => println!("[viewer] {other:?}"),
                None => break,
            },
            changed = viewer.frames.changed() => {
                if changed.is_err() { break; }
                let update = viewer.frames.borrow_and_update().clone();
                if let Some(update) = update {
                    frames_seen += 1;
                    let pixel = &update.bitmap.bgra[..4];
                    println!("[viewer] frame #{} {}x{} first pixel {:?} damage {:?}", update.serial, update.bitmap.width, update.bitmap.height, pixel, update.damage);
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                println!("[viewer] deadline reached");
                break;
            }
        }
        if accepted && frames_seen >= 4 && chat_echo {
            println!("[viewer] success; disconnecting");
            viewer.handle.close().await;
            break;
        }
    }
    let inputs = tokio::time::timeout(Duration::from_secs(10), host_task).await.ok().and_then(Result::ok).unwrap_or(0);
    println!("RESULT accepted={accepted} frames={frames_seen} chat_echo={chat_echo} host_inputs={inputs}");
}
