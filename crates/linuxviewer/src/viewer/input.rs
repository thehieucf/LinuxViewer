//! Session-local input state: translates Linux keyboard/pointer events into
//! UltraViewer application payloads, sent in order with the false format flag.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use ultraviewer_protocol::input::{KeyboardKind, KeyboardMessage, MouseButton, MouseMessage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const SHIFT: Self = Self(1);
    pub const CONTROL: Self = Self(2);
    pub const ALT: Self = Self(4);
    pub const SUPER: Self = Self(8);
    pub const CAPS_LOCK: Self = Self(16);

    pub fn empty() -> Self {
        Self(0)
    }
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RemoteInput {
    /// Integer pixels in remote virtual-desktop coordinates.
    PointerMoved { x: i64, y: i64 },
    /// Button 0 = left, 1 = right, 2 = middle.
    PointerButton { button: u8, pressed: bool, x: i64, y: i64 },
    /// Wheel detents; positive scrolls up (away from the user).
    Scroll { delta_y: f64 },
    /// `code` is the Linux evdev key code; the encoder maps it for Windows.
    Key { code: u16, characters: Option<String>, pressed: bool, modifiers: Modifiers },
    /// Text committed by an input method (e.g. Vietnamese Telex).
    Text(String),
    Modifiers(Modifiers),
    /// Release held remote keys/buttons on focus loss, control toggle, or disconnect.
    ReleaseAll,
}

#[derive(Debug, Clone, Copy)]
struct Key {
    virtual_key: u8,
    scan_code: u16,
    text: bool,
    control: Option<u16>,
}

const fn key(virtual_key: u8, scan_code: u16) -> Key {
    Key { virtual_key, scan_code, text: false, control: None }
}

const fn text(virtual_key: u8, scan_code: u16) -> Key {
    Key { virtual_key, scan_code, text: true, control: None }
}

const fn control(virtual_key: u8, scan_code: u16, character: u16) -> Key {
    Key { virtual_key, scan_code, text: false, control: Some(character) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModifierGroup {
    Shift,
    Control,
    Alt,
    Super,
}

impl ModifierGroup {
    const ALL: [ModifierGroup; 4] = [ModifierGroup::Shift, ModifierGroup::Control, ModifierGroup::Alt, ModifierGroup::Super];

    fn flag(self) -> Modifiers {
        match self {
            Self::Shift => Modifiers::SHIFT,
            Self::Control => Modifiers::CONTROL,
            Self::Alt => Modifiers::ALT,
            Self::Super => Modifiers::SUPER,
        }
    }
    /// evdev codes for the left/right keys of each group.
    fn left(self) -> u16 {
        match self {
            Self::Shift => 42,
            Self::Control => 29,
            Self::Alt => 56,
            Self::Super => 125,
        }
    }
    fn right(self) -> u16 {
        match self {
            Self::Shift => 54,
            Self::Control => 97,
            Self::Alt => 100,
            Self::Super => 126,
        }
    }
}

fn modifier_group(code: u16) -> Option<ModifierGroup> {
    ModifierGroup::ALL.into_iter().find(|g| g.left() == code || g.right() == code)
}

pub fn is_modifier_key(code: u16) -> bool {
    modifier_group(code).is_some()
}

/// Linux evdev key positions → Windows VK / Scan Set 1 make code. E0-prefixed
/// keys keep that prefix in the UInt16 scan field.
fn mapping(code: u16) -> Option<Key> {
    Some(match code {
        1 => control(0x1b, 0x01, 27),
        2 => text(0x31, 0x02), 3 => text(0x32, 0x03), 4 => text(0x33, 0x04), 5 => text(0x34, 0x05),
        6 => text(0x35, 0x06), 7 => text(0x36, 0x07), 8 => text(0x37, 0x08), 9 => text(0x38, 0x09),
        10 => text(0x39, 0x0a), 11 => text(0x30, 0x0b),
        12 => text(0xbd, 0x0c), // minus
        13 => text(0xbb, 0x0d), // equal
        14 => control(0x08, 0x0e, 8), // backspace
        15 => control(0x09, 0x0f, 9), // tab
        16 => text(0x51, 0x10), 17 => text(0x57, 0x11), 18 => text(0x45, 0x12), 19 => text(0x52, 0x13),
        20 => text(0x54, 0x14), 21 => text(0x59, 0x15), 22 => text(0x55, 0x16), 23 => text(0x49, 0x17),
        24 => text(0x4f, 0x18), 25 => text(0x50, 0x19),
        26 => text(0xdb, 0x1a), // [
        27 => text(0xdd, 0x1b), // ]
        28 => control(0x0d, 0x1c, 13), // enter
        29 => key(0xa2, 0x1d), // left ctrl
        30 => text(0x41, 0x1e), 31 => text(0x53, 0x1f), 32 => text(0x44, 0x20), 33 => text(0x46, 0x21),
        34 => text(0x47, 0x22), 35 => text(0x48, 0x23), 36 => text(0x4a, 0x24), 37 => text(0x4b, 0x25),
        38 => text(0x4c, 0x26),
        39 => text(0xba, 0x27), // ;
        40 => text(0xde, 0x28), // '
        41 => text(0xc0, 0x29), // `
        42 => key(0xa0, 0x2a), // left shift
        43 => text(0xdc, 0x2b), // backslash
        44 => text(0x5a, 0x2c), 45 => text(0x58, 0x2d), 46 => text(0x43, 0x2e), 47 => text(0x56, 0x2f),
        48 => text(0x42, 0x30), 49 => text(0x4e, 0x31), 50 => text(0x4d, 0x32),
        51 => text(0xbc, 0x33), // ,
        52 => text(0xbe, 0x34), // .
        53 => text(0xbf, 0x35), // /
        54 => key(0xa1, 0x36), // right shift
        55 => text(0x6a, 0x37), // keypad *
        56 => key(0xa4, 0x38), // left alt
        57 => text(0x20, 0x39), // space
        59 => key(0x70, 0x3b), 60 => key(0x71, 0x3c), 61 => key(0x72, 0x3d), 62 => key(0x73, 0x3e),
        63 => key(0x74, 0x3f), 64 => key(0x75, 0x40), 65 => key(0x76, 0x41), 66 => key(0x77, 0x42),
        67 => key(0x78, 0x43), 68 => key(0x79, 0x44),
        70 => key(0x91, 0x46), // scroll lock
        71 => text(0x67, 0x47), 72 => text(0x68, 0x48), 73 => text(0x69, 0x49),
        74 => text(0x6d, 0x4a), // keypad -
        75 => text(0x64, 0x4b), 76 => text(0x65, 0x4c), 77 => text(0x66, 0x4d),
        78 => text(0x6b, 0x4e), // keypad +
        79 => text(0x61, 0x4f), 80 => text(0x62, 0x50), 81 => text(0x63, 0x51), 82 => text(0x60, 0x52),
        83 => text(0x6e, 0x53), // keypad .
        86 => text(0xe2, 0x56), // ISO extra key
        87 => key(0x7a, 0x57), 88 => key(0x7b, 0x58), // F11, F12
        96 => control(0x0d, 0xe01c, 13), // keypad enter
        97 => key(0xa3, 0xe01d), // right ctrl
        98 => text(0x6f, 0xe035), // keypad /
        99 => key(0x2c, 0xe037), // print screen
        100 => key(0xa5, 0xe038), // right alt
        102 => key(0x24, 0xe047), // home
        103 => key(0x26, 0xe048), // up
        104 => key(0x21, 0xe049), // page up
        105 => key(0x25, 0xe04b), // left
        106 => key(0x27, 0xe04d), // right
        107 => key(0x23, 0xe04f), // end
        108 => key(0x28, 0xe050), // down
        109 => key(0x22, 0xe051), // page down
        110 => key(0x2d, 0xe052), // insert
        111 => key(0x2e, 0xe053), // delete
        113 => key(0xad, 0xe020), // mute
        114 => key(0xae, 0xe02e), // volume down
        115 => key(0xaf, 0xe030), // volume up
        117 => text(0xbb, 0x59), // keypad =
        119 => key(0x13, 0x45), // pause
        125 => key(0x5b, 0xe05b), // left super → LWIN
        126 => key(0x5c, 0xe05c), // right super → RWIN
        127 => key(0x5d, 0xe05d), // menu
        163 => key(0xb0, 0xe019), // next track
        164 => key(0xb3, 0xe022), // play/pause
        165 => key(0xb1, 0xe010), // previous track
        166 => key(0xb2, 0xe024), // stop
        183 => key(0x7c, 0x64), 184 => key(0x7d, 0x65), 185 => key(0x7e, 0x66), 186 => key(0x7f, 0x67),
        187 => key(0x80, 0x68), 188 => key(0x81, 0x69), 189 => key(0x82, 0x6a), 190 => key(0x83, 0x6b),
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy)]
struct HeldKey {
    mapping: Key,
    character: u16,
    system: bool,
    synthetic_modifier: bool,
}

impl HeldKey {
    fn message(&self, pressed: bool, caps_lock: bool) -> Vec<u8> {
        let kind = match (self.system, pressed) {
            (true, true) => KeyboardKind::SystemKeyDown,
            (true, false) => KeyboardKind::SystemKeyUp,
            (false, true) => KeyboardKind::KeyDown,
            (false, false) => KeyboardKind::KeyUp,
        };
        KeyboardMessage {
            kind,
            virtual_key: self.mapping.virtual_key,
            scan_code: self.mapping.scan_code,
            character: self.character,
            caps_lock_on: caps_lock,
        }
        .encode()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn new(x: i64, y: i64) -> Option<Self> {
        Some(Self { x: i32::try_from(x).ok()?, y: i32::try_from(y).ok()? })
    }
    fn packed(self) -> i32 {
        MouseMessage::packed_position(self.x as i16, self.y as i16)
    }
}

pub struct RemoteInputEncoder {
    supports_binary_keyboard: bool,
    keys: HashMap<u16, HeldKey>,
    key_order: Vec<u16>,
    buttons: Vec<u8>,
    modifiers: Modifiers,
    pointer: Option<Point>,
    last_move_flags: Option<i32>,
    wheel_remainder: f64,
}

impl RemoteInputEncoder {
    /// Supply the negotiated capability corresponding to the desktop's
    /// isTargetVersionGreaterThan6213 branch.
    pub fn new(supports_binary_keyboard: bool) -> Self {
        Self {
            supports_binary_keyboard,
            keys: HashMap::new(),
            key_order: Vec::new(),
            buttons: Vec::new(),
            modifiers: Modifiers::empty(),
            pointer: None,
            last_move_flags: None,
            wheel_remainder: 0.0,
        }
    }

    pub fn encode(&mut self, input: &RemoteInput) -> Vec<Vec<u8>> {
        let mut output = Vec::new();
        match input {
            RemoteInput::PointerMoved { x, y } => {
                if let Some(point) = Point::new(*x, *y) {
                    self.move_to(point, false, &mut output);
                }
            }
            RemoteInput::PointerButton { button, pressed, x, y } => {
                let Some(event) = button_event(*button, *pressed) else { return output };
                if *pressed == self.buttons.contains(button) {
                    return output;
                }
                if let Some(point) = Point::new(*x, *y) {
                    self.move_to(point, *pressed, &mut output);
                } else if *pressed {
                    return output;
                }
                if *pressed {
                    self.buttons.push(*button);
                } else {
                    self.buttons.retain(|b| b != button);
                }
                output.push(MouseMessage::Button { button: event, w_param: self.mouse_flags(), l_param: self.packed_pointer() }.encode());
            }
            RemoteInput::Scroll { delta_y } => {
                if !delta_y.is_finite() {
                    return output;
                }
                // One input unit is one 120-unit wheel step; keep fractional units.
                let total = self.wheel_remainder + delta_y.clamp(-256.0, 256.0);
                let steps = total.trunc();
                self.wheel_remainder = total - steps;
                if steps != 0.0 {
                    let w_param = MouseMessage::wheel_w_param((steps * 120.0) as i16, self.mouse_flags() as u16);
                    output.push(MouseMessage::VerticalWheel { w_param, l_param: self.packed_pointer() }.encode());
                }
            }
            RemoteInput::Key { code, characters, pressed, modifiers } => {
                self.modifiers = *modifiers;
                if !self.supports_binary_keyboard {
                    return output;
                }
                let group = modifier_group(*code);
                self.reconcile_modifiers(group, &mut output);
                let Some(mapping) = mapping(*code) else { return output };
                if let Some(group) = group {
                    if *pressed {
                        // Replace an aggregate event's guessed side when a concrete
                        // hardware-side event supplies that information.
                        let others: Vec<u16> = self.key_order.iter().copied().filter(|&o| modifier_group(o) == Some(group)).collect();
                        for other in others {
                            if other != *code && self.keys.get(&other).is_some_and(|k| k.synthetic_modifier) {
                                self.release_key(other, &mut output);
                            }
                        }
                        if let Some(held) = self.keys.get_mut(code) {
                            held.synthetic_modifier = false;
                        } else {
                            self.press_key(*code, mapping, 0, false, &mut output);
                        }
                    } else {
                        self.release_key(*code, &mut output);
                    }
                    self.reconcile_modifier(group, if *pressed { None } else { Some(*code) }, &mut output);
                } else if *pressed {
                    let character = character_for(mapping, characters.as_deref(), *modifiers);
                    self.press_key(*code, mapping, character, false, &mut output);
                } else {
                    self.release_key(*code, &mut output);
                }
            }
            RemoteInput::Text(text) => {
                if !self.supports_binary_keyboard {
                    return output;
                }
                // VK_PACKET carries one UTF-16 unit per down/up pair, as the
                // Windows desktop sends for IME text.
                for unit in text.encode_utf16() {
                    if unit < 0x20 || (0xd800..=0xdfff).contains(&unit) {
                        continue;
                    }
                    for kind in [KeyboardKind::KeyDown, KeyboardKind::KeyUp] {
                        output.push(
                            KeyboardMessage {
                                kind,
                                virtual_key: KeyboardMessage::VK_PACKET,
                                scan_code: 0,
                                character: unit,
                                caps_lock_on: self.modifiers.contains(Modifiers::CAPS_LOCK),
                            }
                            .encode(),
                        );
                    }
                }
            }
            RemoteInput::Modifiers(flags) => {
                self.modifiers = *flags;
                if self.supports_binary_keyboard {
                    self.reconcile_modifiers(None, &mut output);
                }
            }
            RemoteInput::ReleaseAll => {
                // Release ordinary keys/buttons while their modifiers remain down.
                let ordinary: Vec<u16> = self.key_order.iter().rev().copied().filter(|&c| modifier_group(c).is_none()).collect();
                for code in ordinary {
                    self.release_key(code, &mut output);
                }
                let buttons: Vec<u8> = self.buttons.iter().rev().copied().collect();
                for button in buttons {
                    self.buttons.retain(|&b| b != button);
                    if let Some(event) = button_event(button, false) {
                        output.push(MouseMessage::Button { button: event, w_param: self.mouse_flags(), l_param: self.packed_pointer() }.encode());
                    }
                }
                let remaining: Vec<u16> = self.key_order.iter().rev().copied().collect();
                for code in remaining {
                    self.release_key(code, &mut output);
                }
                self.modifiers = self.modifiers.intersection(Modifiers::CAPS_LOCK);
                self.wheel_remainder = 0.0;
            }
        }
        output
    }

    fn packed_pointer(&self) -> i32 {
        self.pointer.map(Point::packed).unwrap_or(0)
    }

    fn move_to(&mut self, point: Point, force: bool, output: &mut Vec<Vec<u8>>) {
        let flags = self.mouse_flags();
        if force || Some(point) != self.pointer || self.last_move_flags != Some(flags) {
            output.push(MouseMessage::Move { w_param: flags, x: point.x, y: point.y }.encode());
            self.last_move_flags = Some(flags);
        }
        self.pointer = Some(point);
    }

    fn mouse_flags(&self) -> i32 {
        let mut flags = 0;
        if self.buttons.contains(&0) {
            flags |= 0x01;
        }
        if self.buttons.contains(&1) {
            flags |= 0x02;
        }
        if self.buttons.contains(&2) {
            flags |= 0x10;
        }
        if self.modifiers.contains(Modifiers::SHIFT) || self.holds(ModifierGroup::Shift) {
            flags |= 0x04;
        }
        if self.modifiers.contains(Modifiers::CONTROL) || self.holds(ModifierGroup::Control) {
            flags |= 0x08;
        }
        flags
    }

    fn holds(&self, group: ModifierGroup) -> bool {
        self.keys.keys().any(|&code| modifier_group(code) == Some(group))
    }

    fn reconcile_modifiers(&mut self, excluding: Option<ModifierGroup>, output: &mut Vec<Vec<u8>>) {
        for group in ModifierGroup::ALL {
            if Some(group) != excluding {
                self.reconcile_modifier(group, None, output);
            }
        }
    }

    fn reconcile_modifier(&mut self, group: ModifierGroup, avoiding: Option<u16>, output: &mut Vec<Vec<u8>>) {
        if !self.modifiers.contains(group.flag()) {
            let held: Vec<u16> = self.key_order.iter().copied().filter(|&c| modifier_group(c) == Some(group)).collect();
            for code in held {
                self.release_key(code, output);
            }
        } else if !self.holds(group) {
            let code = if Some(group.left()) == avoiding { group.right() } else { group.left() };
            if let Some(mapping) = mapping(code) {
                self.press_key(code, mapping, 0, true, output);
            }
        }
    }

    fn press_key(&mut self, code: u16, mapping: Key, character: u16, synthetic: bool, output: &mut Vec<Vec<u8>>) {
        let system = modifier_group(code) == Some(ModifierGroup::Alt) || self.holds(ModifierGroup::Alt);
        let held = HeldKey { mapping, character, system, synthetic_modifier: synthetic };
        if !self.keys.contains_key(&code) {
            self.key_order.push(code);
        }
        self.keys.insert(code, held);
        output.push(held.message(true, self.modifiers.contains(Modifiers::CAPS_LOCK)));
    }

    fn release_key(&mut self, code: u16, output: &mut Vec<Vec<u8>>) {
        let Some(held) = self.keys.remove(&code) else { return };
        self.key_order.retain(|&c| c != code);
        output.push(held.message(false, self.modifiers.contains(Modifiers::CAPS_LOCK)));
    }
}

fn character_for(key: Key, characters: Option<&str>, modifiers: Modifiers) -> u16 {
    // Shortcut modifiers select the physical VK path in the recovered host.
    if modifiers.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER) {
        return 0;
    }
    if let Some(control) = key.control {
        return control;
    }
    match characters {
        Some(text) if key.text && text.len() == 1 && (0x20..=0x7e).contains(&text.as_bytes()[0]) => u16::from(text.as_bytes()[0]),
        _ => 0,
    }
}

fn button_event(button: u8, pressed: bool) -> Option<MouseButton> {
    Some(match (button, pressed) {
        (0, true) => MouseButton::LeftDown,
        (0, false) => MouseButton::LeftUp,
        (1, true) => MouseButton::RightDown,
        (1, false) => MouseButton::RightUp,
        (2, true) => MouseButton::MiddleDown,
        (2, false) => MouseButton::MiddleUp,
        _ => return None,
    })
}

/// Wheel/touchpad deltas to wheel detents. GTK reports discrete wheel steps
/// as 1.0 per notch and touchpad motion in surface pixels.
pub fn scroll_detents(delta: f64, precise: bool, sensitivity: f64) -> f64 {
    if !delta.is_finite() || !sensitivity.is_finite() {
        return 0.0;
    }
    let speed = sensitivity.clamp(0.25, 2.0);
    delta / if precise { 40.0 } else { 1.0 } * speed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError {
    Overflow,
    Finished,
}

/// FIFO for one remote-input sender. Wakeups may be coalesced; events live in
/// the bounded queue. Only an immediately preceding, still-pending pointer
/// move is coalesced; every other event keeps its own slot.
pub struct InputQueue {
    items: Mutex<VecDeque<RemoteInput>>,
    notify: Notify,
    finished: AtomicBool,
    capacity: usize,
}

impl InputQueue {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self { items: Mutex::new(VecDeque::new()), notify: Notify::new(), finished: AtomicBool::new(false), capacity })
    }

    pub fn enqueue(&self, input: RemoteInput) -> Result<(), QueueError> {
        if self.finished.load(Ordering::SeqCst) {
            return Err(QueueError::Finished);
        }
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(input, RemoteInput::PointerMoved { .. }) {
            if let Some(last @ RemoteInput::PointerMoved { .. }) = items.back_mut() {
                *last = input;
                drop(items);
                self.notify.notify_one();
                return Ok(());
            }
        }
        if items.len() >= self.capacity {
            return Err(QueueError::Overflow);
        }
        items.push_back(input);
        drop(items);
        self.notify.notify_one();
        Ok(())
    }

    fn pop(&self) -> Option<RemoteInput> {
        self.items.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// Idempotent cancellation boundary. No queued events survive.
    pub fn finish(&self) {
        self.finished.store(true, Ordering::SeqCst);
        self.items.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.notify.notify_one();
    }

    /// Waits for the next event; `None` once finished.
    pub async fn next(&self) -> Option<RemoteInput> {
        loop {
            if self.is_finished() {
                return None;
            }
            if let Some(input) = self.pop() {
                return Some(input);
            }
            self.notify.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(payloads: &[Vec<u8>]) -> Vec<String> {
        payloads
            .iter()
            .map(|p| if p[0] == 30 { format!("K{}:{:02x}:{:04x}:{}", p[4], p[5], u16::from_le_bytes([p[6], p[7]]), u16::from_le_bytes([p[8], p[9]])) } else { String::from_utf8_lossy(p).into_owned() })
            .collect()
    }

    #[test]
    fn letter_with_character_and_release() {
        let mut encoder = RemoteInputEncoder::new(true);
        let down = encoder.encode(&RemoteInput::Key { code: 30, characters: Some("a".into()), pressed: true, modifiers: Modifiers::empty() });
        assert_eq!(decode(&down), vec!["K1:41:001e:97"]);
        let up = encoder.encode(&RemoteInput::Key { code: 30, characters: Some("a".into()), pressed: false, modifiers: Modifiers::empty() });
        assert_eq!(decode(&up), vec!["K2:41:001e:97"]);
    }

    #[test]
    fn modifiers_synthesize_left_keys_and_alt_is_system() {
        let mut encoder = RemoteInputEncoder::new(true);
        let ctrl = encoder.encode(&RemoteInput::Modifiers(Modifiers::CONTROL));
        assert_eq!(decode(&ctrl), vec!["K1:a2:001d:0"]);
        let c = encoder.encode(&RemoteInput::Key { code: 46, characters: Some("c".into()), pressed: true, modifiers: Modifiers::CONTROL });
        assert_eq!(decode(&c), vec!["K1:43:002e:0"]);
        let release = encoder.encode(&RemoteInput::ReleaseAll);
        assert_eq!(decode(&release), vec!["K2:43:002e:0", "K2:a2:001d:0"]);
        let alt = encoder.encode(&RemoteInput::Modifiers(Modifiers::ALT));
        assert_eq!(decode(&alt), vec!["K3:a4:0038:0"]);
        let tab = encoder.encode(&RemoteInput::Key { code: 15, characters: None, pressed: true, modifiers: Modifiers::ALT });
        assert_eq!(decode(&tab), vec!["K3:09:000f:0"]);
    }

    #[test]
    fn pointer_buttons_wheel_and_text() {
        let mut encoder = RemoteInputEncoder::new(true);
        let down = encoder.encode(&RemoteInput::PointerButton { button: 0, pressed: true, x: 30, y: 40 });
        assert_eq!(decode(&down), vec!["8|||512|||0|||30|||40", "8|||513|||1|||2621470"]);
        let moved = encoder.encode(&RemoteInput::PointerMoved { x: 31, y: 40 });
        assert_eq!(decode(&moved), vec!["8|||512|||1|||31|||40"]);
        assert!(encoder.encode(&RemoteInput::PointerMoved { x: 31, y: 40 }).is_empty());
        let wheel = encoder.encode(&RemoteInput::Scroll { delta_y: -1.0 });
        assert_eq!(decode(&wheel), vec![format!("8|||522|||{}|||{}", MouseMessage::wheel_w_param(-120, 1), MouseMessage::packed_position(31, 40))]);
        let text = encoder.encode(&RemoteInput::Text("ệ".into()));
        assert_eq!(decode(&text), vec!["K1:e7:0000:7879", "K2:e7:0000:7879"]);
        let release = encoder.encode(&RemoteInput::ReleaseAll);
        assert_eq!(decode(&release), vec!["8|||514|||0|||2621471"]);
    }

    #[test]
    fn queue_coalesces_only_trailing_moves() {
        let queue = InputQueue::new(2);
        queue.enqueue(RemoteInput::PointerMoved { x: 1, y: 1 }).unwrap();
        queue.enqueue(RemoteInput::PointerMoved { x: 2, y: 2 }).unwrap();
        queue.enqueue(RemoteInput::ReleaseAll).unwrap();
        assert_eq!(queue.enqueue(RemoteInput::Scroll { delta_y: 1.0 }), Err(QueueError::Overflow));
        assert_eq!(queue.pop(), Some(RemoteInput::PointerMoved { x: 2, y: 2 }));
        queue.finish();
        assert_eq!(queue.enqueue(RemoteInput::ReleaseAll), Err(QueueError::Finished));
    }
}
