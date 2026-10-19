//! A synthetic UltraViewer host for testing viewers: registers with the
//! public service, auto-approves one viewer with control, streams an animated
//! test pattern, logs decoded input and echoes chat/clipboard.
//!
//! Run with: HOST_MAC=02AABBCCDDEE cargo run -p ultraviewer-protocol --example host_demo
//! (a distinct hardware address keeps its ID separate from this computer's;
//! HOST_PASSWORD fixes the otherwise random password).

use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use rand::Rng;
use ultraviewer_protocol::host::{self, HostConfiguration, HostEvent, HostedScreenFrame};
use ultraviewer_protocol::input::HostInputMessage;
use ultraviewer_protocol::registration::ClientRegistrationRequest;
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
        password: std::env::var("HOST_PASSWORD")
            .unwrap_or_else(|_| (0..8).map(|_| char::from(b'0' + rng.gen_range(0..10))).collect()),
        local_address: primary_ipv4(),
        computer_name_utf8_base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, "Máy thử nghiệm"),
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

/// Moving color bars plus a cursor cross-hair at the last pointer position.
fn frame(started: Instant, pointer: (i32, i32)) -> HostedScreenFrame {
    let (width, height) = (1280usize, 720usize);
    let t = (started.elapsed().as_millis() / 40) as usize;
    let mut bgra = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let p = (y * width + x) * 4;
            let band = ((x + t * 4) / 160) % 6;
            let (r, g, b) = [(220, 60, 60), (60, 200, 90), (60, 110, 230), (240, 200, 40), (160, 70, 200), (40, 190, 200)][band];
            let shade = 80 + (y * 175 / height) as u32;
            bgra[p] = (b * shade / 255) as u8;
            bgra[p + 1] = (g * shade / 255) as u8;
            bgra[p + 2] = (r * shade / 255) as u8;
            bgra[p + 3] = 255;
        }
    }
    let (px, py) = (pointer.0.clamp(0, width as i32 - 1) as usize, pointer.1.clamp(0, height as i32 - 1) as usize);
    for d in 0..width {
        let p = (py * width + d) * 4;
        bgra[p..p + 3].copy_from_slice(&[255, 255, 255]);
    }
    for d in 0..height {
        let p = (d * width + px) * 4;
        bgra[p..p + 3].copy_from_slice(&[255, 255, 255]);
    }
    HostedScreenFrame::from_bgra_top_down(width, height, width * 4, &bgra).unwrap()
}

/// A 32×32 32-bit CUR file: a red arrow with a white outline.
fn cursor_file() -> Vec<u8> {
    let (width, height) = (32usize, 32usize);
    let mut pixels = vec![0u8; width * height * 4];
    for y in 0..20 {
        for x in 0..=y.min(12) {
            let edge = x == 0 || x == y.min(12) || y == 19;
            let p = ((height - 1 - y) * width + x) * 4; // bottom-up rows
            pixels[p..p + 4].copy_from_slice(if edge { &[255, 255, 255, 255] } else { &[40, 40, 230, 255] });
        }
    }
    let mut info = Vec::new();
    for value in [40u32, width as u32, (height * 2) as u32] {
        info.extend_from_slice(&value.to_le_bytes());
    }
    info.extend_from_slice(&1u16.to_le_bytes());
    info.extend_from_slice(&32u16.to_le_bytes());
    info.extend_from_slice(&[0u8; 24]);
    let mask = vec![0u8; height * 4];
    let resource = [info, pixels, mask].concat();
    let mut file = vec![0, 0, 2, 0, 1, 0, width as u8, height as u8, 0, 0];
    file.extend_from_slice(&1u16.to_le_bytes()); // hotspot x
    file.extend_from_slice(&1u16.to_le_bytes()); // hotspot y
    file.extend_from_slice(&(resource.len() as u32).to_le_bytes());
    file.extend_from_slice(&22u32.to_le_bytes());
    file.extend(resource);
    file
}

fn cursor_shape(hash: i32) -> Vec<u8> {
    let file = cursor_file();
    let compressed = ultraviewer_protocol::zstd::encode(&file, 1 << 20).unwrap();
    ultraviewer_protocol::multipart::encode(&[vec![4u8], hash.to_le_bytes().to_vec(), (file.len() as i32).to_le_bytes().to_vec(), compressed]).unwrap()
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mac = std::env::var("HOST_MAC").unwrap_or_else(|_| "02AABBCCDDEE".into());
    let registration = registration(mac);
    let password = registration.password.clone();
    let mut hosting = host::start(HostConfiguration::new(registration)).unwrap();
    let handle = hosting.handle.clone();
    let started = Instant::now();
    let mut pointer = (640, 360);
    let mut viewer = None;
    let mut audio = tokio::time::interval(Duration::from_millis(100));
    let deadline = std::env::var("HOST_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(600u64);
    let stop_at = tokio::time::Instant::now() + Duration::from_secs(deadline);
    loop {
        let event = tokio::select! {
            event = hosting.events.recv() => event,
            _ = audio.tick() => {
                // 100 ms of 48 kHz stereo: alternate silence and G.722 noise.
                if let Some(session) = viewer {
                    let packet = if started.elapsed().as_secs() % 2 == 0 {
                        ultraviewer_protocol::multipart::encode(&[vec![42u8], 19_200i32.to_le_bytes().to_vec()]).unwrap()
                    } else {
                        let encoded: Vec<u8> = (0..4800).map(|i| if i % 2 == 0 { 0xfa } else { 0x7d }).collect();
                        ultraviewer_protocol::multipart::encode(&[vec![43u8], encoded]).unwrap()
                    };
                    let _ = handle.send_payload(packet, session).await;
                }
                continue;
            }
            _ = tokio::time::sleep_until(stop_at) => { handle.stop().await; break; }
        };
        let Some(event) = event else { break };
        match event {
            HostEvent::Registered(id) => println!("HOST READY id={id} password={password}"),
            HostEvent::Request(request) => {
                println!("REQUEST from {} {:?}", request.viewer_id, request.display_name);
                handle.approve(request.id, frame(started, pointer), true).await.unwrap();
            }
            HostEvent::Connected(request) => {
                println!("CONNECTED {}", request.viewer_id);
                viewer = Some(request.id);
                let _ = handle.send_payload(cursor_shape(777), request.id).await;
                let select = ultraviewer_protocol::multipart::encode(&[vec![3u8], 777i32.to_le_bytes().to_vec()]).unwrap();
                let _ = handle.send_payload(select, request.id).await;
                let _ = handle.set_clipboard_sharing(true, request.id).await;
                let _ = handle.send_chat("Xin chào từ máy thử nghiệm 👋".into(), request.id).await;
            }
            HostEvent::CaptureRequested(ticket) => handle.submit_frame(frame(started, pointer), ticket),
            HostEvent::Input { payload, .. } => {
                let decoded = HostInputMessage::decode(&payload);
                if let Ok(Some(HostInputMessage::Mouse(ultraviewer_protocol::MouseMessage::Move { x, y, .. }))) = decoded {
                    pointer = (x, y);
                    if let Some(session) = viewer {
                        let position = ultraviewer_protocol::multipart::encode(&[vec![28u8], (x as i16).to_le_bytes().to_vec(), (y as i16).to_le_bytes().to_vec()]).unwrap();
                        let _ = handle.send_payload(position, session).await;
                    }
                }
                println!("INPUT {decoded:?}");
            }
            HostEvent::Chat { session, text } => {
                println!("CHAT {text}");
                let _ = handle.send_chat(format!("echo: {text}"), session).await;
                if text == "clip" {
                    let _ = handle.send_clipboard("Văn bản từ máy thử nghiệm".into(), session).await;
                }
            }
            HostEvent::Clipboard { text, .. } => println!("CLIPBOARD {text}"),
            HostEvent::ViewerEnded(reason) => {
                viewer = None;
                println!("VIEWER ENDED {reason:?}");
            }
            HostEvent::Stopped(error) => {
                println!("STOPPED {error:?}");
                break;
            }
            other => println!("{other:?}"),
        }
    }
}
