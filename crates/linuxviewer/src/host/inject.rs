//! Input injection for an approved incoming viewer.
//!
//! Backends, in order of preference: the RemoteDesktop portal (granted with
//! the screen cast), a kernel uinput device (wlroots/Hyprland, needs write
//! access to /dev/uinput), and XTest on X11 sessions.

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::Arc;

use ashpd::desktop::remote_desktop::{Axis, KeyState, RemoteDesktop};
use ashpd::desktop::Session;
use ultraviewer_protocol::input::{HostInputMessage, KeyboardMessage, MouseButton, MouseMessage};

pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

/// Windows virtual key → Linux evdev key code.
pub fn evdev_for_virtual_key(virtual_key: u8, scan_code: u16) -> Option<u16> {
    let extended = scan_code & 0xff00 == 0xe000 || scan_code & 0x0100 != 0;
    Some(match virtual_key {
        0x08 => 14,
        0x09 => 15,
        0x0d => if extended { 96 } else { 28 },
        0x10 => if scan_code & 0xff == 0x36 { 54 } else { 42 },
        0x11 => if extended { 97 } else { 29 },
        0x12 => if extended { 100 } else { 56 },
        0x13 => 119,
        0x14 => 58,
        0x1b => 1,
        0x20 => 57,
        0x21 => 104,
        0x22 => 109,
        0x23 => 107,
        0x24 => 102,
        0x25 => 105,
        0x26 => 103,
        0x27 => 106,
        0x28 => 108,
        0x2c => 99,
        0x2d => 110,
        0x2e => 111,
        0x30 => 11,
        0x31..=0x39 => u16::from(virtual_key - 0x31) + 2,
        0x41..=0x5a => LETTERS[usize::from(virtual_key - 0x41)],
        0x5b => 125,
        0x5c => 126,
        0x5d => 127,
        0x60 => 82,
        0x61 => 79,
        0x62 => 80,
        0x63 => 81,
        0x64 => 75,
        0x65 => 76,
        0x66 => 77,
        0x67 => 71,
        0x68 => 72,
        0x69 => 73,
        0x6a => 55,
        0x6b => 78,
        0x6d => 74,
        0x6e => 83,
        0x6f => 98,
        0x70..=0x79 => u16::from(virtual_key - 0x70) + 59,
        0x7a => 87,
        0x7b => 88,
        0x7c..=0x87 => u16::from(virtual_key - 0x7c) + 183,
        0x90 => 69,
        0x91 => 70,
        0xa0 => 42,
        0xa1 => 54,
        0xa2 => 29,
        0xa3 => 97,
        0xa4 => 56,
        0xa5 => 100,
        0xad => 113,
        0xae => 114,
        0xaf => 115,
        0xb0 => 163,
        0xb1 => 165,
        0xb2 => 166,
        0xb3 => 164,
        0xba => 39,
        0xbb => 13,
        0xbc => 51,
        0xbd => 12,
        0xbe => 52,
        0xbf => 53,
        0xc0 => 41,
        0xdb => 26,
        0xdc => 43,
        0xdd => 27,
        0xde => 40,
        0xe2 => 86,
        // Unknown VK: Scan Set 1 make codes equal evdev codes below 0x59.
        _ if scan_code & 0xff00 == 0 && (1..0x59).contains(&scan_code) => scan_code,
        _ => return None,
    })
}

const LETTERS: [u16; 26] = [30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44];

/// US-layout key and shift state for a printable ASCII character.
pub fn evdev_for_ascii(character: u16) -> Option<(u16, bool)> {
    let c = char::from_u32(u32::from(character))?;
    if c.is_ascii_lowercase() {
        return Some((LETTERS[(c as u8 - b'a') as usize], false));
    }
    if c.is_ascii_uppercase() {
        return Some((LETTERS[(c as u8 - b'A') as usize], true));
    }
    Some(match c {
        '1'..='9' => (u16::from(c as u8 - b'1') + 2, false),
        '0' => (11, false),
        ' ' => (57, false),
        '-' => (12, false),
        '_' => (12, true),
        '=' => (13, false),
        '+' => (13, true),
        '[' => (26, false),
        '{' => (26, true),
        ']' => (27, false),
        '}' => (27, true),
        ';' => (39, false),
        ':' => (39, true),
        '\'' => (40, false),
        '"' => (40, true),
        '`' => (41, false),
        '~' => (41, true),
        '\\' => (43, false),
        '|' => (43, true),
        ',' => (51, false),
        '<' => (51, true),
        '.' => (52, false),
        '>' => (52, true),
        '/' => (53, false),
        '?' => (53, true),
        '!' => (2, true),
        '@' => (3, true),
        '#' => (4, true),
        '$' => (5, true),
        '%' => (6, true),
        '^' => (7, true),
        '&' => (8, true),
        '*' => (9, true),
        '(' => (10, true),
        ')' => (11, true),
        '\t' => (15, false),
        '\r' | '\n' => (28, false),
        _ => return None,
    })
}

/// A low-level injection backend. Coordinates are in the shared frame's
/// pixels; each backend maps them to its own space.
pub trait InjectBackend: Send {
    /// Size of the frames the viewer sees, in pixels.
    fn set_frame(&mut self, width: f64, height: f64);
    fn pointer(&mut self, x: f64, y: f64) -> Result<(), String>;
    fn button(&mut self, code: u16, pressed: bool) -> Result<(), String>;
    /// Positive steps scroll up (away from the user), like a Windows wheel.
    fn wheel(&mut self, steps: i32) -> Result<(), String>;
    fn key(&mut self, code: u16, pressed: bool) -> Result<(), String>;
    /// Types one Unicode character, when the backend supports it.
    fn unicode(&mut self, _character: char) -> Result<bool, String> {
        Ok(false)
    }
}

// ---------------------------------------------------------------- portal

pub struct PortalBackend {
    pub portal: Arc<super::capture::PortalSession>,
    /// Logical stream size; frame pixels are scaled into it.
    pub logical: (f64, f64),
    pub frame: (f64, f64),
    pub runtime: tokio::runtime::Handle,
}

impl PortalBackend {
    /// Called from the dedicated injector thread, never inside the runtime.
    fn call<'a, F, Fut>(&'a self, call: F) -> Result<(), String>
    where
        F: FnOnce(&'a RemoteDesktop, &'a Session<RemoteDesktop>, u32) -> Fut,
        Fut: std::future::Future<Output = Result<(), ashpd::Error>> + 'a,
    {
        let (remote, session) = self.portal.remote_desktop.as_ref().map(|(r, s)| (r, s)).ok_or("remote desktop session closed")?;
        self.runtime.block_on(call(remote, session, self.portal.node_id)).map_err(|e| e.to_string())
    }
}

impl InjectBackend for PortalBackend {
    fn set_frame(&mut self, width: f64, height: f64) {
        self.frame = (width, height);
    }
    fn pointer(&mut self, x: f64, y: f64) -> Result<(), String> {
        let lx = x * self.logical.0 / self.frame.0.max(1.0);
        let ly = y * self.logical.1 / self.frame.1.max(1.0);
        self.call(|remote, session, node| remote.notify_pointer_motion_absolute(session, node, lx, ly, Default::default()))
    }
    fn button(&mut self, code: u16, pressed: bool) -> Result<(), String> {
        let state = if pressed { KeyState::Pressed } else { KeyState::Released };
        self.call(|remote, session, _| remote.notify_pointer_button(session, i32::from(code), state, Default::default()))
    }
    fn wheel(&mut self, steps: i32) -> Result<(), String> {
        // Portal discrete steps follow libinput: positive scrolls down.
        self.call(|remote, session, _| remote.notify_pointer_axis_discrete(session, Axis::Vertical, -steps, Default::default()))
    }
    fn key(&mut self, code: u16, pressed: bool) -> Result<(), String> {
        let state = if pressed { KeyState::Pressed } else { KeyState::Released };
        self.call(|remote, session, _| remote.notify_keyboard_keycode(session, i32::from(code), state, Default::default()))
    }
    fn unicode(&mut self, character: char) -> Result<bool, String> {
        let keysym = if (0x20..0x7f).contains(&(character as u32)) || (0xa0..0x100).contains(&(character as u32)) {
            character as i32
        } else {
            0x0100_0000 | character as i32
        };
        self.call(|remote, session, _| remote.notify_keyboard_keysym(session, keysym, KeyState::Pressed, Default::default()))?;
        self.call(|remote, session, _| remote.notify_keyboard_keysym(session, keysym, KeyState::Released, Default::default()))?;
        Ok(true)
    }
}

// ---------------------------------------------------------------- uinput

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const REL_WHEEL: u16 = 8;
const REL_WHEEL_HI_RES: u16 = 11;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_RANGE: i32 = 65_535;
const BUS_VIRTUAL: u16 = 0x06;

// ioctl request numbers from <linux/uinput.h> (_IO/_IOW with 'U').
const UI_DEV_CREATE: u64 = 0x5501;
const UI_DEV_DESTROY: u64 = 0x5502;
const UI_DEV_SETUP: u64 = 0x405c_5503;
const UI_ABS_SETUP: u64 = 0x401c_5504;
const UI_SET_EVBIT: u64 = 0x4004_5564;
const UI_SET_KEYBIT: u64 = 0x4004_5565;
const UI_SET_RELBIT: u64 = 0x4004_5566;
const UI_SET_ABSBIT: u64 = 0x4004_5567;

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct InputAbsinfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    absinfo: InputAbsinfo,
}

#[repr(C)]
struct InputEvent {
    time: libc::timeval,
    kind: u16,
    code: u16,
    value: i32,
}

struct UinputDevice {
    file: File,
}

impl UinputDevice {
    fn open() -> Result<Self, String> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags_nonblock()
            .open("/dev/uinput")
            .map_err(|e| format!("/dev/uinput: {e}"))?;
        Ok(Self { file })
    }

    fn ioctl(&self, request: u64, value: i32) -> Result<(), String> {
        // SAFETY: integer-argument uinput ioctls on an owned descriptor.
        let result = unsafe { libc::ioctl(self.file.as_raw_fd(), request as _, value) };
        if result < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }

    fn ioctl_ptr<T>(&self, request: u64, value: &T) -> Result<(), String> {
        // SAFETY: the pointer refers to a properly laid out uinput struct.
        let result = unsafe { libc::ioctl(self.file.as_raw_fd(), request as _, value as *const T) };
        if result < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }

    fn create(&self, name: &str, product: u16) -> Result<(), String> {
        let mut setup = UinputSetup { id: InputId { bustype: BUS_VIRTUAL, vendor: 0x4c56, product, version: 1 }, name: [0; 80], ff_effects_max: 0 };
        setup.name[..name.len().min(79)].copy_from_slice(&name.as_bytes()[..name.len().min(79)]);
        self.ioctl_ptr(UI_DEV_SETUP, &setup)?;
        self.ioctl(UI_DEV_CREATE, 0)
    }

    fn emit(&mut self, kind: u16, code: u16, value: i32) -> Result<(), String> {
        let event = InputEvent { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, kind, code, value };
        // SAFETY: InputEvent is a plain repr(C) struct.
        let bytes = unsafe { std::slice::from_raw_parts((&event as *const InputEvent).cast::<u8>(), std::mem::size_of::<InputEvent>()) };
        self.file.write_all(bytes).map_err(|e| e.to_string())
    }

    fn sync(&mut self) -> Result<(), String> {
        self.emit(EV_SYN, SYN_REPORT, 0)
    }
}

impl Drop for UinputDevice {
    fn drop(&mut self) {
        let _ = self.ioctl(UI_DEV_DESTROY, 0);
    }
}

trait NonBlock {
    fn custom_flags_nonblock(&mut self) -> &mut Self;
}

impl NonBlock for std::fs::OpenOptions {
    fn custom_flags_nonblock(&mut self) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.custom_flags(libc::O_NONBLOCK)
    }
}

/// Virtual keyboard plus an absolute pointer covering the desktop layout.
pub struct UinputBackend {
    keyboard: UinputDevice,
    pointer: UinputDevice,
    /// Shared frame rectangle inside the layout bounding box, in logical
    /// pixels: (x, y, width, height) and the layout size.
    pub target: (f64, f64, f64, f64),
    pub layout: (f64, f64),
    pub frame: (f64, f64),
}

impl UinputBackend {
    pub fn available() -> bool {
        std::fs::OpenOptions::new().write(true).open("/dev/uinput").is_ok()
    }

    pub fn new(target: (f64, f64, f64, f64), layout: (f64, f64), frame: (f64, f64)) -> Result<Self, String> {
        let keyboard = UinputDevice::open()?;
        keyboard.ioctl(UI_SET_EVBIT, i32::from(EV_KEY))?;
        for code in 1..=248 {
            keyboard.ioctl(UI_SET_KEYBIT, code)?;
        }
        keyboard.create("LinuxViewer remote keyboard", 1)?;

        let pointer = UinputDevice::open()?;
        pointer.ioctl(UI_SET_EVBIT, i32::from(EV_KEY))?;
        for code in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE] {
            pointer.ioctl(UI_SET_KEYBIT, i32::from(code))?;
        }
        pointer.ioctl(UI_SET_EVBIT, i32::from(EV_REL))?;
        pointer.ioctl(UI_SET_RELBIT, i32::from(REL_WHEEL))?;
        pointer.ioctl(UI_SET_RELBIT, i32::from(REL_WHEEL_HI_RES))?;
        pointer.ioctl(UI_SET_EVBIT, i32::from(EV_ABS))?;
        for axis in [ABS_X, ABS_Y] {
            pointer.ioctl(UI_SET_ABSBIT, i32::from(axis))?;
            let setup = UinputAbsSetup { code: axis, absinfo: InputAbsinfo { value: 0, minimum: 0, maximum: ABS_RANGE, fuzz: 0, flat: 0, resolution: 0 } };
            pointer.ioctl_ptr(UI_ABS_SETUP, &setup)?;
        }
        pointer.create("LinuxViewer remote pointer", 2)?;
        // Give the compositor time to pick up the new devices.
        std::thread::sleep(std::time::Duration::from_millis(250));
        Ok(Self { keyboard, pointer, target, layout, frame })
    }
}

impl InjectBackend for UinputBackend {
    fn set_frame(&mut self, width: f64, height: f64) {
        self.frame = (width, height);
    }
    fn pointer(&mut self, x: f64, y: f64) -> Result<(), String> {
        let (tx, ty, tw, th) = self.target;
        let lx = tx + x * tw / self.frame.0.max(1.0);
        let ly = ty + y * th / self.frame.1.max(1.0);
        // Aim at the pixel centre so the compositor's rounding lands on it.
        let unit = |value: f64, size: f64| -> i32 {
            let fraction = (value + 0.5) / size.max(1.0);
            (fraction * f64::from(ABS_RANGE)).round().clamp(0.0, f64::from(ABS_RANGE)) as i32
        };
        let ax = unit(lx, self.layout.0);
        let ay = unit(ly, self.layout.1);
        self.pointer.emit(EV_ABS, ABS_X, ax)?;
        self.pointer.emit(EV_ABS, ABS_Y, ay)?;
        self.pointer.sync()
    }
    fn button(&mut self, code: u16, pressed: bool) -> Result<(), String> {
        self.pointer.emit(EV_KEY, code, i32::from(pressed))?;
        self.pointer.sync()
    }
    fn wheel(&mut self, steps: i32) -> Result<(), String> {
        self.pointer.emit(EV_REL, REL_WHEEL, steps)?;
        self.pointer.emit(EV_REL, REL_WHEEL_HI_RES, steps * 120)?;
        self.pointer.sync()
    }
    fn key(&mut self, code: u16, pressed: bool) -> Result<(), String> {
        self.keyboard.emit(EV_KEY, code, i32::from(pressed))?;
        self.keyboard.sync()
    }
}

// ---------------------------------------------------------------- X11

pub struct XTestBackend {
    connection: x11rb::rust_connection::RustConnection,
    root: u32,
    pub frame: (f64, f64),
    pub screen: (f64, f64),
}

impl XTestBackend {
    pub fn new(frame: (f64, f64)) -> Result<Self, String> {
        use x11rb::connection::Connection;
        if std::env::var("XDG_SESSION_TYPE").ok().as_deref() != Some("x11") {
            return Err("not an X11 session".into());
        }
        let (connection, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let root_screen = &connection.setup().roots[screen];
        let root = root_screen.root;
        let size = (f64::from(root_screen.width_in_pixels), f64::from(root_screen.height_in_pixels));
        Ok(Self { connection, root, frame, screen: size })
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<(), String> {
        use x11rb::connection::Connection;
        use x11rb::protocol::xtest::ConnectionExt;
        self.connection.xtest_fake_input(kind, detail, 0, self.root, x, y, 0).map_err(|e| e.to_string())?;
        self.connection.flush().map_err(|e| e.to_string())
    }
}

impl InjectBackend for XTestBackend {
    fn set_frame(&mut self, width: f64, height: f64) {
        self.frame = (width, height);
    }
    fn pointer(&mut self, x: f64, y: f64) -> Result<(), String> {
        let sx = (x * self.screen.0 / self.frame.0.max(1.0)) as i16;
        let sy = (y * self.screen.1 / self.frame.1.max(1.0)) as i16;
        self.fake(6, 0, sx, sy) // MotionNotify
    }
    fn button(&mut self, code: u16, pressed: bool) -> Result<(), String> {
        let button = match code {
            BTN_LEFT => 1,
            BTN_MIDDLE => 2,
            BTN_RIGHT => 3,
            _ => return Ok(()),
        };
        self.fake(if pressed { 4 } else { 5 }, button, 0, 0)
    }
    fn wheel(&mut self, steps: i32) -> Result<(), String> {
        let button = if steps > 0 { 4 } else { 5 };
        for _ in 0..steps.unsigned_abs().min(20) {
            self.fake(4, button, 0, 0)?;
            self.fake(5, button, 0, 0)?;
        }
        Ok(())
    }
    fn key(&mut self, code: u16, pressed: bool) -> Result<(), String> {
        self.fake(if pressed { 2 } else { 3 }, (code + 8).min(255) as u8, 0, 0)
    }
}

// ---------------------------------------------------------------- controller

/// Decodes authenticated viewer payloads and drives a backend, remembering
/// held keys and buttons so they can always be released.
pub struct InputController {
    backend: Box<dyn InjectBackend>,
    held_keys: Vec<u16>,
    held_buttons: HashSet<u16>,
    wheel_remainder: i32,
}

impl InputController {
    pub fn new(backend: Box<dyn InjectBackend>) -> Self {
        Self { backend, held_keys: Vec::new(), held_buttons: HashSet::new(), wheel_remainder: 0 }
    }

    pub fn set_frame(&mut self, width: usize, height: usize) {
        self.backend.set_frame(width as f64, height as f64);
    }

    pub fn handle(&mut self, payload: &[u8]) -> Result<bool, String> {
        let Some(message) = HostInputMessage::decode(payload).map_err(|e| e.to_string())? else { return Ok(false) };
        match message {
            HostInputMessage::Keyboard(key) => self.keyboard(key),
            HostInputMessage::Mouse(MouseMessage::Move { x, y, .. }) => {
                self.backend.pointer(f64::from(x.max(0)), f64::from(y.max(0)))?;
                Ok(true)
            }
            HostInputMessage::Mouse(MouseMessage::Button { button, .. }) => {
                // Original Windows click lParam is viewer-client coordinates;
                // the separately transmitted position is authoritative.
                let (code, action) = match button {
                    MouseButton::LeftDown => (BTN_LEFT, Some(true)),
                    MouseButton::LeftUp => (BTN_LEFT, Some(false)),
                    MouseButton::RightDown => (BTN_RIGHT, Some(true)),
                    MouseButton::RightUp => (BTN_RIGHT, Some(false)),
                    MouseButton::MiddleDown => (BTN_MIDDLE, Some(true)),
                    MouseButton::MiddleUp => (BTN_MIDDLE, Some(false)),
                    MouseButton::LeftDoubleClick => (BTN_LEFT, None),
                    MouseButton::RightDoubleClick => (BTN_RIGHT, None),
                    MouseButton::MiddleDoubleClick => (BTN_MIDDLE, None),
                };
                match action {
                    Some(pressed) => self.button(code, pressed)?,
                    None => {
                        // The OS derives the double click from timing.
                        self.button(code, true)?;
                        self.button(code, false)?;
                    }
                }
                Ok(true)
            }
            HostInputMessage::Mouse(MouseMessage::VerticalWheel { w_param, .. }) => {
                self.wheel_remainder += i32::from((w_param as u32 >> 16) as u16 as i16);
                let steps = self.wheel_remainder / 120;
                self.wheel_remainder %= 120;
                if steps != 0 {
                    self.backend.wheel(steps)?;
                }
                Ok(true)
            }
        }
    }

    fn button(&mut self, code: u16, pressed: bool) -> Result<(), String> {
        if !pressed && !self.held_buttons.contains(&code) {
            return Ok(());
        }
        self.backend.button(code, pressed)?;
        if pressed {
            self.held_buttons.insert(code);
        } else {
            self.held_buttons.remove(&code);
        }
        Ok(())
    }

    fn keyboard(&mut self, message: KeyboardMessage) -> Result<bool, String> {
        if (0xd800..=0xdfff).contains(&message.character) {
            return Ok(false);
        }
        let down = message.kind.is_down();
        if message.virtual_key == KeyboardMessage::VK_PACKET {
            if !down || message.character < 0x20 {
                return Ok(true);
            }
            let Some(character) = char::from_u32(u32::from(message.character)) else { return Ok(false) };
            if self.backend.unicode(character)? {
                return Ok(true);
            }
            if let Some((code, shift)) = evdev_for_ascii(message.character) {
                let shift_held = self.held_keys.contains(&42) || self.held_keys.contains(&54);
                if shift && !shift_held {
                    self.backend.key(42, true)?;
                }
                self.backend.key(code, true)?;
                self.backend.key(code, false)?;
                if shift && !shift_held {
                    self.backend.key(42, false)?;
                }
            }
            return Ok(true);
        }
        let Some(code) = evdev_for_virtual_key(message.virtual_key, message.scan_code) else { return Ok(false) };
        if down {
            if !self.held_keys.contains(&code) {
                self.held_keys.push(code);
            }
            self.backend.key(code, true)?;
        } else if let Some(index) = self.held_keys.iter().position(|&k| k == code) {
            self.held_keys.remove(index);
            self.backend.key(code, false)?;
        }
        Ok(true)
    }

    /// Call on peer disconnect, local stop, control revocation and shutdown.
    pub fn release_all(&mut self) {
        for code in std::mem::take(&mut self.held_keys).into_iter().rev() {
            let _ = self.backend.key(code, false);
        }
        for code in std::mem::take(&mut self.held_buttons) {
            let _ = self.backend.button(code, false);
        }
        self.wheel_remainder = 0;
    }
}

impl Drop for InputController {
    fn drop(&mut self) {
        self.release_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default, Clone)]
    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl InjectBackend for Recorder {
        fn set_frame(&mut self, _width: f64, _height: f64) {}
        fn pointer(&mut self, x: f64, y: f64) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("move {x} {y}"));
            Ok(())
        }
        fn button(&mut self, code: u16, pressed: bool) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("button {code} {pressed}"));
            Ok(())
        }
        fn wheel(&mut self, steps: i32) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("wheel {steps}"));
            Ok(())
        }
        fn key(&mut self, code: u16, pressed: bool) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("key {code} {pressed}"));
            Ok(())
        }
    }

    #[test]
    fn decodes_and_releases_held_input() {
        let recorder = Recorder::default();
        let mut controller = InputController::new(Box::new(recorder.clone()));
        controller.handle(b"8|||512|||0|||10|||20").unwrap();
        controller.handle(b"8|||513|||1|||1310730").unwrap();
        let ctrl = KeyboardMessage { kind: ultraviewer_protocol::KeyboardKind::KeyDown, virtual_key: 0xa2, scan_code: 0x1d, character: 0, caps_lock_on: false };
        controller.handle(&ctrl.encode()).unwrap();
        controller.handle(b"8|||256|||67|||-1|||-1").unwrap(); // legacy VK 'C' down
        controller.handle(&MouseMessage::VerticalWheel { w_param: MouseMessage::wheel_w_param(-240, 0), l_param: 0 }.encode()).unwrap();
        let text = KeyboardMessage { kind: ultraviewer_protocol::KeyboardKind::KeyDown, virtual_key: 0xe7, scan_code: 0, character: u16::from(b'A'), caps_lock_on: false };
        controller.handle(&text.encode()).unwrap();
        controller.release_all();
        let log = recorder.0.lock().unwrap().clone();
        assert_eq!(
            log,
            vec![
                "move 10 20", "button 272 true", "key 29 true", "key 46 true", "wheel -2",
                "key 42 true", "key 30 true", "key 30 false", "key 42 false",
                "key 46 false", "key 29 false", "button 272 false",
            ]
        );
    }
}
