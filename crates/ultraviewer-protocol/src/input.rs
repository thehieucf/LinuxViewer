//! Keyboard and mouse application payloads, plus host-side decoding.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyboardKind {
    KeyDown = 1,
    KeyUp = 2,
    SystemKeyDown = 3,
    SystemKeyUp = 4,
}

impl KeyboardKind {
    pub fn from_raw(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::KeyDown),
            2 => Some(Self::KeyUp),
            3 => Some(Self::SystemKeyDown),
            4 => Some(Self::SystemKeyUp),
            _ => None,
        }
    }

    /// Recovered VFunction.MsgToKBByte mapping.
    pub fn from_windows_message(message: u32) -> Option<Self> {
        match message {
            0x100 => Some(Self::KeyDown),
            0x101 => Some(Self::KeyUp),
            0x104 => Some(Self::SystemKeyDown),
            0x105 => Some(Self::SystemKeyUp),
            _ => None,
        }
    }

    pub fn is_down(self) -> bool {
        matches!(self, Self::KeyDown | Self::SystemKeyDown)
    }
}

/// Eleven application bytes emitted by the supplied desktop's SendKeyboardMsg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardMessage {
    pub kind: KeyboardKind,
    pub virtual_key: u8,
    pub scan_code: u16,
    /// One UTF-16 code unit, matching the recovered UInt16 conversion.
    pub character: u16,
    pub caps_lock_on: bool,
}

impl KeyboardMessage {
    pub const VK_PACKET: u8 = 0xe7;

    pub fn encode(&self) -> Vec<u8> {
        let scan = self.scan_code.to_le_bytes();
        let character = self.character.to_le_bytes();
        vec![30, 0x7c, 0x7c, 0x7c, self.kind as u8, self.virtual_key, scan[0], scan[1], character[0], character[1],
             u8::from(self.caps_lock_on)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    LeftDown = 0x201,
    LeftUp = 0x202,
    LeftDoubleClick = 0x203,
    RightDown = 0x204,
    RightUp = 0x205,
    RightDoubleClick = 0x206,
    MiddleDown = 0x207,
    MiddleUp = 0x208,
    MiddleDoubleClick = 0x209,
}

impl MouseButton {
    pub fn from_raw(value: i32) -> Option<Self> {
        Some(match value {
            0x201 => Self::LeftDown,
            0x202 => Self::LeftUp,
            0x203 => Self::LeftDoubleClick,
            0x204 => Self::RightDown,
            0x205 => Self::RightUp,
            0x206 => Self::RightDoubleClick,
            0x207 => Self::MiddleDown,
            0x208 => Self::MiddleUp,
            0x209 => Self::MiddleDoubleClick,
            _ => return None,
        })
    }
}

/// Application text emitted through Form1.SendMSG (+0xce8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMessage {
    /// Coordinates are already scaled into remote desktop coordinates.
    Move { w_param: i32, x: i32, y: i32 },
    /// A changed mouse position is sent separately before this event.
    Button { button: MouseButton, w_param: i32, l_param: i32 },
    /// Vertical wheel (WM_MOUSEWHEEL 0x20a). The signed delta remains packed in
    /// wParam's high word.
    VerticalWheel { w_param: i32, l_param: i32 },
}

impl MouseMessage {
    pub fn windows_message(&self) -> i32 {
        match self {
            Self::Move { .. } => 0x200,
            Self::Button { button, .. } => *button as i32,
            Self::VerticalWheel { .. } => 0x20a,
        }
    }

    pub fn text(&self) -> String {
        let parameters: Vec<i32> = match *self {
            Self::Move { w_param, x, y } => vec![w_param, x, y],
            Self::Button { w_param, l_param, .. } | Self::VerticalWheel { w_param, l_param } => vec![w_param, l_param],
        };
        let mut fields = vec!["8".to_string(), self.windows_message().to_string()];
        fields.extend(parameters.iter().map(i32::to_string));
        fields.join("|||")
    }

    pub fn encode(&self) -> Vec<u8> {
        self.text().into_bytes()
    }

    /// Preserve Win32's two 16-bit coordinate words, including negative values.
    pub fn packed_position(x: i16, y: i16) -> i32 {
        (u32::from(x as u16) | (u32::from(y as u16) << 16)) as i32
    }

    pub fn wheel_w_param(delta: i16, key_flags: u16) -> i32 {
        (u32::from(key_flags) | (u32::from(delta as u16) << 16)) as i32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("The remote computer sent an invalid input message.")]
pub struct MalformedInput;

/// Host-side decoding of viewer input. The hosting session must establish the
/// authenticated sender, route, and the user's control permission first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostInputMessage {
    Mouse(MouseMessage),
    Keyboard(KeyboardMessage),
}

impl HostInputMessage {
    pub fn decode(payload: &[u8]) -> Result<Option<Self>, MalformedInput> {
        if payload.first() == Some(&30) {
            if payload.len() != 11 || payload[1..4] != [124, 124, 124] || payload[10] > 1 {
                return Err(MalformedInput);
            }
            let kind = KeyboardKind::from_raw(payload[4]).ok_or(MalformedInput)?;
            return Ok(Some(Self::Keyboard(KeyboardMessage {
                kind,
                virtual_key: payload[5],
                scan_code: u16::from_le_bytes([payload[6], payload[7]]),
                character: u16::from_le_bytes([payload[8], payload[9]]),
                caps_lock_on: payload[10] != 0,
            })));
        }
        if !payload.starts_with(&[56, 124, 124, 124]) {
            return Ok(None);
        }
        if payload.len() > 256 || !payload.iter().all(|b| (32..=126).contains(b)) {
            return Err(MalformedInput);
        }
        let text = std::str::from_utf8(payload).map_err(|_| MalformedInput)?;
        let fields: Vec<&str> = text.split("|||").collect();
        let canonical = |field: &str| -> Result<i32, MalformedInput> {
            let value: i32 = field.parse().map_err(|_| MalformedInput)?;
            if value.to_string() == field { Ok(value) } else { Err(MalformedInput) }
        };
        if fields.len() < 2 || fields[0] != "8" {
            return Err(MalformedInput);
        }
        let message = canonical(fields[1])?;
        if [0x100, 0x101, 0x104, 0x105].contains(&message) {
            // Modern Windows still sends modifier keys through this VK-only
            // legacy branch. The two -1 sentinels are not scan/Unicode values.
            if fields.len() != 5 || fields[3] != "-1" || fields[4] != "-1" {
                return Err(MalformedInput);
            }
            let vk: u8 = fields[2].parse().map_err(|_| MalformedInput)?;
            if vk.to_string() != fields[2] {
                return Err(MalformedInput);
            }
            let kind = KeyboardKind::from_windows_message(message as u32).ok_or(MalformedInput)?;
            return Ok(Some(Self::Keyboard(KeyboardMessage { kind, virtual_key: vk, scan_code: 0, character: 0, caps_lock_on: false })));
        }
        if !(message == 0x200 || message == 0x20a || MouseButton::from_raw(message).is_some()) {
            return Ok(None);
        }
        let expected = if message == 0x200 { 5 } else { 4 };
        if fields.len() != expected {
            return Err(MalformedInput);
        }
        let values: Vec<i32> = fields[2..].iter().map(|f| canonical(f)).collect::<Result<_, _>>()?;
        Ok(Some(Self::Mouse(match message {
            0x200 => MouseMessage::Move { w_param: values[0], x: values[1], y: values[2] },
            0x20a => MouseMessage::VerticalWheel { w_param: values[0], l_param: values[1] },
            _ => MouseMessage::Button { button: MouseButton::from_raw(message).ok_or(MalformedInput)?, w_param: values[0], l_param: values[1] },
        })))
    }
}
