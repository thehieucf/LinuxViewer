//! A separate native host registration and relay. Capture and system input
//! are owned by the app, after password verification and explicit approval.

pub mod messages;

use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::chat::{self, ChatEvent};
use crate::clipboard;
use crate::commands::{RemoteControlCommands, SideSwitchRequest};
use crate::crypto::LegacyCommandCipher;
use crate::login::HostLoginRequest;
use crate::registration::{ClientRegistrationReply, ClientRegistrationRequest};
use crate::relay::{self, encode_forward, encode_system, HttpMethod, RelayEnvelope, RelayIPv4Address, RelayKeepalive};
use crate::transport::{self, Connection, OpenOptions, ReaderEvent, Security, TransportError};
use crate::{CLIENT_VERSION, COMMAND_HOST};
pub use messages::{HostedScreenFrame, RelayAddress, RelayRequest};

#[derive(Debug, Clone)]
pub struct HostConfiguration {
    pub registration: ClientRegistrationRequest,
    pub command_host: String,
    pub listener_port: u16,
}

impl HostConfiguration {
    pub fn new(registration: ClientRegistrationRequest) -> Self {
        Self { registration, command_host: COMMAND_HOST.into(), listener_port: 443 }
    }
}

pub type ViewerId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingRequest {
    pub id: ViewerId,
    /// The peer's claim, not an independently verified account identity.
    pub viewer_id: String,
    pub display_name: Option<String>,
}

/// A local confirmation token. Credentials stay inside the session until this
/// exact request has been approved and its acknowledgment was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideSwitchApproval {
    pub id: u64,
    pub session: ViewerId,
    pub claimed_id: String,
}

#[derive(Debug, Clone)]
pub enum HostEvent {
    Status(String),
    Registered(String),
    Request(IncomingRequest),
    Connected(IncomingRequest),
    CaptureRequested(u64),
    Input { session: ViewerId, payload: Vec<u8> },
    ReleaseInput,
    ViewerEnded(Option<String>),
    Chat { session: ViewerId, text: String },
    Typing { session: ViewerId, active: bool },
    Clipboard { session: ViewerId, text: String },
    SideSwitchRequested { session: ViewerId, request: SideSwitchApproval },
    /// Hosting stopped; `None` is an orderly stop.
    Stopped(Option<HostError>),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    #[error("Incoming access is already running.")]
    AlreadyRunning,
    #[error("The incoming connection is no longer available.")]
    NotConnected,
    #[error("The UltraViewer incoming relay is unavailable. Try again.")]
    RelayUnavailable,
    #[error("The incoming UltraViewer connection timed out.")]
    TimedOut,
    #[error("The remote viewer requested an unsupported screen color mode. Use 16-bit color.")]
    UnsupportedDisplay,
    #[error("{0}")]
    Transport(TransportError),
    #[error("The incoming connection sent an invalid message: {0}")]
    Protocol(String),
    #[error("This computer's host profile is invalid: {0}")]
    Registration(String),
}

impl From<TransportError> for HostError {
    fn from(error: TransportError) -> Self {
        HostError::Transport(error)
    }
}

macro_rules! host_protocol_error {
    ($($t:ty),*) => {$(
        impl From<$t> for HostError {
            fn from(error: $t) -> Self { HostError::Protocol(error.to_string()) }
        }
    )*};
}
host_protocol_error!(
    crate::relay::EnvelopeError,
    messages::HostRelayError,
    messages::HostedScreenError,
    crate::chat::ChatError,
    crate::clipboard::ClipboardError,
    crate::multipart::MultipartError
);

impl From<crate::registration::RegistrationError> for HostError {
    fn from(error: crate::registration::RegistrationError) -> Self {
        HostError::Registration(error.to_string())
    }
}

type Reply<T = ()> = oneshot::Sender<Result<T, HostError>>;

enum Command {
    Approve { id: ViewerId, first_frame: HostedScreenFrame, allows_control: bool, reply: Reply },
    Decline { id: ViewerId },
    SetControl { allowed: bool, session: ViewerId, reply: Reply },
    SubmitFrame { frame: HostedScreenFrame, ticket: u64 },
    EndViewer { session: Option<ViewerId>, reason: Option<String> },
    SendChat { text: String, session: ViewerId, reply: Reply },
    SendTyping { typing: bool, session: ViewerId },
    SetClipboardSharing { enabled: bool, session: ViewerId, reply: Reply },
    SendClipboard { text: String, session: ViewerId, reply: Reply },
    ApproveSideSwitch { approval: u64, session: ViewerId, reply: Reply<SideSwitchRequest> },
    DeclineSideSwitch { approval: u64, session: ViewerId },
    IsControlAllowed { session: ViewerId, reply: oneshot::Sender<bool> },
    SendPayload { session: ViewerId, payload: Vec<u8>, reply: Reply },
    Registration { reply: oneshot::Sender<Option<ClientRegistrationReply>> },
    Lookup { plaintext: Vec<u8>, correlation: i32, reply: Reply<Vec<u8>> },
    Stop { reply: oneshot::Sender<()> },
}

#[derive(Clone)]
pub struct HostHandle {
    commands: mpsc::UnboundedSender<Command>,
}

impl HostHandle {
    async fn request<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, HostError> {
        let (reply, response) = oneshot::channel();
        self.commands.send(make(reply)).map_err(|_| HostError::NotConnected)?;
        response.await.unwrap_or(Err(HostError::NotConnected))
    }

    pub async fn approve(&self, id: ViewerId, first_frame: HostedScreenFrame, allows_control: bool) -> Result<(), HostError> {
        self.request(|reply| Command::Approve { id, first_frame, allows_control, reply }).await
    }

    pub fn decline(&self, id: ViewerId) {
        let _ = self.commands.send(Command::Decline { id });
    }

    pub async fn set_control_allowed(&self, allowed: bool, session: ViewerId) -> Result<(), HostError> {
        self.request(|reply| Command::SetControl { allowed, session, reply }).await
    }

    pub fn submit_frame(&self, frame: HostedScreenFrame, ticket: u64) {
        let _ = self.commands.send(Command::SubmitFrame { frame, ticket });
    }

    pub fn end_viewer(&self, session: Option<ViewerId>, reason: Option<String>) {
        let _ = self.commands.send(Command::EndViewer { session, reason });
    }

    pub async fn send_chat(&self, text: String, session: ViewerId) -> Result<(), HostError> {
        self.request(|reply| Command::SendChat { text, session, reply }).await
    }

    pub fn send_typing(&self, typing: bool, session: ViewerId) {
        let _ = self.commands.send(Command::SendTyping { typing, session });
    }

    pub async fn set_clipboard_sharing(&self, enabled: bool, session: ViewerId) -> Result<(), HostError> {
        self.request(|reply| Command::SetClipboardSharing { enabled, session, reply }).await
    }

    pub async fn send_clipboard(&self, text: String, session: ViewerId) -> Result<(), HostError> {
        self.request(|reply| Command::SendClipboard { text, session, reply }).await
    }

    /// Only the current, locally approved viewer can switch roles. A successful
    /// return authorizes the caller to start its explicitly approved connection.
    pub async fn approve_side_switch(&self, approval: u64, session: ViewerId) -> Result<SideSwitchRequest, HostError> {
        self.request(|reply| Command::ApproveSideSwitch { approval, session, reply }).await
    }

    /// Sends an already encoded multipart application payload (cursor,
    /// computer audio…) to the streaming viewer.
    pub async fn send_payload(&self, payload: Vec<u8>, session: ViewerId) -> Result<(), HostError> {
        self.request(|reply| Command::SendPayload { session, payload, reply }).await
    }

    pub fn decline_side_switch(&self, approval: u64, session: ViewerId) {
        let _ = self.commands.send(Command::DeclineSideSwitch { approval, session });
    }

    pub async fn is_control_allowed(&self, session: ViewerId) -> bool {
        let (reply, response) = oneshot::channel();
        if self.commands.send(Command::IsControlAllowed { session, reply }).is_err() {
            return false;
        }
        response.await.unwrap_or(false)
    }

    /// Ends any connected viewer first so it is told the session is over.
    pub async fn stop(&self) {
        let (reply, done) = oneshot::channel();
        if self.commands.send(Command::Stop { reply }).is_ok() {
            let _ = tokio::time::timeout(Duration::from_secs(5), done).await;
        }
    }
}

/// Lets an outgoing session use this host's registration for partner lookup
/// (command 8), so one computer keeps a single UltraViewer registration.
#[derive(Clone)]
pub struct CommandPort {
    commands: mpsc::UnboundedSender<Command>,
}

impl std::fmt::Debug for CommandPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CommandPort")
    }
}

impl CommandPort {
    /// The current registration reply; waits briefly while registering.
    pub async fn registration(&self) -> Option<ClientRegistrationReply> {
        for _ in 0..40 {
            let (reply, response) = oneshot::channel();
            self.commands.send(Command::Registration { reply }).ok()?;
            if let Some(registration) = response.await.ok()? {
                return Some(registration);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        None
    }

    /// Sends a command-8 lookup and returns its plaintext reply.
    pub async fn lookup(&self, plaintext: Vec<u8>, correlation: i32) -> Result<Vec<u8>, HostError> {
        let (reply, response) = oneshot::channel();
        self.commands.send(Command::Lookup { plaintext, correlation, reply }).map_err(|_| HostError::NotConnected)?;
        response.await.unwrap_or(Err(HostError::NotConnected))
    }
}

impl HostHandle {
    pub fn command_port(&self) -> CommandPort {
        CommandPort { commands: self.commands.clone() }
    }
}

pub struct HostStreams {
    pub handle: HostHandle,
    pub events: mpsc::Receiver<HostEvent>,
}

pub fn start(configuration: HostConfiguration) -> Result<HostStreams, HostError> {
    configuration.registration.encode()?;
    if configuration.registration.uses_proxy {
        return Err(HostError::RelayUnavailable);
    }
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::channel(128);
    let (inbox_tx, inbox_rx) = mpsc::channel(64);
    let actor = HostActor {
        running: true,
        configuration,
        events: event_tx,
        inbox: inbox_tx,
        command: None,
        relay: None,
        relay_id: 0,
        relay_request: None,
        deferred_request: None,
        relay_address: None,
        default_relay_addresses: String::new(),
        default_relay_hostnames: String::new(),
        registered: false,
        registration_reply: None,
        lookups: std::collections::HashMap::new(),
        registration_deadline: None,
        relay_deadline: None,
        viewer_deadline: None,
        keepalive_at: None,
        capture_at: None,
        peer: None,
        peer_phase: PeerPhase::None,
        control_allowed: false,
        clipboard_allowed: false,
        side_switch: SideSwitchGate::default(),
        pending_frame: None,
        last_frame: None,
        next_sequence: messages::FIRST_FRAME_NUMBER,
        waiting_for_ack: false,
        capture_ticket: None,
        next_ticket: 1,
        next_viewer: 1,
    };
    tokio::spawn(actor.run(command_rx, inbox_rx));
    Ok(HostStreams { handle: HostHandle { commands: command_tx }, events: event_rx })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Route {
    address: RelayIPv4Address,
    port: u16,
}

#[derive(Debug, Clone)]
struct Peer {
    request: IncomingRequest,
    login: HostLoginRequest,
    route: Route,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerPhase {
    None,
    Approval,
    Starting,
    Streaming,
}

enum Inbox {
    CommandOpened(Result<Connection, TransportError>),
    CommandReader(ReaderEvent),
    RelayOpened(u64, Result<Connection, TransportError>),
    RelayReader(u64, ReaderEvent),
}

/// Confirmation state separated from transport so replay, decline and stale
/// session behavior can be verified without opening a network connection.
#[derive(Debug, Default)]
pub struct SideSwitchGate {
    pending: Option<(SideSwitchApproval, SideSwitchRequest)>,
    approving: Option<u64>,
    next: u64,
}

impl SideSwitchGate {
    pub fn receive(&mut self, request: SideSwitchRequest, session: ViewerId, approved_viewer_id: &str, is_sharing: bool)
        -> Option<SideSwitchApproval> {
        if !is_sharing || request.claimed_id != approved_viewer_id || self.pending.is_some() || self.approving.is_some() {
            return None;
        }
        self.next += 1;
        let approval = SideSwitchApproval { id: self.next, session, claimed_id: request.claimed_id.clone() };
        self.pending = Some((approval.clone(), request));
        Some(approval)
    }

    pub fn begin_approval(&mut self, id: u64, session: ViewerId) -> Option<SideSwitchRequest> {
        if self.approving.is_some() {
            return None;
        }
        let (approval, _) = self.pending.as_ref()?;
        if approval.id != id || approval.session != session {
            return None;
        }
        let (_, request) = self.pending.take()?;
        self.approving = Some(id);
        Some(request)
    }

    pub fn decline(&mut self, id: u64, session: ViewerId) {
        if self.pending.as_ref().is_some_and(|(a, _)| a.id == id && a.session == session) {
            self.pending = None;
        }
    }

    pub fn complete_approval(&mut self, id: u64) {
        if self.approving == Some(id) {
            self.approving = None;
        }
    }

    pub fn reset(&mut self) {
        self.pending = None;
        self.approving = None;
    }
}

struct HostActor {
    running: bool,
    configuration: HostConfiguration,
    events: mpsc::Sender<HostEvent>,
    inbox: mpsc::Sender<Inbox>,
    command: Option<Connection>,
    relay: Option<Connection>,
    relay_id: u64,
    relay_request: Option<RelayRequest>,
    /// A newer request that arrived while a viewer was still attached; it is
    /// opened as soon as that viewer ends (e.g. a viewer that crashed).
    deferred_request: Option<RelayRequest>,
    relay_address: Option<RelayAddress>,
    default_relay_addresses: String,
    default_relay_hostnames: String,
    registered: bool,
    registration_reply: Option<ClientRegistrationReply>,
    lookups: std::collections::HashMap<i32, Reply<Vec<u8>>>,
    registration_deadline: Option<Instant>,
    relay_deadline: Option<Instant>,
    viewer_deadline: Option<Instant>,
    keepalive_at: Option<Instant>,
    capture_at: Option<Instant>,
    peer: Option<Peer>,
    peer_phase: PeerPhase,
    control_allowed: bool,
    clipboard_allowed: bool,
    side_switch: SideSwitchGate,
    pending_frame: Option<HostedScreenFrame>,
    last_frame: Option<HostedScreenFrame>,
    next_sequence: i32,
    waiting_for_ack: bool,
    capture_ticket: Option<u64>,
    next_ticket: u64,
    next_viewer: ViewerId,
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn system_tag(payload: &[u8], separator: u8) -> String {
    let tag: Vec<u8> = payload.iter().copied().take_while(|&b| b != separator).collect();
    if tag.is_empty() || tag.len() > 3 || !tag.iter().all(u8::is_ascii_digit) {
        return "other".into();
    }
    String::from_utf8(tag).unwrap_or_default()
}

fn supported_version(version: &str) -> bool {
    let fields: Vec<&str> = version.split('.').collect();
    fields.len() == 3 && fields.iter().all(|f| crate::util::is_digits(f)) && fields[0].parse::<u32>().is_ok_and(|v| v >= 6)
}

impl HostActor {
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>, mut inbox: mpsc::Receiver<Inbox>) {
        self.open_command();
        while self.running {
            tokio::select! {
                biased;
                command = commands.recv() => match command {
                    Some(command) => self.handle_command(command).await,
                    None => self.stop().await,
                },
                Some(message) = inbox.recv() => self.handle_inbox(message).await,
                _ = sleep_until_opt(self.registration_deadline) => {
                    self.registration_deadline = None;
                    if !self.registered {
                        self.finish(Some(HostError::TimedOut)).await;
                    }
                }
                _ = sleep_until_opt(self.relay_deadline) => {
                    self.relay_deadline = None;
                    if self.relay_address.is_none() {
                        self.relay_ended(Some("The incoming relay did not provide an address. Ask the other computer to connect again.".into())).await;
                    }
                }
                _ = sleep_until_opt(self.viewer_deadline) => {
                    self.viewer_deadline = None;
                    if self.peer.is_some() {
                        self.end_viewer(Some("The incoming connection timed out. Ask the other computer to connect again.".into())).await;
                    }
                }
                _ = sleep_until_opt(self.capture_at) => {
                    self.capture_at = None;
                    self.request_capture().await;
                }
                _ = sleep_until_opt(self.keepalive_at) => self.keepalive().await,
            }
        }
        commands.close();
        while let Ok(command) = commands.try_recv() {
            reject(command);
        }
    }

    fn open_command(&mut self) {
        let inbox = self.inbox.clone();
        let events = self.events.clone();
        let mut options = OpenOptions::new(self.configuration.command_host.clone(), self.configuration.listener_port,
                                           Security::ultraviewer(COMMAND_HOST));
        options.initial_request = relay::http_preamble(COMMAND_HOST, HttpMethod::Post).ok();
        options.expects_http_response = true;
        tokio::spawn(async move {
            let _ = events.send(HostEvent::Status("Getting this computer's UltraViewer ID…".into())).await;
            let result = transport::open_command_connection(options, Duration::from_secs(3), inbox.clone(), Inbox::CommandReader).await;
            let _ = inbox.send(Inbox::CommandOpened(result)).await;
        });
    }

    async fn emit(&mut self, event: HostEvent) {
        if self.events.send(event).await.is_err() {
            // The owner dropped its receiver.
            self.running = false;
        }
    }

    // ------------------------------------------------------------- commands

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Approve { id, first_frame, allows_control, reply } => {
                let result = match &self.peer {
                    Some(peer) if self.peer_phase == PeerPhase::Approval && peer.request.id == id => {
                        let peer = peer.clone();
                        self.initialize(peer, first_frame, allows_control).await
                    }
                    _ => Err(HostError::NotConnected),
                };
                let _ = reply.send(result);
            }
            Command::Decline { id } => {
                if self.peer.as_ref().is_some_and(|p| p.request.id == id) {
                    self.end_viewer(Some("The incoming request was declined.".into())).await;
                }
            }
            Command::SetControl { allowed, session, reply } => {
                let peer = self.peer.as_ref().map(|p| (p.request.id, p.route));
                let result = match peer {
                    Some((id, route)) if matches!(self.peer_phase, PeerPhase::Starting | PeerPhase::Streaming) => {
                        if id != session {
                            Ok(())
                        } else {
                            self.control_allowed = allowed;
                            if !allowed {
                                self.emit(HostEvent::ReleaseInput).await;
                            }
                            self.send(&messages::control_permission(allowed), route).await
                        }
                    }
                    _ => Err(HostError::NotConnected),
                };
                let _ = reply.send(result);
            }
            Command::SubmitFrame { frame, ticket } => {
                if self.peer_phase != PeerPhase::Streaming || self.capture_ticket != Some(ticket) {
                    return;
                }
                self.capture_ticket = None;
                if self.last_frame.as_ref().is_some_and(|previous| *previous == frame) {
                    self.schedule_capture();
                    return;
                }
                if let Err(error) = self.send_frame(frame).await {
                    log::warn!("frame send failed: {error}");
                    self.end_viewer(Some(error.to_string())).await;
                }
            }
            Command::EndViewer { session, reason } => {
                let matches = match session {
                    Some(session) => self.peer.as_ref().is_some_and(|p| p.request.id == session),
                    None => self.peer.is_some(),
                };
                if matches {
                    self.end_viewer(reason.or_else(|| Some("You ended the incoming session.".into()))).await;
                }
            }
            Command::SendChat { text, session, reply } => {
                let result = match self.streaming_route(session) {
                    Some(route) => match chat::host_to_viewer(&text) {
                        Ok(payload) => self.send(&payload, route).await,
                        Err(error) => Err(error.into()),
                    },
                    None => Err(HostError::NotConnected),
                };
                let _ = reply.send(result);
            }
            Command::SendTyping { typing, session } => {
                if let Some(route) = self.streaming_route(session) {
                    let _ = self.send(&chat::host_to_viewer_typing(typing), route).await;
                }
            }
            Command::SetClipboardSharing { enabled, session, reply } => {
                let result = if self.streaming_route(session).is_some() {
                    self.clipboard_allowed = enabled;
                    Ok(())
                } else {
                    Err(HostError::NotConnected)
                };
                let _ = reply.send(result);
            }
            Command::SendClipboard { text, session, reply } => {
                let result = match self.streaming_route(session) {
                    Some(route) if self.clipboard_allowed => match clipboard::host_to_viewer(&text) {
                        Ok(payload) => self.send(&payload, route).await,
                        Err(error) => Err(error.into()),
                    },
                    _ => Err(HostError::NotConnected),
                };
                let _ = reply.send(result);
            }
            Command::ApproveSideSwitch { approval, session, reply } => {
                let result = async {
                    let route = self.streaming_route(session).ok_or(HostError::NotConnected)?;
                    let request = self.side_switch.begin_approval(approval, session).ok_or(HostError::NotConnected)?;
                    let sent = self.send(&RemoteControlCommands::side_switch_acknowledgment(), route).await;
                    self.side_switch.complete_approval(approval);
                    sent?;
                    // Windows may close its viewer immediately after receiving 54.
                    if !self.running || self.peer.as_ref().is_some_and(|p| p.request.id != session || p.route != route) {
                        return Err(HostError::NotConnected);
                    }
                    Ok(request)
                }
                .await;
                let _ = reply.send(result);
            }
            Command::DeclineSideSwitch { approval, session } => self.side_switch.decline(approval, session),
            Command::IsControlAllowed { session, reply } => {
                let allowed = self.running
                    && self.peer_phase == PeerPhase::Streaming
                    && self.peer.as_ref().is_some_and(|p| p.request.id == session)
                    && self.control_allowed;
                let _ = reply.send(allowed);
            }
            Command::SendPayload { session, payload, reply } => {
                let result = match self.streaming_route(session) {
                    Some(route) => self.send(&payload, route).await,
                    None => Err(HostError::NotConnected),
                };
                let _ = reply.send(result);
            }
            Command::Registration { reply } => {
                let _ = reply.send(self.registration_reply.clone());
            }
            Command::Lookup { plaintext, correlation, reply } => {
                if !self.registered {
                    let _ = reply.send(Err(HostError::NotConnected));
                    return;
                }
                match self.send_command(&plaintext).await {
                    Ok(()) => {
                        self.lookups.insert(correlation, reply);
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Command::Stop { reply } => {
                self.stop().await;
                let _ = reply.send(());
            }
        }
    }

    fn streaming_route(&self, session: ViewerId) -> Option<Route> {
        match &self.peer {
            Some(peer) if self.peer_phase == PeerPhase::Streaming && peer.request.id == session => Some(peer.route),
            _ => None,
        }
    }

    async fn stop(&mut self) {
        if self.running && self.peer.is_some() {
            self.end_viewer(Some("You ended the incoming session.".into())).await;
        }
        self.finish(None).await;
    }

    // ------------------------------------------------------------- inbox

    async fn handle_inbox(&mut self, message: Inbox) {
        match message {
            Inbox::CommandOpened(result) => match result {
                Ok(connection) => {
                    self.command = Some(connection);
                    self.registration_deadline = Some(Instant::now() + Duration::from_secs(20));
                    let envelope = match self.configuration.registration.relay_envelope(&LegacyCommandCipher::desktop_server_commands()) {
                        Ok(envelope) => envelope,
                        Err(error) => return self.finish(Some(error.into())).await,
                    };
                    if let Err(error) = self.command_send_raw(&envelope).await {
                        self.finish(Some(error)).await;
                    }
                }
                Err(error) => self.finish(Some(error.into())).await,
            },
            Inbox::CommandReader(event) => match event {
                ReaderEvent::Frame(bytes) => {
                    let result = match RelayEnvelope::decode(&bytes) {
                        Ok(RelayEnvelope::System { payload }) => self.consume_command(&payload).await,
                        Ok(_) => Ok(()),
                        Err(error) => Err(error.into()),
                    };
                    if let Err(error) = result {
                        self.finish(Some(error)).await;
                    }
                }
                ReaderEvent::Closed(error) => {
                    self.command = None;
                    self.finish(Some(error.map(HostError::from).unwrap_or(HostError::RelayUnavailable))).await;
                }
            },
            Inbox::RelayOpened(id, result) => {
                if id != self.relay_id || !self.running {
                    return;
                }
                match result {
                    Ok(connection) => {
                        self.relay = Some(connection);
                        self.relay_deadline = Some(Instant::now() + Duration::from_secs(15));
                        let request = messages::address_request_envelope();
                        if let Err(error) = self.relay_send_raw(&request).await {
                            self.relay_ended(Some(error.to_string())).await;
                        }
                    }
                    Err(error) => self.relay_ended(Some(error.to_string())).await,
                }
            }
            Inbox::RelayReader(id, event) => {
                if id != self.relay_id || self.relay.is_none() {
                    return;
                }
                match event {
                    ReaderEvent::Frame(bytes) => {
                        if let Err(error) = self.consume_relay(&bytes).await {
                            self.relay_ended(Some(error.to_string())).await;
                        }
                    }
                    ReaderEvent::Closed(error) => self.relay_ended(error.map(|e| e.to_string())).await,
                }
            }
        }
    }

    async fn consume_command(&mut self, payload: &[u8]) -> Result<(), HostError> {
        let tag = system_tag(payload, b':');
        if tag == "3" && !self.registered {
            let reply = ClientRegistrationReply::parse(payload)?;
            self.default_relay_addresses = reply.ping_list().to_string();
            self.default_relay_hostnames = reply.ping_host_name_list().to_string();
            self.registration_reply = Some(reply.clone());
            self.registered = true;
            self.registration_deadline = None;
            self.emit(HostEvent::Registered(reply.assigned_id_text())).await;
            self.keepalive_at = Some(Instant::now() + Duration::from_secs(30));
        } else if tag == "8" {
            // A lookup made on behalf of an outgoing session.
            let correlation = std::str::from_utf8(payload).ok().and_then(|t| t.rsplit(':').next()).and_then(|c| c.parse::<i32>().ok());
            if let Some(reply) = correlation.and_then(|c| self.lookups.remove(&c)) {
                let _ = reply.send(Ok(payload.to_vec()));
            }
        } else if (tag == "11" || tag == "17") && self.registered {
            self.emit(HostEvent::Status("Preparing the incoming connection…".into())).await;
            log::debug!("relay request {:?} defaults {:?} / {:?}", String::from_utf8_lossy(payload), self.default_relay_addresses, self.default_relay_hostnames);
            let request = messages::request(payload, &self.default_relay_addresses, &self.default_relay_hostnames)?;
            log::debug!("relay request parsed: {request:?}");
            if self.relay_request.as_ref() == Some(&request) && self.relay_address.is_some() {
                self.advertise_relay().await?;
            } else if self.peer.is_none() {
                self.replace_relay(request);
            } else {
                self.deferred_request = Some(request);
            }
        }
        Ok(())
    }

    fn replace_relay(&mut self, request: RelayRequest) {
        self.relay = None;
        self.relay_address = None;
        self.relay_request = Some(request.clone());
        self.relay_id += 1;
        self.open_relay(request);
    }

    fn open_relay(&mut self, request: RelayRequest) {
        let id = self.relay_id;
        let mut options = OpenOptions::new(request.address.to_string(), self.configuration.listener_port, Security::ultraviewer(&request.hostname));
        options.initial_request = relay::http_preamble(&request.hostname, HttpMethod::Post).ok();
        options.expects_http_response = true;
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            let result = transport::open(options, inbox.clone(), move |event| Inbox::RelayReader(id, event)).await;
            let _ = inbox.send(Inbox::RelayOpened(id, result)).await;
        });
    }

    async fn consume_relay(&mut self, bytes: &[u8]) -> Result<(), HostError> {
        match RelayEnvelope::decode(bytes)? {
            RelayEnvelope::System { payload } => {
                if payload.starts_with(b"0:") {
                    self.relay_address = Some(messages::address(&payload)?);
                    self.relay_deadline = None;
                    self.advertise_relay().await?;
                }
                Ok(())
            }
            RelayEnvelope::Forward { address, port, format_flag, payload } => {
                let Ok(decoded) = messages::peer_payload(&payload, format_flag) else { return Ok(()) };
                self.consume_peer(&decoded, Route { address, port }).await
            }
        }
    }

    async fn advertise_relay(&mut self) -> Result<(), HostError> {
        let (Some(request), Some(address)) = (self.relay_request.clone(), self.relay_address) else { return Ok(()) };
        self.send_command(&messages::availability(&request, &address)).await?;
        self.emit(HostEvent::Status("Waiting for the other computer's login…".into())).await;
        Ok(())
    }

    async fn consume_peer(&mut self, payload: &[u8], route: Route) -> Result<(), HostError> {
        if payload.starts_with(b"login:") {
            let verified = HostLoginRequest::decode(payload)
                .ok()
                .filter(|login| login.verify(&self.configuration.registration.password).unwrap_or(false));
            let Some(login) = verified else {
                return self.send(&messages::single_command(22), route).await;
            };
            if login.display_colors != 2
                || !supported_version(login.client_version())
                || login.connection_hash_key().is_empty()
                || login.connection_hash_key().len() > 128
            {
                self.send(&messages::single_command(27), route).await?;
                self.emit(HostEvent::Status("The remote viewer needs modern 16-bit screen mode.".into())).await;
                return Ok(());
            }
            if let Some(old) = self.peer.clone() {
                if old.route == route && old.login.connection_hash_key() == login.connection_hash_key()
                    && self.peer_phase == PeerPhase::Streaming
                {
                    if let Some(frame) = self.last_frame.clone() {
                        let mut renewed = old;
                        renewed.login = login;
                        let control = self.control_allowed;
                        return self.initialize(renewed, frame, control).await;
                    }
                } else if old.route != route {
                    return self.send(&messages::single_command(27), route).await;
                }
                return Ok(());
            }
            let name: String = login.display_name().chars().take(50).collect();
            let request = IncomingRequest {
                id: self.next_viewer,
                viewer_id: login.claimed_id().to_string(),
                display_name: (!name.is_empty()).then_some(name),
            };
            self.next_viewer += 1;
            self.peer = Some(Peer { request: request.clone(), login, route });
            self.peer_phase = PeerPhase::Approval;
            self.emit(HostEvent::Request(request)).await;
            self.viewer_deadline = Some(Instant::now() + Duration::from_secs(45));
            return Ok(());
        }
        let Some(peer) = self.peer.clone() else { return Ok(()) };
        if peer.route != route {
            return Ok(());
        }
        let session = peer.request.id;
        if payload.starts_with(b"54|||") {
            if let Ok(Some(request)) = RemoteControlCommands::decode_side_switch_request(payload) {
                let sharing = self.peer_phase == PeerPhase::Streaming;
                if let Some(approval) = self.side_switch.receive(request, session, &peer.request.viewer_id, sharing) {
                    self.emit(HostEvent::SideSwitchRequested { session, request: approval }).await;
                }
            }
            return Ok(());
        }
        if self.peer_phase == PeerPhase::Streaming && (payload.starts_with(b"33|||") || payload.starts_with(b"5|||")) {
            if self.clipboard_allowed {
                if let Ok(text) = clipboard::decode_normalized_viewer_to_host(payload) {
                    self.emit(HostEvent::Clipboard { session, text }).await;
                }
            }
            return Ok(());
        }
        if self.peer_phase == PeerPhase::Streaming {
            if let Ok(Some(event)) = chat::decode_host_payload(payload) {
                match event {
                    ChatEvent::Message(text) => self.emit(HostEvent::Chat { session, text }).await,
                    ChatEvent::Typing(active) => self.emit(HostEvent::Typing { session, active }).await,
                }
                return Ok(());
            }
        }
        if payload == b"17" && self.peer_phase == PeerPhase::Starting {
            self.viewer_deadline = None;
            self.peer_phase = PeerPhase::Streaming;
            self.emit(HostEvent::Connected(peer.request.clone())).await;
            if let Some(frame) = self.pending_frame.take() {
                self.send_frame(frame).await?;
            }
            return Ok(());
        }
        let tag = system_tag(payload, b'|');
        if tag == "19" {
            self.end_viewer(Some("The other computer disconnected.".into())).await;
        } else if tag == "20" {
            self.send(&messages::keepalive_reply(), route).await?;
        } else if tag == "18" && self.peer_phase == PeerPhase::Streaming && self.waiting_for_ack {
            self.waiting_for_ack = false;
            self.viewer_deadline = None;
            self.schedule_capture();
        } else if tag == "32" && payload == b"32|||1" {
            self.end_viewer(Some(HostError::UnsupportedDisplay.to_string())).await;
        } else if self.peer_phase == PeerPhase::Streaming
            && self.control_allowed
            && (payload.starts_with(b"8|||") || payload.starts_with(&[30, 124, 124, 124]))
        {
            if payload.len() <= 4096 {
                self.emit(HostEvent::Input { session, payload: payload.to_vec() }).await;
            }
        }
        Ok(())
    }

    async fn initialize(&mut self, approved: Peer, frame: HostedScreenFrame, control: bool) -> Result<(), HostError> {
        let initialization = messages::initialization(frame.width, frame.height, CLIENT_VERSION)?;
        let route = approved.route;
        self.peer = Some(approved);
        self.peer_phase = PeerPhase::Starting;
        self.control_allowed = control;
        self.side_switch.reset();
        self.pending_frame = Some(frame);
        self.last_frame = None;
        self.next_sequence = messages::FIRST_FRAME_NUMBER;
        self.waiting_for_ack = false;
        self.capture_ticket = None;
        self.capture_at = None;
        self.emit(HostEvent::ReleaseInput).await;
        self.viewer_deadline = Some(Instant::now() + Duration::from_secs(20));
        self.send(&initialization, route).await?;
        self.send(&messages::control_permission(self.control_allowed), route).await
    }

    async fn send_frame(&mut self, frame: HostedScreenFrame) -> Result<(), HostError> {
        let Some(peer) = self.peer.clone() else { return Ok(()) };
        if self.peer_phase != PeerPhase::Streaming || self.waiting_for_ack {
            return Ok(());
        }
        if let Some(old) = &self.last_frame {
            if old.width != frame.width || old.height != frame.height {
                let control = self.control_allowed;
                return self.initialize(peer, frame, control).await;
            }
        }
        if self.next_sequence == i32::MAX {
            return Err(HostError::UnsupportedDisplay);
        }
        let sequence = self.next_sequence;
        let data = messages::full_frame(&frame, sequence)?;
        self.next_sequence += 1;
        self.waiting_for_ack = true;
        self.last_frame = Some(frame);
        self.viewer_deadline = Some(Instant::now() + Duration::from_secs(20));
        self.send(&data, peer.route).await
    }

    fn schedule_capture(&mut self) {
        if self.peer_phase != PeerPhase::Streaming || self.waiting_for_ack || self.capture_ticket.is_some() || self.peer.is_none() {
            return;
        }
        self.capture_at = Some(Instant::now() + Duration::from_millis(250));
    }

    async fn request_capture(&mut self) {
        if !self.running || self.peer.is_none() || self.peer_phase != PeerPhase::Streaming || self.waiting_for_ack || self.capture_ticket.is_some() {
            return;
        }
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.capture_ticket = Some(ticket);
        self.emit(HostEvent::CaptureRequested(ticket)).await;
    }

    async fn end_viewer(&mut self, reason: Option<String>) {
        let old = self.peer.take();
        self.peer_phase = PeerPhase::None;
        self.control_allowed = false;
        self.side_switch.reset();
        self.clipboard_allowed = false;
        self.waiting_for_ack = false;
        self.capture_ticket = None;
        self.pending_frame = None;
        self.last_frame = None;
        self.viewer_deadline = None;
        self.capture_at = None;
        self.emit(HostEvent::ReleaseInput).await;
        if let Some(old) = old {
            self.emit(HostEvent::ViewerEnded(reason)).await;
            let _ = self.send(&messages::single_command(27), old.route).await;
        }
        if let Some(request) = self.deferred_request.take() {
            if self.running && self.registered {
                self.replace_relay(request);
            }
        }
    }

    async fn relay_ended(&mut self, reason: Option<String>) {
        log::warn!("incoming relay ended: {reason:?}");
        self.relay_id += 1;
        self.relay_deadline = None;
        self.relay = None;
        self.relay_address = None;
        self.relay_request = None;
        self.end_viewer(Some(reason.unwrap_or_else(|| "The incoming relay disconnected.".into()))).await;
    }

    async fn keepalive(&mut self) {
        self.keepalive_at = Some(Instant::now() + Duration::from_secs(30));
        if let Err(error) = self.command_send_raw(&RelayKeepalive::command_envelope()).await {
            return self.finish(Some(error)).await;
        }
        if self.relay.is_some() {
            if let Err(error) = self.relay_send_raw(&RelayKeepalive::relay_envelope()).await {
                self.relay_ended(Some(error.to_string())).await;
            }
        }
    }

    async fn send(&mut self, payload: &[u8], route: Route) -> Result<(), HostError> {
        self.relay_send_raw(&encode_forward(route.address, route.port, false, payload)).await
    }

    async fn relay_send_raw(&mut self, envelope: &[u8]) -> Result<(), HostError> {
        let relay = self.relay.as_mut().ok_or(HostError::NotConnected)?;
        relay.send(envelope).await.map_err(HostError::from)
    }

    async fn command_send_raw(&mut self, envelope: &[u8]) -> Result<(), HostError> {
        let command = self.command.as_mut().ok_or(HostError::NotConnected)?;
        command.send(envelope).await.map_err(HostError::from)
    }

    async fn send_command(&mut self, payload: &[u8]) -> Result<(), HostError> {
        let encrypted = LegacyCommandCipher::desktop_server_commands().encrypt(payload);
        self.command_send_raw(&encode_system(&encrypted, Some(false))).await
    }

    async fn finish(&mut self, error: Option<HostError>) {
        if !self.running {
            return;
        }
        self.running = false;
        self.command = None;
        self.relay = None;
        self.relay_id += 1;
        self.registration_deadline = None;
        self.relay_deadline = None;
        self.viewer_deadline = None;
        self.keepalive_at = None;
        self.capture_at = None;
        self.control_allowed = false;
        self.peer = None;
        self.peer_phase = PeerPhase::None;
        self.side_switch.reset();
        self.clipboard_allowed = false;
        self.pending_frame = None;
        self.last_frame = None;
        self.capture_ticket = None;
        self.waiting_for_ack = false;
        self.relay_request = None;
        self.relay_address = None;
        self.registration_reply = None;
        self.lookups.clear();
        if let Some(error) = &error {
            log::info!("hosting stopped: {error}");
        }
        let _ = self.events.send(HostEvent::Stopped(error)).await;
    }
}

fn reject(command: Command) {
    match command {
        Command::Approve { reply, .. }
        | Command::SetControl { reply, .. }
        | Command::SendChat { reply, .. }
        | Command::SetClipboardSharing { reply, .. }
        | Command::SendClipboard { reply, .. } => {
            let _ = reply.send(Err(HostError::NotConnected));
        }
        Command::ApproveSideSwitch { reply, .. } => {
            let _ = reply.send(Err(HostError::NotConnected));
        }
        Command::IsControlAllowed { reply, .. } => {
            let _ = reply.send(false);
        }
        Command::Registration { reply } => {
            let _ = reply.send(None);
        }
        Command::SendPayload { reply, .. } => {
            let _ = reply.send(Err(HostError::NotConnected));
        }
        Command::Lookup { reply, .. } => {
            let _ = reply.send(Err(HostError::NotConnected));
        }
        Command::Stop { reply } => {
            let _ = reply.send(());
        }
        Command::Decline { .. }
        | Command::SubmitFrame { .. }
        | Command::EndViewer { .. }
        | Command::SendTyping { .. }
        | Command::DeclineSideSwitch { .. } => {}
    }
}
