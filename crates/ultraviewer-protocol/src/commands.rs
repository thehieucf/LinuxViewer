//! Verified viewer-to-host application commands. The session owns approval,
//! control permission and routing; these pure builders never execute an action.

use crate::screen::ScreenRegion;
use crate::util::utf16le;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMessage {
    pub payload: Vec<u8>,
    pub format_flag: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAction {
    ControlAltDelete,
    Restart,
    LockScreen,
}

impl RemoteAction {
    fn code(self) -> &'static str {
        match self {
            Self::ControlAltDelete => "22",
            Self::Restart => "40",
            Self::LockScreen => "53",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityPreset {
    Automatic,
    Speed,
    Quality,
}

impl QualityPreset {
    pub fn display_quality(self) -> i32 {
        match self {
            Self::Automatic => 45,
            Self::Speed => 30,
            Self::Quality => 70,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("malformed monitor information")]
    MalformedMonitorInformation,
    #[error("too many monitors")]
    TooManyMonitors,
    #[error("invalid monitor geometry")]
    InvalidMonitorGeometry,
    #[error("invalid side switch request")]
    InvalidSideSwitchRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideSwitchRequest {
    /// Peer-supplied identity and credential; never implies local approval.
    pub claimed_id: String,
    pub password: String,
}

pub struct RemoteControlCommands;

impl RemoteControlCommands {
    /// Original Actions menu uses Unicode Form1.SendString for these requests.
    pub fn action(action: RemoteAction) -> CommandMessage {
        unicode(action.code())
    }

    /// ChangeDisplayQuality sends an ASCII signed Long.
    pub fn quality(value: i32) -> CommandMessage {
        ascii(&format!("31|||{value}"))
    }

    /// The currently supported RGB555 converter mode is source value2.
    pub fn rgb555_color_mode() -> CommandMessage {
        ascii("32|||2")
    }

    pub fn quality_preset(preset: QualityPreset) -> Vec<CommandMessage> {
        vec![Self::rgb555_color_mode(), Self::quality(preset.display_quality())]
    }

    pub fn wallpaper(visible: bool) -> CommandMessage {
        unicode(&format!("23|||{}", if visible { -1 } else { 0 }))
    }

    /// Command28 toggles clsRemoteClient.SendMousePos.
    pub fn cursor_positions(enabled: bool) -> CommandMessage {
        ascii(&format!("28|||{}", i32::from(enabled)))
    }

    pub fn computer_sound(enabled: bool) -> CommandMessage {
        unicode(&format!("46|||{}", if enabled { -1 } else { 0 }))
    }

    /// A Windows viewer sends command19 as its last peer message when it leaves.
    pub fn end_session() -> CommandMessage {
        ascii("19")
    }

    /// Sends the currently hosted local ID/PIN to the authenticated Windows
    /// host. The caller must explicitly approve this credential disclosure.
    pub fn side_switch(own_id: &str, password: &str) -> Result<CommandMessage, CommandError> {
        checked_side_switch(own_id, password)?;
        Ok(unicode(&format!("54|||{own_id}|||{password}")))
    }

    /// Host-side input is already normalized from flagged UTF-16LE to UTF-8.
    pub fn decode_side_switch_request(payload: &[u8]) -> Result<Option<SideSwitchRequest>, CommandError> {
        if !payload.starts_with(b"54|||") {
            return Ok(None);
        }
        if payload.len() > 512 || !payload.iter().all(|b| (32..=126).contains(b)) {
            return Err(CommandError::InvalidSideSwitchRequest);
        }
        let text = std::str::from_utf8(payload).map_err(|_| CommandError::InvalidSideSwitchRequest)?;
        let fields: Vec<&str> = text.split("|||").collect();
        if fields.len() != 3 || fields[0] != "54" {
            return Err(CommandError::InvalidSideSwitchRequest);
        }
        checked_side_switch(fields[1], fields[2]).map(Some)
    }

    pub fn side_switch_acknowledgment() -> Vec<u8> {
        crate::multipart::encode(&[vec![54u8]]).expect("static payload")
    }

    /// Command0 part8: each monitor is left:top:right:bottom followed by '|'.
    /// Monitor selection crops the virtual desktop locally; it sends no command.
    pub fn monitors(payload: &[u8]) -> Result<Vec<RemoteMonitor>, CommandError> {
        Self::monitors_with(payload, 64)
    }

    pub fn monitors_with(payload: &[u8], maximum_monitors: usize) -> Result<Vec<RemoteMonitor>, CommandError> {
        if payload.len() > 16_384
            || maximum_monitors == 0
            || !payload.iter().all(|&b| b == b'-' || b == b':' || b == b'|' || b.is_ascii_digit())
        {
            return Err(CommandError::MalformedMonitorInformation);
        }
        if payload.is_empty() {
            return Ok(Vec::new());
        }
        let text = std::str::from_utf8(payload).map_err(|_| CommandError::MalformedMonitorInformation)?;
        let body = text.strip_suffix('|').ok_or(CommandError::MalformedMonitorInformation)?;
        let records: Vec<&str> = body.split('|').collect();
        if records.len() > maximum_monitors {
            return Err(CommandError::TooManyMonitors);
        }
        records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                let fields: Vec<&str> = record.split(':').collect();
                if fields.len() != 4 {
                    return Err(CommandError::MalformedMonitorInformation);
                }
                let numbers: Vec<i32> = fields
                    .iter()
                    .map(|f| f.parse::<i32>().map_err(|_| CommandError::MalformedMonitorInformation))
                    .collect::<Result<_, _>>()?;
                if numbers[2] <= numbers[0]
                    || numbers[3] <= numbers[1]
                    || i64::from(numbers[2]) - i64::from(numbers[0]) > i64::from(i32::MAX)
                    || i64::from(numbers[3]) - i64::from(numbers[1]) > i64::from(i32::MAX)
                {
                    return Err(CommandError::InvalidMonitorGeometry);
                }
                Ok(RemoteMonitor { index: index + 1, left: numbers[0], top: numbers[1], right: numbers[2], bottom: numbers[3] })
            })
            .collect()
    }
}

fn ascii(text: &str) -> CommandMessage {
    CommandMessage { payload: text.as_bytes().to_vec(), format_flag: false }
}

fn unicode(text: &str) -> CommandMessage {
    CommandMessage { payload: utf16le(text), format_flag: true }
}

fn checked_side_switch(id: &str, password: &str) -> Result<SideSwitchRequest, CommandError> {
    let valid_id = !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit()) && id.parse::<u64>().map_or(false, |v| v > 0);
    let valid_password = !password.is_empty()
        && password.len() <= 256
        && password.bytes().all(|b| (32..=126).contains(&b))
        && !password.contains("|||");
    if !valid_id || !valid_password {
        return Err(CommandError::InvalidSideSwitchRequest);
    }
    Ok(SideSwitchRequest { claimed_id: id.to_string(), password: password.to_string() })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteMonitor {
    /// One-based native menu index; zero is the native all-monitors selection.
    pub index: usize,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RemoteMonitor {
    pub fn width(&self) -> i64 {
        i64::from(self.right) - i64::from(self.left)
    }

    pub fn height(&self) -> i64 {
        i64::from(self.bottom) - i64::from(self.top)
    }

    /// Convert Windows virtual-desktop coordinates into bounded decoded pixels.
    pub fn region(&self, virtual_left: i32, virtual_top: i32, canvas_width: usize, canvas_height: usize) -> Result<ScreenRegion, CommandError> {
        let x = i64::from(self.left) - i64::from(virtual_left);
        let y = i64::from(self.top) - i64::from(virtual_top);
        if canvas_width == 0
            || canvas_height == 0
            || x < 0
            || y < 0
            || x + self.width() > canvas_width as i64
            || y + self.height() > canvas_height as i64
        {
            return Err(CommandError::InvalidMonitorGeometry);
        }
        Ok(ScreenRegion::new(x, y, self.width(), self.height()))
    }
}
