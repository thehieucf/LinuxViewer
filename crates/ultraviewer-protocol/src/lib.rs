//! UltraViewer-compatible protocol engine for LinuxViewer.
//!
//! This is a Rust port of the `UltraViewerProtocol` Swift module from
//! MacViewer. Wire formats, limits and session state machines follow the
//! original implementation; byte-level fixtures from its test suite are
//! reproduced under `tests/`.

pub mod audio;
pub mod chat;
pub mod clipboard;
pub mod commands;
pub mod crypto;
pub mod cursor;
pub mod dictionary;
pub mod direct;
pub mod frame;
pub mod host;
pub mod input;
pub mod login;
pub mod lookup;
pub mod multipart;
pub mod registration;
pub mod relay;
pub mod screen;
pub mod session;
pub mod transport;
pub mod zstd;

mod jpeg;
mod tls;
mod util;

pub use audio::{ComputerAudioMessage, G722Decoder};
pub use commands::{RemoteControlCommands, RemoteMonitor, SideSwitchRequest};
pub use cursor::{CursorFile, CursorMessage};
pub use input::{KeyboardKind, KeyboardMessage, MouseButton, MouseMessage};
pub use registration::{CaptchaChallenge, ClientRegistrationReply, ClientRegistrationRequest};
pub use relay::RelayIPv4Address;
pub use screen::{ModernScreenBitmap, ModernScreenRenderer, ScreenRegion};

/// Version advertised by this client, matching the reference implementation.
pub const CLIENT_VERSION: &str = "6.6.133";
/// Default UltraViewer command service.
pub const COMMAND_HOST: &str = "functions3.ultraviewer.net";
