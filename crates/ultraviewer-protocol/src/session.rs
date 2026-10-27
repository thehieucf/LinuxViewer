//! Viewer relay session with an optional encrypted LAN candidate. Ordinary
//! partner Login and complete initialization are required on either channel.
//!
//! The Swift original is an actor with reentrant awaits; this port is a
//! single tokio task owning all state. Network readers and timers feed one
//! inbox, so ordering is explicit and no generation tokens are needed beyond
//! connection identities.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use base64::Engine;
use rand::Rng;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::audio::{ComputerAudioMessage, G722Decoder};
use crate::chat::{self, ChatEvent};
use crate::clipboard::{self, ViewerEncoding};
use crate::commands::{CommandMessage, RemoteControlCommands, RemoteMonitor};
use crate::crypto::LegacyCommandCipher;
use crate::cursor::CursorMessage;
use crate::direct::{CandidateId, DirectEvent, DirectTransport, LanDirectUpgradeState};
use crate::lookup::{DirectDetails, LookupCredential, LookupReply, LookupRequest, PeerInfo};
use crate::login::{LoginContext, LoginCredential, PartnerLoginMessage};
use crate::multipart;
use crate::registration::{CaptchaChallenge, ClientRegistrationReply, ClientRegistrationRequest};
use crate::relay::{self, encode_forward, encode_system, HttpMethod, RelayEnvelope, RelayIPv4Address, RelayKeepalive};
use crate::screen::{ModernScreenBitmap, ModernScreenFrameMessage, ModernScreenInitialization, ModernScreenLimits, ModernScreenRenderer, ScreenDump};
use crate::transport::{self, Connection, OpenOptions, ReaderEvent, Security, TransportError};
use crate::util::version_at_least;
use crate::{dictionary, COMMAND_HOST};

#[derive(Debug, Clone)]
pub struct SessionConfiguration {
    pub registration: ClientRegistrationRequest,
    pub command_host: String,
    pub listener_port: u16,
    pub local_screen_width: i32,
    pub local_screen_height: i32,
    pub display_name: String,
    pub enable_lan_direct: bool,
    /// When this computer is also sharing, look up the partner through the
    /// sharing registration instead of registering the same ID twice (the
    /// service routes incoming requests to the newest registration).
    pub shared_command: Option<crate::host::CommandPort>,
}

impl SessionConfiguration {
    pub fn new(registration: ClientRegistrationRequest, local_screen_width: i32, local_screen_height: i32, display_name: &str) -> Self {
        Self {
            registration,
            command_host: COMMAND_HOST.into(),
            listener_port: 443,
            local_screen_width,
            local_screen_height,
            display_name: display_name.into(),
            enable_lan_direct: true,
            shared_command: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSummary {
    pub display_name: Option<String>,
    pub version: String,
    pub width: usize,
    pub height: usize,
    pub virtual_left: i32,
    pub virtual_top: i32,
    pub supports_binary_keyboard: bool,
    pub allows_remote_input: bool,
    pub monitors: Vec<RemoteMonitor>,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Status(String),
    Accepted(PeerSummary),
    ScreenChanged(PeerSummary),
    InputPermission(bool),
    Chat(String),
    Typing(bool),
    Clipboard(String),
    ComputerAudio(Vec<u8>),
    Cursor(CursorMessage),
    /// The session ended; `None` is an orderly close.
    Closed(Option<SessionError>),
}

/// The newest decoded screen. `damage` bounds changed pixels since the
/// previous update, when known.
#[derive(Debug, Clone)]
pub struct FrameUpdate {
    pub serial: u64,
    pub bitmap: ModernScreenBitmap,
    pub damage: Option<(usize, usize, usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("A remote session is already open.")]
    AlreadyOpen,
    #[error("Enter the remote computer's UltraViewer ID and password.")]
    InvalidPartner,
    #[error("The remote session is not connected.")]
    NotConnected,
    #[error("The remote connection timed out.")]
    TimedOut,
    #[error("The remote computer disconnected.")]
    Closed,
    #[error("UltraViewer rejected the partner password.")]
    WrongPassword,
    #[error("UltraViewer declined the partner lookup (code {0}).")]
    LookupRejected(i32),
    #[error("UltraViewer returned an incomplete partner address.")]
    InvalidLookupRoute,
    #[error("The remote computer blocked this connection.")]
    PartnerBlocked,
    #[error("This UltraViewer version is not supported yet.")]
    UnsupportedVersion,
    #[error("The remote computer returned incomplete screen information.")]
    MalformedInitialization,
    #[error("The remote session could not deliver its status updates.")]
    ConsumerTooSlow,
    #[error("The remote computer has enabled view-only access.")]
    ViewOnly,
    #[error("{0}")]
    Transport(TransportError),
    #[error("The remote computer sent an invalid message: {0}")]
    Protocol(String),
    #[error("This computer's client profile is invalid: {0}")]
    Registration(String),
    #[error("UltraViewer asks you to solve a captcha before it gives this computer an ID.")]
    CaptchaRequired(CaptchaChallenge),
}

impl From<TransportError> for SessionError {
    fn from(error: TransportError) -> Self {
        SessionError::Transport(error)
    }
}

macro_rules! protocol_error {
    ($($t:ty),*) => {$(
        impl From<$t> for SessionError {
            fn from(error: $t) -> Self { SessionError::Protocol(error.to_string()) }
        }
    )*};
}
protocol_error!(
    crate::multipart::MultipartError,
    crate::relay::EnvelopeError,
    crate::lookup::LookupError,
    crate::login::LoginError,
    crate::screen::ScreenError,
    crate::chat::ChatError,
    crate::clipboard::ClipboardError,
    crate::audio::G722Error,
    crate::direct::DirectError,
    crate::relay::InvalidHost
);

impl From<crate::registration::RegistrationError> for SessionError {
    fn from(error: crate::registration::RegistrationError) -> Self {
        SessionError::Registration(error.to_string())
    }
}

type Reply = oneshot::Sender<Result<(), SessionError>>;

enum Command {
    Send { payload: Vec<u8>, reply: Reply },
    SendChat { text: String, reply: Reply },
    SendTyping { typing: bool, reply: Reply },
    SendCommand { message: CommandMessage, requires_control: bool, reply: Reply },
    SetAudio { enabled: bool, reply: Reply },
    SetClipboardSharing { enabled: bool, reply: Reply },
    SendClipboard { text: String, reply: Reply },
    RefreshScreen { reply: Reply },
    Close { reply: oneshot::Sender<()> },
}

/// Cheap, cloneable control surface for a running session.
#[derive(Clone)]
pub struct SessionHandle {
    commands: mpsc::UnboundedSender<Command>,
}

impl SessionHandle {
    async fn request(&self, make: impl FnOnce(Reply) -> Command) -> Result<(), SessionError> {
        let (reply, response) = oneshot::channel();
        self.commands.send(make(reply)).map_err(|_| SessionError::NotConnected)?;
        response.await.unwrap_or(Err(SessionError::NotConnected))
    }

    /// Sends an application payload with the false format flag, such as input.
    pub async fn send(&self, payload: Vec<u8>) -> Result<(), SessionError> {
        self.request(|reply| Command::Send { payload, reply }).await
    }

    pub async fn send_chat(&self, text: String) -> Result<(), SessionError> {
        self.request(|reply| Command::SendChat { text, reply }).await
    }

    pub async fn send_typing(&self, typing: bool) -> Result<(), SessionError> {
        self.request(|reply| Command::SendTyping { typing, reply }).await
    }

    pub async fn send_command(&self, message: CommandMessage, requires_control: bool) -> Result<(), SessionError> {
        self.request(|reply| Command::SendCommand { message, requires_control, reply }).await
    }

    pub async fn set_computer_audio(&self, enabled: bool) -> Result<(), SessionError> {
        self.request(|reply| Command::SetAudio { enabled, reply }).await
    }

    pub async fn set_clipboard_sharing(&self, enabled: bool) -> Result<(), SessionError> {
        self.request(|reply| Command::SetClipboardSharing { enabled, reply }).await
    }

    pub async fn send_clipboard(&self, text: String) -> Result<(), SessionError> {
        self.request(|reply| Command::SendClipboard { text, reply }).await
    }

    pub async fn refresh_screen(&self) -> Result<(), SessionError> {
        self.request(|reply| Command::RefreshScreen { reply }).await
    }

    /// Tells an active host that this viewer is leaving, then closes.
    pub async fn close(&self) {
        let (reply, done) = oneshot::channel();
        if self.commands.send(Command::Close { reply }).is_ok() {
            let _ = tokio::time::timeout(Duration::from_secs(5), done).await;
        }
    }

    pub fn is_running(&self) -> bool {
        !self.commands.is_closed()
    }
}

pub struct SessionStreams {
    pub handle: SessionHandle,
    pub events: mpsc::Receiver<SessionEvent>,
    pub frames: watch::Receiver<Option<FrameUpdate>>,
}

/// Starts a viewer session on the current tokio runtime.
pub fn connect(configuration: SessionConfiguration, partner_id: &str, password: &str) -> Result<SessionStreams, SessionError> {
    if partner_id.is_empty() || partner_id.len() > 64 || !partner_id.bytes().all(|b| b.is_ascii_digit()) || password.is_empty() {
        return Err(SessionError::InvalidPartner);
    }
    configuration.registration.encode()?;
    if configuration.registration.uses_proxy {
        return Err(SessionError::InvalidLookupRoute);
    }
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::channel(64);
    let (frame_tx, frame_rx) = watch::channel(None);
    let (inbox_tx, inbox_rx) = mpsc::channel(64);
    let mut rng = rand::thread_rng();
    let actor = Actor {
        phase: Phase::Registering,
        configuration,
        target_id: partner_id.to_string(),
        password: password.to_string(),
        connection_hash: random_string(&mut rng, 16, "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"),
        correlation: rng.gen_range(1..=i32::MAX),
        events: event_tx,
        frames: frame_tx,
        frame_serial: 0,
        screen_failures: 0,
        frames_since_screen_failure: 0,
        screen_dump: ScreenDump::from_environment(),
        inbox: inbox_tx,
        command: None,
        peer: None,
        peer_id: 0,
        direct: None,
        direct_generation: 0,
        direct_state: LanDirectUpgradeState::default(),
        direct_pending_initialization: None,
        direct_deadline: None,
        direct_window: None,
        relay_ready: false,
        relay_failed: false,
        relay_initialization_suspended: false,
        pending_relay_initialization: None,
        deadline: None,
        heartbeat_at: None,
        command_keepalive_at: None,
        renderer: None,
        registration_reply: None,
        peer_info: None,
        target_address: None,
        target_port: None,
        remote_view_only: false,
        clipboard_allowed: false,
        audio_decoder: None,
        audio_enabled: false,
        relay_ping_started: None,
        peer_ping_started: None,
        heartbeat_ticks: 0,
        diagnostics: Diagnostics::default(),
    };
    tokio::spawn(actor.run(command_rx, inbox_rx));
    Ok(SessionStreams { handle: SessionHandle { commands: command_tx }, events: event_rx, frames: frame_rx })
}

#[derive(Debug, Default, Clone)]
pub struct Diagnostics {
    pub registration_accepted: bool,
    pub lookup_status: Option<i32>,
    pub peer_transport_opened: bool,
    pub login_sent: bool,
    pub peer_accepted: bool,
    pub decoded_frames: u64,
    pub screen_refreshes: u32,
    pub last_screen_failure: Option<String>,
    pub relay_round_trip_ms: Option<f64>,
    pub peer_round_trip_ms: Option<f64>,
    pub lan_direct_advertised: bool,
    pub lan_direct_candidates: u32,
    pub lan_direct_login_sent: bool,
    pub lan_direct_active: bool,
    pub lan_direct_fallbacks: u32,
    pub ignored_sources: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Closed,
    Registering,
    Lookup,
    Login,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerSource {
    Relay,
    Direct(CandidateId),
}

enum Inbox {
    SharedRegistration(Option<ClientRegistrationReply>),
    SharedLookup(Result<Vec<u8>, SessionError>),
    CommandOpened(Result<Connection, TransportError>),
    CommandReader(ReaderEvent),
    PeerOpened(u64, Result<Connection, TransportError>),
    PeerReader(u64, ReaderEvent),
    Direct(u64, DirectEvent),
}

struct PendingRelayInitialization {
    payload: Vec<u8>,
    parts: Vec<Vec<u8>>,
    address: RelayIPv4Address,
    port: u16,
}

const COMMAND_KEEPALIVE: Duration = Duration::from_secs(30);
/// Decoded frames after which earlier screen failures no longer count.
const GOOD_FRAMES_FORGIVING_SCREEN_FAILURES: u32 = 100;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

struct Actor {
    phase: Phase,
    configuration: SessionConfiguration,
    target_id: String,
    password: String,
    connection_hash: String,
    correlation: i32,
    events: mpsc::Sender<SessionEvent>,
    frames: watch::Sender<Option<FrameUpdate>>,
    frame_serial: u64,
    /// Screen decoding failures since the last run of good frames.
    screen_failures: u32,
    frames_since_screen_failure: u32,
    screen_dump: Option<ScreenDump>,
    inbox: mpsc::Sender<Inbox>,
    command: Option<Connection>,
    peer: Option<Connection>,
    peer_id: u64,
    direct: Option<DirectTransport<Inbox>>,
    direct_generation: u64,
    direct_state: LanDirectUpgradeState,
    direct_pending_initialization: Option<(CandidateId, Vec<u8>)>,
    direct_deadline: Option<(CandidateId, Instant)>,
    direct_window: Option<Instant>,
    relay_ready: bool,
    relay_failed: bool,
    relay_initialization_suspended: bool,
    pending_relay_initialization: Option<PendingRelayInitialization>,
    deadline: Option<Instant>,
    heartbeat_at: Option<Instant>,
    /// The command service drops idle registrations after roughly a minute.
    command_keepalive_at: Option<Instant>,
    renderer: Option<ModernScreenRenderer>,
    registration_reply: Option<ClientRegistrationReply>,
    peer_info: Option<PeerInfo>,
    target_address: Option<RelayIPv4Address>,
    target_port: Option<u16>,
    remote_view_only: bool,
    clipboard_allowed: bool,
    audio_decoder: Option<G722Decoder>,
    audio_enabled: bool,
    relay_ping_started: Option<Instant>,
    peer_ping_started: Option<Instant>,
    heartbeat_ticks: u64,
    diagnostics: Diagnostics,
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn random_string(rng: &mut impl Rng, count: usize, alphabet: &str) -> String {
    let alphabet = alphabet.as_bytes();
    (0..count).map(|_| alphabet[rng.gen_range(0..alphabet.len())] as char).collect()
}

impl Actor {
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>, mut inbox: mpsc::Receiver<Inbox>) {
        self.begin();
        while self.phase != Phase::Closed {
            let direct_deadline = self.direct_deadline;
            tokio::select! {
                biased;
                command = commands.recv() => match command {
                    Some(command) => self.handle_command(command).await,
                    None => self.close_gracefully().await,
                },
                Some(message) = inbox.recv() => self.handle_inbox(message).await,
                _ = sleep_until_opt(self.deadline) => self.expired().await,
                _ = sleep_until_opt(self.heartbeat_at) => self.send_heartbeat().await,
                _ = sleep_until_opt(self.command_keepalive_at) => self.send_command_keepalive().await,
                _ = sleep_until_opt(direct_deadline.map(|(_, at)| at)) => {
                    self.direct_deadline = None;
                    if let Some((id, _)) = direct_deadline {
                        if self.direct_state.selected != Some(id) {
                            self.direct_candidate_ended(id).await;
                        }
                    }
                }
                _ = sleep_until_opt(self.direct_window) => {
                    self.direct_window = None;
                    // LAN discovery is an initial optimization, not a permanently
                    // exposed listener.
                    if self.direct_state.selected.is_none() {
                        if let Some(direct) = self.direct.as_mut() {
                            direct.stop_listening();
                        }
                    }
                }
            }
        }
        // Answer anyone still waiting after the session closed.
        commands.close();
        while let Ok(command) = commands.try_recv() {
            reject(command);
        }
    }

    // ------------------------------------------------------------- commands

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Send { payload, reply } => {
                // During an authenticated screen refresh the adapter must still
                // be able to release held input.
                let result = if self.diagnostics.peer_accepted && matches!(self.phase, Phase::Active | Phase::Login) {
                    self.send_to_peer(&payload, false, None).await
                } else {
                    Err(SessionError::NotConnected)
                };
                let _ = reply.send(result);
            }
            Command::SendChat { text, reply } => {
                let result = match self.require_active() {
                    Ok(()) => match chat::viewer_to_host(&text) {
                        Ok(message) => self.send_to_peer(&message.payload, message.format_flag, None).await,
                        Err(error) => Err(error.into()),
                    },
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
            Command::SendTyping { typing, reply } => {
                let result = match self.require_active() {
                    Ok(()) => {
                        let message = chat::viewer_to_host_typing(typing);
                        self.send_to_peer(&message.payload, message.format_flag, None).await
                    }
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
            Command::SendCommand { message, requires_control, reply } => {
                let result = if self.phase != Phase::Active {
                    Err(SessionError::NotConnected)
                } else if requires_control && self.remote_view_only {
                    Err(SessionError::ViewOnly)
                } else {
                    self.send_to_peer(&message.payload, message.format_flag, None).await
                };
                let _ = reply.send(result);
            }
            Command::SetAudio { enabled, reply } => {
                let result = match self.require_active() {
                    Ok(()) => {
                        let previous = self.audio_enabled;
                        self.audio_enabled = enabled;
                        let message = RemoteControlCommands::computer_sound(enabled);
                        let result = self.send_to_peer(&message.payload, message.format_flag, None).await;
                        if result.is_err() {
                            self.audio_enabled = previous;
                        }
                        result
                    }
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
            Command::SetClipboardSharing { enabled, reply } => {
                let result = if self.diagnostics.peer_accepted && matches!(self.phase, Phase::Active | Phase::Login) {
                    self.clipboard_allowed = enabled;
                    Ok(())
                } else {
                    Err(SessionError::NotConnected)
                };
                let _ = reply.send(result);
            }
            Command::SendClipboard { text, reply } => {
                let result = if self.phase != Phase::Active || !self.clipboard_allowed {
                    Err(SessionError::NotConnected)
                } else {
                    match clipboard::viewer_to_host(&text, ViewerEncoding::Compressed) {
                        Ok(message) => self.send_to_peer(&message.payload, message.format_flag, None).await,
                        Err(error) => Err(error.into()),
                    }
                };
                let _ = reply.send(result);
            }
            Command::RefreshScreen { reply } => {
                let result = self.refresh_screen().await;
                if let Err(error) = &result {
                    if !matches!(error, SessionError::NotConnected) {
                        self.finish(Some(error.clone())).await;
                    }
                }
                let _ = reply.send(result);
            }
            Command::Close { reply } => {
                self.close_gracefully().await;
                let _ = reply.send(());
            }
        }
    }

    fn require_active(&self) -> Result<(), SessionError> {
        if self.phase == Phase::Active {
            Ok(())
        } else {
            Err(SessionError::NotConnected)
        }
    }

    async fn close_gracefully(&mut self) {
        if self.phase == Phase::Active {
            let message = RemoteControlCommands::end_session();
            let _ = tokio::time::timeout(Duration::from_secs(2), self.send_to_peer(&message.payload, message.format_flag, None)).await;
        }
        self.finish(None).await;
    }

    /// Renegotiate a complete screen using the established clear-mode Login
    /// path. Retain the authenticated identity, but suspend control until init.
    async fn refresh_screen(&mut self) -> Result<(), SessionError> {
        if self.phase != Phase::Active {
            return Err(SessionError::NotConnected);
        }
        self.diagnostics.screen_refreshes += 1;
        self.renderer = None;
        self.phase = Phase::Login;
        self.emit(SessionEvent::InputPermission(false)).await?;
        self.emit(SessionEvent::Status("Refreshing the remote screen…".into())).await?;
        self.arm_deadline();
        self.login(-1, true, None).await
    }

    // ------------------------------------------------------------- inbox

    async fn handle_inbox(&mut self, message: Inbox) {
        match message {
            Inbox::SharedRegistration(reply) => match reply {
                Some(reply) => {
                    if let Err(error) = self.registered(reply).await {
                        self.finish(Some(error)).await;
                    }
                }
                None => {
                    // Sharing stopped meanwhile: register this session itself.
                    self.configuration.shared_command = None;
                    self.begin();
                }
            },
            Inbox::SharedLookup(result) => {
                let result = match result {
                    Ok(payload) => self.lookup_replied(&payload).await,
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    self.finish(Some(error)).await;
                }
            }
            Inbox::CommandOpened(result) => match result {
                Ok(connection) => {
                    if self.phase == Phase::Closed {
                        return;
                    }
                    self.command = Some(connection);
                    self.arm_deadline();
                    let envelope = match self.configuration.registration.relay_envelope(&LegacyCommandCipher::desktop_server_commands()) {
                        Ok(envelope) => envelope,
                        Err(error) => return self.finish(Some(error.into())).await,
                    };
                    if let Err(error) = self.command_send(&envelope).await {
                        self.finish(Some(error)).await;
                    }
                }
                Err(error) => self.finish(Some(error.into())).await,
            },
            Inbox::CommandReader(event) => match event {
                ReaderEvent::Frame(frame) => {
                    if let Err(error) = self.consume_command(&frame).await {
                        self.finish(Some(error)).await;
                    }
                }
                ReaderEvent::Closed(error) => {
                    self.command = None;
                    self.command_keepalive_at = None;
                    // The command service only carries registration, lookup and
                    // keepalives; an established peer stream continues without it.
                    if self.phase == Phase::Active {
                        log::warn!("command connection closed during an active session: {error:?}");
                    } else {
                        self.finish(Some(error.map(SessionError::from).unwrap_or(SessionError::Closed))).await;
                    }
                }
            },
            Inbox::PeerOpened(id, result) => {
                if id != self.peer_id || self.phase == Phase::Closed {
                    return;
                }
                match result {
                    Ok(connection) => {
                        self.peer = Some(connection);
                        self.relay_ready = true;
                        self.relay_failed = false;
                        self.diagnostics.peer_transport_opened = true;
                        if self.direct_state.selected.is_none() && self.direct_state.login_candidate.is_none() {
                            let result = async {
                                self.emit(SessionEvent::Status("Waiting for the remote computer to accept the session…".into())).await?;
                                if self.phase != Phase::Active {
                                    // Hosts that ask their user (MacViewer, LinuxViewer)
                                    // need time for approval and screen selection.
                                    self.deadline = Some(Instant::now() + Duration::from_secs(60));
                                }
                                self.login(0, false, Some(PeerSource::Relay)).await
                            }
                            .await;
                            if let Err(error) = result {
                                self.relay_ended(error).await;
                            }
                        }
                    }
                    Err(error) => self.relay_ended(error.into()).await,
                }
            }
            Inbox::PeerReader(id, event) => {
                if id != self.peer_id || self.peer.is_none() {
                    return;
                }
                match event {
                    ReaderEvent::Frame(frame) => {
                        if let Err(error) = self.consume_peer(&frame).await {
                            self.relay_ended(error).await;
                        }
                    }
                    ReaderEvent::Closed(error) => {
                        self.relay_ended(error.map(SessionError::from).unwrap_or(SessionError::Closed)).await;
                    }
                }
            }
            Inbox::Direct(generation, event) => {
                if generation == self.direct_generation {
                    self.consume_direct(event).await;
                }
            }
        }
    }

    fn begin(&mut self) {
        if let Some(port) = self.configuration.shared_command.clone() {
            let inbox = self.inbox.clone();
            tokio::spawn(async move {
                let reply = port.registration().await;
                let _ = inbox.send(Inbox::SharedRegistration(reply)).await;
            });
            return;
        }
        let inbox = self.inbox.clone();
        let mut options = OpenOptions::new(self.configuration.command_host.clone(), self.configuration.listener_port,
                                           Security::ultraviewer(COMMAND_HOST));
        options.initial_request = relay::http_preamble(COMMAND_HOST, HttpMethod::Post).ok();
        options.expects_http_response = true;
        let events = self.events.clone();
        tokio::spawn(async move {
            let _ = events.send(SessionEvent::Status("Registering this computer…".into())).await;
            let result = transport::open_command_connection(options, Duration::from_secs(3), inbox.clone(), Inbox::CommandReader).await;
            let _ = inbox.send(Inbox::CommandOpened(result)).await;
        });
    }

    async fn command_send(&mut self, payload: &[u8]) -> Result<(), SessionError> {
        let connection = self.command.as_mut().ok_or(SessionError::NotConnected)?;
        connection.send(payload).await.map_err(SessionError::from)
    }

    async fn consume_command(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        let RelayEnvelope::System { payload } = RelayEnvelope::decode(bytes)? else { return Ok(()) };
        if self.phase == Phase::Registering && payload.starts_with(b"60:") {
            return Err(SessionError::CaptchaRequired(CaptchaChallenge::parse(&payload)?));
        } else if self.phase == Phase::Registering && payload.starts_with(b"3:") {
            let reply = ClientRegistrationReply::parse(&payload)?;
            self.command_keepalive_at = Some(Instant::now() + COMMAND_KEEPALIVE);
            self.registered(reply).await?;
        } else if self.phase == Phase::Lookup && payload.starts_with(b"8:") {
            self.lookup_replied(&payload).await?;
        }
        Ok(())
    }

    async fn registered(&mut self, reply: ClientRegistrationReply) -> Result<(), SessionError> {
        if self.phase != Phase::Registering {
            return Ok(());
        }
        log::info!("viewer registered as {} (public {}:{})", reply.assigned_id_text(), reply.public_address, reply.public_port);
        self.diagnostics.registration_accepted = true;
        self.phase = Phase::Lookup;
        self.emit(SessionEvent::Status("Finding the remote computer…".into())).await?;
        let direct_details = self.prepare_lan_direct(&reply).await;
        let config = &self.configuration;
        let lookup = LookupRequest {
            target_id: self.target_id.clone(),
            credential: LookupCredential::Password(self.password.clone()),
            client_version: config.registration.client_version.clone(),
            license_context: config.registration.license_context1.clone(),
            additional_license_context: config.registration.license_context2.clone(),
            local_id: reply.assigned_id_text(),
            local_password: config.registration.password.clone(),
            target_session_id: -1,
            correlation_number: self.correlation,
            is_reconnect: false,
            referral_id_text: "0".into(),
            payment_type_text: "0".into(),
            date_time_offset_minutes: local_utc_offset_minutes(),
            object_dictionary_count: 1,
            fixed_ping_list: String::new(),
            direct_details,
        };
        self.registration_reply = Some(reply);
        let plaintext = lookup.encode()?;
        self.arm_deadline();
        match self.configuration.shared_command.clone() {
            Some(port) => {
                let inbox = self.inbox.clone();
                let correlation = self.correlation;
                tokio::spawn(async move {
                    let result = port.lookup(plaintext, correlation).await.map_err(|_| SessionError::NotConnected);
                    let _ = inbox.send(Inbox::SharedLookup(result)).await;
                });
            }
            None => {
                let encrypted = LegacyCommandCipher::desktop_server_commands().encrypt(&plaintext);
                self.command_send(&encode_system(&encrypted, Some(false))).await?;
            }
        }
        Ok(())
    }

    async fn lookup_replied(&mut self, payload: &[u8]) -> Result<(), SessionError> {
        if self.phase != Phase::Lookup {
            return Ok(());
        }
        let reply = LookupReply::parse(payload)?;
        log::debug!("lookup reply status {} with {} fields", reply.status, reply.fields.len());
        if reply.correlation_number() != Some(self.correlation) {
            return Ok(());
        }
        self.diagnostics.lookup_status = Some(reply.status);
        if reply.status == 2 {
            return Err(SessionError::WrongPassword);
        }
        let info = reply.peer_info()?.ok_or(SessionError::LookupRejected(reply.status))?;
        let address = RelayIPv4Address::parse(&info.public_ip).map_err(|_| SessionError::InvalidLookupRoute)?;
        let port = u16::try_from(info.target_tcp_port).ok().filter(|&p| p != 0).ok_or(SessionError::InvalidLookupRoute)?;
        if info.relay_ip.is_empty() {
            return Err(SessionError::InvalidLookupRoute);
        }
        self.peer_info = Some(info.clone());
        self.target_address = Some(address);
        self.target_port = Some(port);
        self.phase = Phase::Login;
        self.deadline = None;
        self.configure_lan_direct(&info).await;
        if self.phase == Phase::Closed {
            return Ok(());
        }
        self.begin_peer(&info);
        Ok(())
    }

    fn begin_peer(&mut self, info: &PeerInfo) {
        self.peer_id += 1;
        let id = self.peer_id;
        let hostname = if info.relay_hostname.is_empty() { COMMAND_HOST.to_string() } else { info.relay_hostname.clone() };
        let mut options = OpenOptions::new(info.relay_ip.clone(), self.configuration.listener_port, Security::ultraviewer(&hostname));
        options.initial_request = relay::http_preamble(&hostname, HttpMethod::Post).ok();
        options.expects_http_response = true;
        let inbox = self.inbox.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            let _ = events.send(SessionEvent::Status("Connecting to the remote computer…".into())).await;
            let result = transport::open(options, inbox.clone(), move |event| Inbox::PeerReader(id, event)).await;
            let _ = inbox.send(Inbox::PeerOpened(id, result)).await;
        });
    }

    // ------------------------------------------------------------- LAN direct

    async fn prepare_lan_direct(&mut self, registration: &ClientRegistrationReply) -> Option<DirectDetails> {
        let local = self.configuration.registration.local_address;
        if !self.configuration.enable_lan_direct || !local.is_lan() {
            return None;
        }
        // Independently generated per-session transport key, carried only in the
        // already encrypted command lookup. Never log or capture it.
        let key = random_string(&mut rand::thread_rng(), 14, "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz");
        self.direct_generation += 1;
        let generation = self.direct_generation;
        match DirectTransport::start(local, &key, false, self.inbox.clone(), move |event| Inbox::Direct(generation, event)).await {
            Ok((transport, advertisement)) => {
                self.direct = Some(transport);
                self.diagnostics.lan_direct_advertised = true;
                Some(DirectDetails {
                    command_public_ip: registration.public_address.to_string(),
                    command_public_source_port: i32::from(registration.public_port),
                    local_ip: advertisement.local_address.to_string(),
                    local_port: i32::from(advertisement.port),
                    encryption_key: key,
                })
            }
            // A LAN optimization cannot make relay registration fail.
            Err(_) => None,
        }
    }

    async fn configure_lan_direct(&mut self, peer: &PeerInfo) {
        let Some(registration) = &self.registration_reply else { return };
        if self.direct.is_none() {
            return;
        }
        let Some(address) = LanDirectUpgradeState::peer_lan_address(&peer.public_ip, registration.public_address,
                                                                     peer.direct_details_field.as_deref(), &peer.lan_ip) else {
            self.direct = None;
            self.direct_generation += 1;
            self.direct_state = LanDirectUpgradeState::default();
            return;
        };
        let rejected = self.direct_state.expect(address);
        if let Some(direct) = self.direct.as_mut() {
            for id in rejected {
                direct.close_candidate(id);
            }
        }
        self.direct_window = Some(Instant::now() + Duration::from_secs(15));
        self.begin_direct_login_if_possible().await;
    }

    async fn consume_direct(&mut self, event: DirectEvent) {
        if self.phase == Phase::Closed || self.direct.is_none() {
            return;
        }
        match event {
            DirectEvent::Accepted { id, stream, address } => {
                self.diagnostics.lan_direct_candidates += 1;
                let attached = self.diagnostics.lan_direct_candidates <= 4
                    && self.direct_state.connected(id, address)
                    && self.direct.as_mut().is_some_and(|d| d.attach(id, stream));
                if !attached {
                    self.direct_state.remove(id);
                    return;
                }
                self.begin_direct_login_if_possible().await;
            }
            DirectEvent::Message { id, envelope } => {
                if self.direct_state.login_candidate == Some(id) && !self.direct_state.login_sent {
                    // A response can race the Login write. Hold one small init,
                    // but never select it before Login was written.
                    if self.direct_pending_initialization.is_none() && envelope.payload.len() <= 65_536 {
                        if let Ok(parts) = multipart::decode(&envelope.payload) {
                            if parts.first().map(Vec::as_slice) == Some(&[0u8][..]) {
                                self.direct_pending_initialization = Some((id, envelope.payload));
                            }
                        }
                    }
                    return;
                }
                if self.direct_state.selected != Some(id) && !self.direct_state.permits_initialization(id) {
                    return;
                }
                if let Err(error) = self.consume_application(envelope.payload, None, PeerSource::Direct(id), None, None).await {
                    if self.phase == Phase::Closed {
                        return;
                    }
                    let fatal = matches!(error, SessionError::Closed | SessionError::PartnerBlocked | SessionError::WrongPassword);
                    if self.direct_state.selected == Some(id) && fatal {
                        self.finish(Some(error)).await;
                    } else {
                        self.direct_candidate_ended(id).await;
                    }
                }
            }
            DirectEvent::Disconnected { id } => self.direct_candidate_ended(id).await,
            DirectEvent::ListenerEnded => {}
        }
    }

    fn begin_direct_login_if_possible<'a>(&'a mut self) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if !matches!(self.phase, Phase::Login | Phase::Active) || self.peer_info.is_none() || self.direct.is_none() {
                return;
            }
            let Some(id) = self.direct_state.begin_login() else { return };
            let previously_accepted = self.diagnostics.peer_accepted;
            self.direct_deadline = Some((id, Instant::now() + Duration::from_secs(8)));
            match self.login(if previously_accepted { -1 } else { 0 }, false, Some(PeerSource::Direct(id))).await {
                Ok(()) => self.direct_login_was_sent(id).await,
                Err(_) => self.direct_candidate_ended(id).await,
            }
        })
    }

    async fn direct_login_was_sent(&mut self, id: CandidateId) {
        if !self.direct_state.mark_login_sent(id) {
            return;
        }
        self.diagnostics.lan_direct_login_sent = true;
        if let Some((pending_id, payload)) = self.direct_pending_initialization.take() {
            if pending_id == id {
                if self.consume_application(payload, None, PeerSource::Direct(id), None, None).await.is_err() {
                    self.direct_candidate_ended(id).await;
                }
            }
        }
    }

    fn direct_candidate_ended<'a>(&'a mut self, id: CandidateId) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if self.phase == Phase::Closed || !self.direct_state.candidates.contains_key(&id) {
                return;
            }
            let was_login = self.direct_state.login_candidate == Some(id);
            let was_selected = self.direct_state.remove(id);
            if was_selected {
                self.renderer = None;
                self.phase = Phase::Login;
                self.relay_initialization_suspended = true;
                self.pending_relay_initialization = None;
                if let Err(error) = self.emit(SessionEvent::InputPermission(false)).await {
                    return self.finish(Some(error)).await;
                }
            }
            if was_login || was_selected {
                self.direct_deadline = None;
            }
            if self.direct_pending_initialization.as_ref().is_some_and(|(pending, _)| *pending == id) {
                self.direct_pending_initialization = None;
            }
            if let Some(direct) = self.direct.as_mut() {
                direct.close_candidate(id);
            }
            if was_selected {
                self.diagnostics.lan_direct_active = false;
                self.diagnostics.lan_direct_fallbacks += 1;
                if !self.relay_ready {
                    return self.finish(Some(SessionError::Closed)).await;
                }
                // A failed selected direct channel needs a fresh relay Login/init,
                // not plaintext retry or reuse of a partial direct frame cache.
                let result = async {
                    self.emit(SessionEvent::Status("Restoring the relay connection…".into())).await?;
                    self.arm_deadline();
                    self.login(-1, true, Some(PeerSource::Relay)).await
                }
                .await;
                if let Err(error) = result {
                    self.finish(Some(error)).await;
                }
            } else {
                if !self.relay_ready && self.relay_failed {
                    return self.finish(Some(SessionError::Closed)).await;
                }
                // If direct Login retargeted the native host, restore its relay
                // route without throwing away a still healthy relay renderer.
                if was_login && self.relay_ready {
                    if self.phase == Phase::Login {
                        self.arm_deadline();
                    }
                    let mode = if self.diagnostics.peer_accepted { -1 } else { 0 };
                    if let Err(error) = self.login(mode, false, Some(PeerSource::Relay)).await {
                        return self.relay_ended(error).await;
                    }
                }
                if self.phase == Phase::Closed {
                    return;
                }
                self.begin_direct_login_if_possible().await;
            }
        })
    }

    fn relay_ended<'a>(&'a mut self, error: SessionError) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if self.phase == Phase::Closed {
                return;
            }
            self.relay_ready = false;
            self.relay_failed = true;
            self.peer = None;
            self.peer_id += 1;
            if self.direct_state.selected.is_some() || self.direct_state.login_candidate.is_some() {
                return;
            }
            self.finish(Some(error)).await;
        })
    }

    // ------------------------------------------------------------- login & peer

    fn login<'a>(&'a mut self, control_mode: i16, clear_control_mode: bool, source: Option<PeerSource>) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            if self.phase == Phase::Closed {
                return Err(SessionError::NotConnected);
            }
            let (Some(reply), Some(peer)) = (&self.registration_reply, &self.peer_info) else {
                return Err(SessionError::NotConnected);
            };
            let config = &self.configuration;
            let context = LoginContext {
                screen_pixel_width: config.local_screen_width,
                screen_pixel_height: config.local_screen_height,
                desktop_metric: 0,
                screen_twips_per_pixel_x_text: "15".into(),
                screen_twips_per_pixel_y_text: "15".into(),
                form_vertical_inset: 0,
                form_horizontal_inset: 0,
                local_id: reply.assigned_id_text(),
                display_name: config.display_name.clone(),
                control_mode,
                clear_control_mode,
                local_user_sid: String::new(),
                connection_hash_key: self.connection_hash.clone(),
                display_colors: 2,
                display_quality: 45,
                remote_wallpaper_disabled: 0,
                has_desktop_token: false,
                partner_secret_token: peer.partner_secret_token.clone().unwrap_or_default(),
                client_version: config.registration.client_version.clone(),
                desktop_counter: 0,
            };
            let nonce = random_string(&mut rand::thread_rng(), 8, "0123456789");
            let message = PartnerLoginMessage { credential: LoginCredential::Password { value: self.password.clone(), nonce }, context };
            let destination = source.unwrap_or(match self.direct_state.selected {
                Some(id) => PeerSource::Direct(id),
                None => PeerSource::Relay,
            });
            self.send_to_peer(&message.encode()?, false, Some(destination)).await?;
            if self.phase == Phase::Closed {
                return Err(SessionError::Closed);
            }
            if destination == PeerSource::Relay && self.relay_initialization_suspended {
                self.relay_initialization_suspended = false;
                if let Some(pending) = self.pending_relay_initialization.take() {
                    self.consume_application(pending.payload, Some(pending.parts), PeerSource::Relay, Some(pending.address), Some(pending.port)).await?;
                }
            }
            self.diagnostics.login_sent = true;
            Ok(())
        })
    }

    async fn consume_peer(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        match RelayEnvelope::decode(bytes)? {
            RelayEnvelope::System { payload } => {
                if payload == b"pong" {
                    if let Some(started) = self.relay_ping_started.take() {
                        self.diagnostics.relay_round_trip_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                Ok(())
            }
            RelayEnvelope::Forward { address, port, payload, .. } => {
                // The old relay stays alive as fallback, but it cannot overwrite
                // the renderer, permissions or input state of a direct peer.
                if self.direct_state.selected.is_some() {
                    return Ok(());
                }
                let known_source = Some(address) == self.target_address && Some(port) == self.target_port;
                let parts = match multipart::decode(&payload) {
                    Ok(parts) => parts,
                    Err(error) => {
                        if !known_source {
                            self.diagnostics.ignored_sources += 1;
                            return Ok(());
                        }
                        return Err(error.into());
                    }
                };
                let Some(&command) = parts.first().and_then(|p| p.first()) else {
                    if !known_source {
                        self.diagnostics.ignored_sources += 1;
                        return Ok(());
                    }
                    return Err(SessionError::MalformedInitialization);
                };
                if !(known_source || (command == 26 && parts.len() > 1 && parts[1] == self.connection_hash.as_bytes())) {
                    self.diagnostics.ignored_sources += 1;
                    return Ok(());
                }
                if self.relay_initialization_suspended {
                    // Preserve only one bounded response that races the fallback
                    // Login write. No relay change is applied before it.
                    if known_source && command == 0 && payload.len() <= 65_536 && self.pending_relay_initialization.is_none() {
                        self.pending_relay_initialization = Some(PendingRelayInitialization { payload, parts, address, port });
                    }
                    return Ok(());
                }
                self.consume_application(payload, Some(parts), PeerSource::Relay, Some(address), Some(port)).await
            }
        }
    }

    fn consume_application<'a>(&'a mut self, payload: Vec<u8>, supplied_parts: Option<Vec<Vec<u8>>>, source: PeerSource,
                               relay_address: Option<RelayIPv4Address>, relay_port: Option<u16>)
                               -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            if self.phase == Phase::Closed {
                return Ok(());
            }
            let parts = match supplied_parts {
                Some(parts) => parts,
                None => multipart::decode(&payload)?,
            };
            let command = *parts.first().and_then(|p| p.first()).ok_or(SessionError::MalformedInitialization)?;
            let mut upgrading: Option<CandidateId> = None;
            match source {
                PeerSource::Direct(id) => {
                    if self.direct_state.selected != Some(id) {
                        if !self.direct_state.permits_initialization(id) {
                            return Ok(());
                        }
                        // A pending direct candidate can prove acceptance only
                        // through a complete ordinary Login initialization.
                        if command != 0 {
                            if [22, 27, 39, 41, 54].contains(&command) {
                                return Err(SessionError::Closed);
                            }
                            return Ok(());
                        }
                        if parts.len() < 11 {
                            return Err(SessionError::MalformedInitialization);
                        }
                        upgrading = Some(id);
                    }
                }
                PeerSource::Relay => {
                    if self.direct_state.selected.is_some() {
                        return Ok(());
                    }
                }
            }
            match command {
                3 | 4 | 28 => {
                    if self.phase == Phase::Active {
                        if let Ok(cursor) = CursorMessage::decode(&parts) {
                            self.emit(SessionEvent::Cursor(cursor)).await?;
                        }
                    }
                }
                42 | 43 => {
                    if self.phase != Phase::Active {
                        return Ok(());
                    }
                    let Ok(message) = ComputerAudioMessage::decode(&parts) else { return Ok(()) };
                    match message {
                        ComputerAudioMessage::Encoded(encoded) => {
                            if self.audio_decoder.is_none() {
                                self.audio_decoder = Some(G722Decoder::new()?);
                            }
                            // Preserve the predictor state while paused once initialized.
                            if let Some(decoder) = self.audio_decoder.as_mut() {
                                let pcm = decoder.decode(&encoded)?;
                                if self.audio_enabled {
                                    self.emit(SessionEvent::ComputerAudio(pcm)).await?;
                                }
                            }
                        }
                        ComputerAudioMessage::Silence { pcm_byte_count } => {
                            if self.audio_enabled {
                                self.emit(SessionEvent::ComputerAudio(vec![0; pcm_byte_count])).await?;
                            }
                        }
                    }
                }
                5 | 33 => {
                    if self.phase == Phase::Active && self.clipboard_allowed {
                        if let Ok(text) = clipboard::decode_host_to_viewer(&parts) {
                            self.emit(SessionEvent::Clipboard(text)).await?;
                        }
                    }
                }
                12 | 29 | 30 => {
                    if self.phase == Phase::Active {
                        match chat::decode_viewer_parts(&parts) {
                            Ok(Some(ChatEvent::Message(text))) => self.emit(SessionEvent::Chat(text)).await?,
                            Ok(Some(ChatEvent::Typing(typing))) => self.emit(SessionEvent::Typing(typing)).await?,
                            _ => {}
                        }
                    }
                }
                0 => self.consume_initialization(&parts, upgrading).await?,
                1 => {
                    if self.phase != Phase::Active || self.renderer.is_none() {
                        return Ok(());
                    }
                    let renderer = self.renderer.as_mut().expect("renderer");
                    let dump = &mut self.screen_dump;
                    let result = ModernScreenFrameMessage::decode(&parts).and_then(|message| {
                        if let Some(dump) = dump.as_mut() {
                            dump.record(renderer.width, renderer.height, renderer.tile_size(), &message);
                        }
                        renderer.apply(&message)
                    });
                    match result {
                        Ok(()) => {
                            self.diagnostics.decoded_frames += 1;
                            self.frames_since_screen_failure = self.frames_since_screen_failure.saturating_add(1);
                            if self.frames_since_screen_failure >= GOOD_FRAMES_FORGIVING_SCREEN_FAILURES {
                                self.screen_failures = 0;
                            }
                            let bitmap = renderer.bitmap();
                            let damage = renderer.take_damage();
                            self.frame_serial += 1;
                            let _ = self.frames.send(Some(FrameUpdate { serial: self.frame_serial, bitmap, damage }));
                            // Backpressure follows successful decoding, so a slow
                            // client cannot acknowledge a queue of stale updates.
                            self.send_to_peer(b"18", false, None).await?;
                        }
                        Err(error) => {
                            self.diagnostics.last_screen_failure = Some(error.to_string());
                            log::warn!("screen update failed: {error}");
                            self.send_to_peer(b"18", false, None).await?;
                            // A malformed drawing mission must not immediately tear
                            // down an authenticated connection. A new full
                            // initialization resets the decoder and tile cache;
                            // only repeated failures without good frames end it.
                            self.screen_failures += 1;
                            self.frames_since_screen_failure = 0;
                            if self.screen_failures > 2 {
                                return Err(error.into());
                            }
                            self.refresh_screen().await?;
                        }
                    }
                }
                20 => {
                    if let Some(started) = self.peer_ping_started.take() {
                        self.diagnostics.peer_round_trip_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                23 => {
                    if self.phase == Phase::Active {
                        self.phase = Phase::Login;
                        self.arm_deadline();
                        self.login(-1, false, None).await?;
                    }
                }
                26 => {
                    if parts.len() <= 1 || parts[1] != self.connection_hash.as_bytes() {
                        return Ok(());
                    }
                    let (PeerSource::Relay, Some(address), Some(port)) = (source, relay_address, relay_port) else { return Ok(()) };
                    self.target_address = Some(address);
                    self.target_port = Some(port);
                    self.renderer = None;
                    if self.phase == Phase::Active {
                        self.phase = Phase::Login;
                        self.arm_deadline();
                        self.login(-1, true, None).await?;
                    }
                }
                27 | 41 | 54 => return Err(SessionError::Closed),
                39 => return Err(SessionError::PartnerBlocked),
                48 => {
                    if self.phase == Phase::Active {
                        if let Some(renderer) = self.renderer.as_mut() {
                            renderer.set_tile_size(&parts)?;
                            log::info!("screen tile size {}", renderer.tile_size());
                        }
                    } else {
                        log::warn!("screen tile size {:?} ignored outside an active session", parts.get(1).and_then(|p| p.first()));
                    }
                }
                49 => {
                    let value = *parts.get(1).and_then(|p| p.first()).ok_or(SessionError::MalformedInitialization)?;
                    self.remote_view_only = value != 0;
                    if self.phase == Phase::Active {
                        self.emit(SessionEvent::InputPermission(!self.remote_view_only)).await?;
                    }
                }
                _ => {}
            }
            Ok(())
        })
    }

    async fn consume_initialization(&mut self, parts: &[Vec<u8>], upgrading: Option<CandidateId>) -> Result<(), SessionError> {
        if !matches!(self.phase, Phase::Login | Phase::Active) {
            return Ok(());
        }
        let initialization = ModernScreenInitialization::decode(parts)?;
        let geometry_changed = upgrading.is_some()
            || self.renderer.as_ref().map(|r| (r.width, r.height)) != Some((initialization.width, initialization.height));
        if geometry_changed && parts.len() > 13 && parts[13].is_empty() {
            return Err(SessionError::MalformedInitialization);
        }
        let version = initialization.target_version_bytes.as_ref().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
        log::info!("screen initialization {}x{} tile {} ({} parts, version {version:?}, geometry changed {geometry_changed})",
                   initialization.width, initialization.height, initialization.tile_size, parts.len());
        if !version_at_least(&version, [6, 0, 0]) {
            return Err(SessionError::UnsupportedVersion);
        }
        if geometry_changed {
            let last = if upgrading.is_none() {
                self.renderer.as_ref().map(|r| r.last_frame_number()).unwrap_or(ModernScreenRenderer::RESET_FRAME_NUMBER)
            } else {
                ModernScreenRenderer::RESET_FRAME_NUMBER
            };
            let dictionary = dictionary::load().map_err(|e| SessionError::Protocol(e.to_string()))?;
            let fresh = ModernScreenRenderer::new(initialization.width, initialization.height, initialization.tile_size, 2, last,
                                                  ModernScreenLimits::default(), Some(dictionary))?;
            if let Some(id) = upgrading {
                if !self.direct_state.select_initialized(id) {
                    return Ok(());
                }
                self.diagnostics.lan_direct_active = true;
                self.relay_initialization_suspended = false;
                self.pending_relay_initialization = None;
                self.direct_deadline = None;
                self.direct_window = None;
            }
            self.renderer = Some(fresh);
        } else if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_tile_size(&[vec![48], vec![initialization.tile_size as u8]])?;
        }
        self.phase = Phase::Active;
        self.deadline = None;
        let previously_accepted = self.diagnostics.peer_accepted;
        self.diagnostics.peer_accepted = true;
        if self.heartbeat_at.is_none() {
            self.heartbeat_at = Some(Instant::now() + Duration::from_secs(10));
        }
        let name = self.peer_info.as_ref().and_then(|info| {
            base64::engine::general_purpose::STANDARD.decode(&info.computer_name_field).ok().and_then(|b| String::from_utf8(b).ok())
        });
        let monitors = initialization
            .monitor_information_bytes
            .as_ref()
            .and_then(|bytes| RemoteControlCommands::monitors(bytes).ok())
            .unwrap_or_default();
        let peer = PeerSummary {
            display_name: name.filter(|n| !n.is_empty()),
            version: version.clone(),
            width: initialization.width,
            height: initialization.height,
            virtual_left: initialization.virtual_left.unwrap_or(0),
            virtual_top: initialization.virtual_top.unwrap_or(0),
            supports_binary_keyboard: version_at_least(&version, [6, 2, 13]),
            allows_remote_input: !self.remote_view_only,
            monitors,
        };
        self.emit(if previously_accepted { SessionEvent::ScreenChanged(peer) } else { SessionEvent::Accepted(peer) }).await?;
        if upgrading.is_some() {
            self.emit(SessionEvent::Status("Connected directly over the local network.".into())).await?;
        } else if previously_accepted {
            self.emit(SessionEvent::Status("Remote screen refreshed.".into())).await?;
        }
        if let Some(id) = upgrading {
            if let Some(direct) = self.direct.as_mut() {
                direct.select_candidate(id);
            }
        }
        if self.phase != Phase::Active {
            return Ok(());
        }
        // TCP ConnectionType1: source sends ASCII17 after initialization.
        self.send_to_peer(b"17", false, None).await
    }

    async fn send_to_peer(&mut self, payload: &[u8], format_flag: bool, source: Option<PeerSource>) -> Result<(), SessionError> {
        let destination = source.unwrap_or(match self.direct_state.selected {
            Some(id) => PeerSource::Direct(id),
            None => PeerSource::Relay,
        });
        match destination {
            PeerSource::Direct(id) => {
                if self.direct_state.selected != Some(id) && self.direct_state.login_candidate != Some(id) {
                    return Err(SessionError::NotConnected);
                }
                let direct = self.direct.as_mut().ok_or(SessionError::NotConnected)?;
                direct.send(payload, format_flag, id).await.map_err(SessionError::from)
            }
            PeerSource::Relay => {
                let (Some(address), Some(port)) = (self.target_address, self.target_port) else {
                    return Err(SessionError::NotConnected);
                };
                if !self.relay_ready {
                    return Err(SessionError::NotConnected);
                }
                let peer = self.peer.as_mut().ok_or(SessionError::NotConnected)?;
                peer.send(&encode_forward(address, port, format_flag, payload)).await.map_err(SessionError::from)
            }
        }
    }

    fn arm_deadline(&mut self) {
        self.deadline = Some(Instant::now() + Duration::from_secs(20));
    }

    async fn expired(&mut self) {
        self.deadline = None;
        if self.phase != Phase::Closed && self.phase != Phase::Active {
            self.finish(Some(SessionError::TimedOut)).await;
        }
    }

    async fn send_heartbeat(&mut self) {
        self.heartbeat_at = Some(Instant::now() + Duration::from_secs(10));
        if self.phase != Phase::Active {
            return;
        }
        self.heartbeat_ticks += 1;
        // Preserve an available relay while direct is selected. A relay-only
        // heartbeat failure must not tear down an authenticated direct stream.
        if self.relay_ready {
            if let Some(peer) = self.peer.as_mut() {
                self.relay_ping_started = Some(Instant::now());
                if let Err(error) = peer.send(&RelayKeepalive::relay_envelope()).await {
                    self.relay_ended(error.into()).await;
                }
            }
        }
        if self.phase != Phase::Active {
            return;
        }
        self.peer_ping_started = Some(Instant::now());
        let result = self.send_to_peer(b"20", false, None).await;
        if let Err(error) = result {
            if self.phase == Phase::Closed {
                return;
            }
            match self.direct_state.selected {
                Some(id) => self.direct_candidate_ended(id).await,
                None => self.finish(Some(error)).await,
            }
        }
    }

    /// frmMain.tmPingServer sends encrypted ASCII2 on the command channel.
    /// The interval is measured from registration, not from acceptance.
    async fn send_command_keepalive(&mut self) {
        self.command_keepalive_at = Some(Instant::now() + COMMAND_KEEPALIVE);
        if self.command.is_none() {
            self.command_keepalive_at = None;
            return;
        }
        if let Err(error) = self.command_send(&RelayKeepalive::command_envelope()).await {
            log::warn!("command keepalive failed: {error}");
        }
    }

    async fn emit(&mut self, event: SessionEvent) -> Result<(), SessionError> {
        match event {
            SessionEvent::ComputerAudio(_) | SessionEvent::Cursor(_) => {
                match self.events.try_send(event) {
                    Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
                    Err(mpsc::error::TrySendError::Closed(_)) => Err(SessionError::ConsumerTooSlow),
                }
            }
            event => {
                if self.events.send(event).await.is_err() {
                    // The owner dropped its receiver: treat as cancellation.
                    return Err(SessionError::ConsumerTooSlow);
                }
                Ok(())
            }
        }
    }

    fn finish<'a>(&'a mut self, error: Option<SessionError>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if self.phase == Phase::Closed {
                return;
            }
            self.phase = Phase::Closed;
            self.deadline = None;
            self.heartbeat_at = None;
            self.command_keepalive_at = None;
            self.direct_deadline = None;
            self.direct_window = None;
            self.command = None;
            self.peer = None;
            self.direct = None;
            self.direct_state = LanDirectUpgradeState::default();
            self.direct_pending_initialization = None;
            self.relay_ready = false;
            self.relay_failed = false;
            self.relay_initialization_suspended = false;
            self.pending_relay_initialization = None;
            self.diagnostics.lan_direct_active = false;
            self.password.clear();
            self.target_id.clear();
            self.connection_hash.clear();
            self.clipboard_allowed = false;
            self.audio_enabled = false;
            self.audio_decoder = None;
            self.renderer = None;
            self.peer_info = None;
            self.registration_reply = None;
            self.target_address = None;
            self.target_port = None;
            if let Some(error) = &error {
                log::info!("viewer session ended: {error}");
            }
            let _ = self.events.send(SessionEvent::Closed(error)).await;
        })
    }
}

fn reject(command: Command) {
    let reply = match command {
        Command::Send { reply, .. }
        | Command::SendChat { reply, .. }
        | Command::SendTyping { reply, .. }
        | Command::SendCommand { reply, .. }
        | Command::SetAudio { reply, .. }
        | Command::SetClipboardSharing { reply, .. }
        | Command::SendClipboard { reply, .. }
        | Command::RefreshScreen { reply } => reply,
        Command::Close { reply } => {
            let _ = reply.send(());
            return;
        }
    };
    let _ = reply.send(Err(SessionError::NotConnected));
}

/// Minutes east of UTC for the current local time, as .NET reports it.
pub fn local_utc_offset_minutes() -> i32 {
    // SAFETY: localtime_r only writes into the provided struct.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return 0;
        }
        (tm.tm_gmtoff / 60) as i32
    }
}
