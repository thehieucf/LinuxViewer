//! Screen capture for sharing this computer.
//!
//! Wayland (and X11) desktops: the xdg-desktop-portal ScreenCast interface,
//! optionally inside a RemoteDesktop session so the same portal also injects
//! input. Frames arrive on a PipeWire stream. Plain X11 sessions can also be
//! captured directly when no portal is available.

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions, StartOptions};
use ashpd::desktop::screencast::{CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType, StartCastOptions};
use ashpd::desktop::{CreateSessionOptions, PersistMode, Session};
use ultraviewer_protocol::host::HostedScreenFrame;

use crate::i18n::{tr, trf};

/// Byte offsets of blue, green and red within a 4-byte pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub blue: usize,
    pub green: usize,
    pub red: usize,
}

impl Layout {
    pub const BGRX: Self = Self { blue: 0, green: 1, red: 2 };
    pub const RGBX: Self = Self { blue: 2, green: 1, red: 0 };
    pub const XRGB: Self = Self { blue: 3, green: 2, red: 1 };
    pub const XBGR: Self = Self { blue: 1, green: 2, red: 3 };
}

/// One raw captured frame, top-down.
#[derive(Clone)]
pub struct RawFrame {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub layout: Layout,
    pub data: Arc<Vec<u8>>,
}

/// Largest frame sent to the viewer, matching the macOS host.
pub const MAXIMUM_WIDTH: usize = 1920;
pub const MAXIMUM_HEIGHT: usize = 1080;

pub fn scaled_size(width: usize, height: usize) -> (usize, usize) {
    let scale = (MAXIMUM_WIDTH as f64 / width as f64).min(MAXIMUM_HEIGHT as f64 / height as f64).min(1.0);
    (((width as f64 * scale) as usize).max(1), ((height as f64 * scale) as usize).max(1))
}

/// Box-filter downscale into RGB555 bottom-up rows.
pub fn convert(frame: &RawFrame) -> Option<HostedScreenFrame> {
    if frame.width == 0 || frame.height == 0 || frame.stride < frame.width * 4 || frame.data.len() < frame.stride * (frame.height - 1) + frame.width * 4 {
        return None;
    }
    let (width, height) = scaled_size(frame.width, frame.height);
    let mut out = vec![0u8; width * height * 2];
    let layout = frame.layout;
    let data = &frame.data[..];
    if width == frame.width && height == frame.height {
        for y in 0..height {
            let row = &data[y * frame.stride..y * frame.stride + width * 4];
            let target = (height - 1 - y) * width * 2;
            for (x, pixel) in row.chunks_exact(4).enumerate() {
                let word = (u16::from(pixel[layout.red] >> 3) << 10) | (u16::from(pixel[layout.green] >> 3) << 5) | u16::from(pixel[layout.blue] >> 3);
                out[target + x * 2..target + x * 2 + 2].copy_from_slice(&word.to_le_bytes());
            }
        }
    } else {
        let x_bounds: Vec<(usize, usize)> = (0..width).map(|x| (x * frame.width / width, ((x + 1) * frame.width / width).max(x * frame.width / width + 1))).collect();
        for y in 0..height {
            let y0 = y * frame.height / height;
            let y1 = ((y + 1) * frame.height / height).max(y0 + 1);
            let target = (height - 1 - y) * width * 2;
            for (x, &(x0, x1)) in x_bounds.iter().enumerate() {
                let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
                for sy in y0..y1 {
                    let row = sy * frame.stride;
                    for sx in x0..x1 {
                        let p = row + sx * 4;
                        r += u32::from(data[p + layout.red]);
                        g += u32::from(data[p + layout.green]);
                        b += u32::from(data[p + layout.blue]);
                        n += 1;
                    }
                }
                let word = (((r / n) as u16 >> 3) << 10) | (((g / n) as u16 >> 3) << 5) | ((b / n) as u16 >> 3);
                out[target + x * 2..target + x * 2 + 2].copy_from_slice(&word.to_le_bytes());
            }
        }
    }
    Some(HostedScreenFrame { width, height, rgb555_bottom_up: out })
}

/// Shared state written by the PipeWire thread.
#[derive(Default)]
pub struct StreamState {
    pub latest: Mutex<Option<RawFrame>>,
    pub failed: Mutex<Option<String>>,
    pub stop: AtomicBool,
}

/// What the portal granted for this sharing session.
pub struct PortalSession {
    pub state: Arc<StreamState>,
    /// Stream node and logical position/size in the desktop layout.
    pub node_id: u32,
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
    pub remote_desktop: Option<(RemoteDesktop, Session<RemoteDesktop>)>,
    screencast_session: Option<Session<Screencast>>,
    stopper: Mutex<Option<pipewire::channel::Sender<()>>>,
}

impl PortalSession {
    pub fn has_input(&self) -> bool {
        self.remote_desktop.is_some()
    }

    pub async fn close(&self) {
        self.state.stop.store(true, Ordering::SeqCst);
        if let Some(stopper) = self.stopper.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = stopper.send(());
        }
        if let Some((_, session)) = &self.remote_desktop {
            let _ = session.close().await;
        }
        if let Some(session) = &self.screencast_session {
            let _ = session.close().await;
        }
    }
}

fn portal_error(error: ashpd::Error) -> String {
    match error {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => tr("Screen sharing was cancelled."),
        other => trf("Screen sharing is unavailable: {}", &[&other.to_string()]),
    }
}

/// Asks the desktop for a monitor to share (the portal shows its own
/// dialog). With `want_input`, a RemoteDesktop session is tried first so the
/// portal also grants keyboard and pointer control.
pub async fn start_portal(want_input: bool, restore_token: Option<String>) -> Result<(PortalSession, Option<String>), String> {
    let screencast = Screencast::new().await.map_err(portal_error)?;
    let cursor = match screencast.available_cursor_modes().await {
        Ok(modes) if modes.contains(CursorMode::Embedded) => CursorMode::Embedded,
        _ => CursorMode::Hidden,
    };
    // A restore token lets the desktop skip its picker for the same screen.
    let token = restore_token.clone();
    let sources = || {
        SelectSourcesOptions::default()
            .set_cursor_mode(cursor)
            .set_sources(enumflags2::BitFlags::from(SourceType::Monitor))
            .set_multiple(false)
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(token.as_deref())
    };

    if want_input {
        if let Ok(remote) = RemoteDesktop::new().await {
            let devices_available = remote.available_device_types().await.unwrap_or_default();
            if devices_available.contains(DeviceType::Keyboard) && devices_available.contains(DeviceType::Pointer) {
                let attempt = async {
                    let session = remote.create_session(CreateSessionOptions::default()).await?;
                    remote
                        .select_devices(
                            &session,
                            SelectDevicesOptions::default()
                                .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
                                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                                .set_restore_token(restore_token.as_deref()),
                        )
                        .await?
                        .response()?;
                    screencast.select_sources(&session, sources()).await?.response()?;
                    let selected = remote.start(&session, None, StartOptions::default()).await?.response()?;
                    let stream = selected.streams().first().cloned().ok_or(ashpd::Error::NoResponse)?;
                    let new_token = selected.restore_token().map(str::to_string);
                    let fd = screencast.open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default()).await?;
                    Ok::<_, ashpd::Error>((session, stream, fd, new_token))
                };
                match attempt.await {
                    Ok((session, stream, fd, new_token)) => {
                        let mut granted = launch(fd, stream.pipe_wire_node_id(), stream.position(), stream.size())?;
                        granted.remote_desktop = Some((remote, session));
                        return Ok((granted, new_token));
                    }
                    Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => {
                        return Err(tr("Screen sharing was cancelled."));
                    }
                    Err(error) => log::info!("RemoteDesktop portal unavailable, using ScreenCast only: {error}"),
                }
            }
        }
    }

    let session = screencast.create_session(CreateSessionOptions::default()).await.map_err(portal_error)?;
    screencast.select_sources(&session, sources()).await.map_err(portal_error)?.response().map_err(portal_error)?;
    let streams = screencast.start(&session, None, StartCastOptions::default()).await.map_err(portal_error)?.response().map_err(portal_error)?;
    let stream = streams.streams().first().cloned().ok_or_else(|| tr("Screen sharing was cancelled."))?;
    let new_token = streams.restore_token().map(str::to_string);
    let fd = screencast.open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default()).await.map_err(portal_error)?;
    let mut granted = launch(fd, stream.pipe_wire_node_id(), stream.position(), stream.size())?;
    granted.screencast_session = Some(session);
    Ok((granted, new_token))
}

fn launch(fd: OwnedFd, node_id: u32, position: Option<(i32, i32)>, size: Option<(i32, i32)>) -> Result<PortalSession, String> {
    let state = Arc::new(StreamState::default());
    let (stopper, receiver) = pipewire::channel::channel::<()>();
    let thread_state = state.clone();
    std::thread::Builder::new()
        .name("linuxviewer-pipewire".into())
        .spawn(move || {
            if let Err(error) = run_stream(fd, node_id, thread_state.clone(), receiver) {
                *thread_state.failed.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(PortalSession { state, node_id, position, size, remote_desktop: None, screencast_session: None, stopper: Mutex::new(Some(stopper)) })
}

struct StreamData {
    format: pipewire::spa::param::video::VideoInfoRaw,
    state: Arc<StreamState>,
}

fn run_stream(fd: OwnedFd, node_id: u32, state: Arc<StreamState>, stop: pipewire::channel::Receiver<()>) -> Result<(), String> {
    use pipewire as pw;
    use pw::spa;
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::param::video::VideoFormat;

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
    let core = context.connect_fd_rc(fd, None).map_err(|e| e.to_string())?;
    let weak_loop = mainloop.downgrade();
    let _stop = stop.attach(mainloop.loop_(), move |()| {
        if let Some(mainloop) = weak_loop.upgrade() {
            mainloop.quit();
        }
    });
    let stream = pw::stream::StreamRc::new(
        core,
        "linuxviewer-share",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|e| e.to_string())?;
    let data = StreamData { format: Default::default(), state: state.clone() };
    let weak_loop = mainloop.downgrade();
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |_, data, _, new| {
            if let pw::stream::StreamState::Error(message) = new {
                *data.state.failed.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
                if let Some(mainloop) = weak_loop.upgrade() {
                    mainloop.quit();
                }
            }
        })
        .param_changed(|_, data, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = spa::param::format_utils::parse_format(param) else { return };
            if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
                return;
            }
            let _ = data.format.parse(param);
        })
        .process(|stream, data| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let layout = match data.format.format() {
                VideoFormat::BGRx | VideoFormat::BGRA => Layout::BGRX,
                VideoFormat::RGBx | VideoFormat::RGBA => Layout::RGBX,
                VideoFormat::xRGB | VideoFormat::ARGB => Layout::XRGB,
                VideoFormat::xBGR | VideoFormat::ABGR => Layout::XBGR,
                _ => return,
            };
            let size = data.format.size();
            let (width, height) = (size.width as usize, size.height as usize);
            let datas = buffer.datas_mut();
            let Some(plane) = datas.first_mut() else { return };
            let chunk = plane.chunk();
            let stride = if chunk.stride() > 0 { chunk.stride() as usize } else { width * 4 };
            let offset = chunk.offset() as usize;
            let chunk_size = chunk.size() as usize;
            let Some(bytes) = plane.data() else { return };
            if width == 0 || height == 0 || chunk_size == 0 || offset + stride * (height - 1) + width * 4 > bytes.len() {
                return;
            }
            let pixels = bytes[offset..offset + (stride * height).min(bytes.len() - offset)].to_vec();
            *data.state.latest.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(RawFrame { width, height, stride, layout, data: Arc::new(pixels) });
        })
        .register()
        .map_err(|e| e.to_string())?;

    let format = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
            VideoFormat::xRGB,
            VideoFormat::ARGB,
            VideoFormat::xBGR,
            VideoFormat::ABGR
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 8192, height: 8192 }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 15, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 60, denom: 1 }
        ),
    );
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(format))
        .map_err(|e| format!("{e:?}"))?
        .0
        .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values).ok_or("invalid format pod")?];
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| e.to_string())?;
    if !state.stop.load(Ordering::SeqCst) {
        mainloop.run();
    }
    let _ = stream.disconnect();
    Ok(())
}

/// Direct capture of the X11 root window (X11 sessions only).
pub mod x11 {
    use super::{Layout, RawFrame};
    use std::sync::Arc;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};
    use x11rb::rust_connection::RustConnection;

    pub struct X11Capture {
        connection: RustConnection,
        root: u32,
    }

    impl X11Capture {
        pub fn open() -> Result<Self, String> {
            if std::env::var("XDG_SESSION_TYPE").ok().as_deref() != Some("x11") {
                return Err("not an X11 session".into());
            }
            let (connection, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
            let root = connection.setup().roots[screen].root;
            Ok(Self { connection, root })
        }

        pub fn capture(&self) -> Result<RawFrame, String> {
            let geometry = self.connection.get_geometry(self.root).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
            let (width, height) = (geometry.width, geometry.height);
            let image = self
                .connection
                .get_image(ImageFormat::Z_PIXMAP, self.root, 0, 0, width, height, u32::MAX)
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?;
            if image.depth < 24 || image.data.len() < usize::from(width) * usize::from(height) * 4 {
                return Err("unsupported X11 visual".into());
            }
            Ok(RawFrame { width: width.into(), height: height.into(), stride: usize::from(width) * 4, layout: Layout::BGRX, data: Arc::new(image.data) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_downscales_and_flips() {
        let (width, height) = (3840usize, 2160usize);
        let mut data = vec![0u8; width * height * 4];
        // Top row red, rest blue.
        for x in 0..width {
            data[x * 4 + 2] = 255;
        }
        for y in 2..height {
            for x in 0..width {
                data[(y * width + x) * 4] = 255;
            }
        }
        let frame = RawFrame { width, height, stride: width * 4, layout: Layout::BGRX, data: Arc::new(data) };
        let converted = convert(&frame).unwrap();
        assert_eq!((converted.width, converted.height), (1920, 1080));
        // Bottom-up: the last row is the top of the screen (half red average).
        let top = u16::from_le_bytes([converted.rgb555_bottom_up[(1079 * 1920) * 2], converted.rgb555_bottom_up[(1079 * 1920) * 2 + 1]]);
        assert_eq!(top >> 10, (127u16 >> 3));
        let bottom = u16::from_le_bytes([converted.rgb555_bottom_up[0], converted.rgb555_bottom_up[1]]);
        assert_eq!(bottom, 31);
        assert_eq!(scaled_size(1366, 768), (1366, 768));
    }
}
