//! Outgoing connection state for the UI: owns the protocol session, the input
//! sender, audio playback, cursor presentation and per-session tools.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gtk::glib;
use ultraviewer_protocol::commands::{CommandMessage, QualityPreset, RemoteAction, RemoteControlCommands, RemoteMonitor};
use ultraviewer_protocol::session::{self, FrameUpdate, PeerSummary, SessionConfiguration, SessionError, SessionEvent, SessionHandle};
use ultraviewer_protocol::CaptchaChallenge;
use ultraviewer_protocol::CursorMessage;

use super::audio::AudioPlayer;
use super::cursor::{CursorPresenter, Snapshot as CursorSnapshot};
use super::input::{InputQueue, QueueError, RemoteInput, RemoteInputEncoder};
use crate::i18n::{tr, trf};
use crate::runtime;
use crate::settings;
use crate::tools::{SessionTools, ToolsBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Connecting,
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageQuality {
    Automatic,
    Speed,
    Quality,
    Custom(i32),
}

#[derive(Debug, Clone)]
pub struct RemoteInfo {
    pub partner_id: String,
    pub display_name: Option<String>,
    pub allows_remote_input: bool,
    pub monitors: Vec<RemoteMonitor>,
    pub virtual_left: i32,
    pub virtual_top: i32,
    pub width: usize,
    pub height: usize,
}

impl RemoteInfo {
    pub fn title(&self) -> String {
        self.display_name.clone().unwrap_or_else(|| format_id(&self.partner_id))
    }
}

/// What changed, so views can update cheaply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    State,
    Frame,
    Cursor,
}

struct State {
    phase: Phase,
    partner_id: String,
    password: String,
    status: String,
    error: Option<String>,
    credential_message: Option<String>,
    info: Option<RemoteInfo>,
    selected_monitor: usize,
    control_enabled: bool,
    audio_enabled: bool,
    changing_audio: bool,
    desired_audio: bool,
    show_remote_cursor: bool,
    wallpaper_visible: bool,
    quality: ImageQuality,
    generation: u64,
    session: Option<SessionHandle>,
    pending_credential: Option<(String, String)>,
    /// Registration stopped for UltraViewer's captcha; the connection is
    /// retried with these credentials once it is solved.
    captcha: Option<(CaptchaChallenge, String, String)>,
    frame: Option<FrameUpdate>,
    queue: Option<Arc<InputQueue>>,
    encoder: Option<Arc<Mutex<RemoteInputEncoder>>>,
    allows_input: Arc<AtomicBool>,
    supports_binary_keyboard: bool,
    recent: Vec<String>,
}

pub type CommandPortProvider = Rc<dyn Fn() -> Option<ultraviewer_protocol::host::CommandPort>>;

pub struct ViewerModel {
    state: RefCell<State>,
    command_port: RefCell<Option<CommandPortProvider>>,
    listeners: RefCell<Vec<Rc<dyn Fn(Change)>>>,
    cursor: RefCell<CursorPresenter>,
    audio: RefCell<AudioPlayer>,
    pub tools: Rc<SessionTools>,
}

pub fn normalize_id(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace() && *c != '-').collect()
}

/// "123456789" → "123 456 789".
pub fn format_id(id: &str) -> String {
    let digits: Vec<char> = id.chars().collect();
    let mut groups = Vec::new();
    let mut end = digits.len();
    while end > 0 {
        let start = end.saturating_sub(3);
        groups.insert(0, digits[start..end].iter().collect::<String>());
        end = start;
    }
    groups.join(" ")
}

pub fn error_text(error: &SessionError) -> String {
    tr(&error.to_string())
}

impl ViewerModel {
    pub fn new() -> Rc<Self> {
        let settings = settings::get();
        let recent = if settings.remember_partners { settings.recent_partners.clone() } else { Vec::new() };
        Rc::new(Self {
            state: RefCell::new(State {
                phase: Phase::Idle,
                partner_id: String::new(),
                password: String::new(),
                status: tr("Enter the ID and password from the other computer."),
                error: None,
                credential_message: None,
                info: None,
                selected_monitor: 0,
                control_enabled: false,
                audio_enabled: false,
                changing_audio: false,
                desired_audio: false,
                show_remote_cursor: settings.show_remote_cursor,
                wallpaper_visible: true,
                quality: ImageQuality::Automatic,
                generation: 0,
                session: None,
                pending_credential: None,
                captcha: None,
                frame: None,
                queue: None,
                encoder: None,
                allows_input: Arc::new(AtomicBool::new(false)),
                supports_binary_keyboard: true,
                recent,
            }),
            listeners: RefCell::default(),
            command_port: RefCell::new(None),
            cursor: RefCell::new(CursorPresenter::new()),
            audio: RefCell::new(AudioPlayer::new()),
            tools: SessionTools::new(Rc::new(|| settings::get().share_clipboard)),
        })
    }

    /// Supplies the sharing registration when this computer is also shared.
    pub fn set_command_port_provider(&self, provider: CommandPortProvider) {
        *self.command_port.borrow_mut() = Some(provider);
    }

    pub fn connect_changed(&self, listener: impl Fn(Change) + 'static) {
        self.listeners.borrow_mut().push(Rc::new(listener));
    }

    fn notify(&self, change: Change) {
        let listeners: Vec<Rc<dyn Fn(Change)>> = self.listeners.borrow().clone();
        for listener in listeners {
            listener(change);
        }
    }

    // ------------------------------------------------------------ getters

    pub fn phase(&self) -> Phase {
        self.state.borrow().phase
    }
    pub fn is_session_active(&self) -> bool {
        matches!(self.phase(), Phase::Connecting | Phase::Connected)
    }
    pub fn status(&self) -> String {
        tr(&self.state.borrow().status)
    }
    pub fn error(&self) -> Option<String> {
        self.state.borrow().error.clone()
    }
    pub fn credential_message(&self) -> Option<String> {
        self.state.borrow().credential_message.clone()
    }
    pub fn partner_id(&self) -> String {
        self.state.borrow().partner_id.clone()
    }
    pub fn password(&self) -> String {
        self.state.borrow().password.clone()
    }
    pub fn info(&self) -> Option<RemoteInfo> {
        self.state.borrow().info.clone()
    }
    pub fn selected_monitor(&self) -> usize {
        self.state.borrow().selected_monitor
    }
    pub fn control_enabled(&self) -> bool {
        self.state.borrow().control_enabled
    }
    pub fn audio_enabled(&self) -> bool {
        self.state.borrow().audio_enabled
    }
    pub fn changing_audio(&self) -> bool {
        self.state.borrow().changing_audio
    }
    pub fn show_remote_cursor(&self) -> bool {
        self.state.borrow().show_remote_cursor
    }
    pub fn wallpaper_visible(&self) -> bool {
        self.state.borrow().wallpaper_visible
    }
    pub fn quality(&self) -> ImageQuality {
        self.state.borrow().quality
    }
    pub fn frame(&self) -> Option<FrameUpdate> {
        self.state.borrow().frame.clone()
    }
    pub fn recent_partners(&self) -> Vec<String> {
        self.state.borrow().recent.clone()
    }
    pub fn cursor_snapshot(&self) -> CursorSnapshot {
        self.cursor.borrow().snapshot().clone()
    }

    #[cfg_attr(not(feature = "captcha"), allow(dead_code))]
    pub fn captcha(&self) -> Option<CaptchaChallenge> {
        self.state.borrow().captcha.as_ref().map(|(challenge, _, _)| challenge.clone())
    }

    #[cfg_attr(not(feature = "captcha"), allow(dead_code))]
    /// Restores the interrupted connection's credentials; returns whether
    /// `connect` should be called again.
    pub fn solve_captcha(&self, token: String) -> bool {
        crate::profile::set_captcha_token(token);
        let mut state = self.state.borrow_mut();
        let Some((_, id, password)) = state.captcha.take() else { return false };
        state.partner_id = id;
        state.password = password;
        true
    }

    #[cfg_attr(not(feature = "captcha"), allow(dead_code))]
    pub fn dismiss_captcha(&self) {
        if self.state.borrow_mut().captcha.take().is_some() {
            self.notify(Change::State);
        }
    }

    pub fn clear_error(&self) {
        if self.state.borrow_mut().error.take().is_some() {
            self.notify(Change::State);
        }
    }

    /// The selected monitor's region in decoded pixels, if any.
    pub fn selected_region(&self) -> Option<(i64, i64, i64, i64)> {
        let state = self.state.borrow();
        let info = state.info.as_ref()?;
        let frame = state.frame.as_ref()?;
        let monitor = info.monitors.iter().find(|m| m.index == state.selected_monitor)?;
        let region = monitor.region(info.virtual_left, info.virtual_top, frame.bitmap.width, frame.bitmap.height).ok()?;
        Some((region.x, region.y, region.width, region.height))
    }

    // ------------------------------------------------------------ credentials

    pub fn set_partner_id(self: &Rc<Self>, text: &str) {
        let normalized = normalize_id(text);
        let changed = {
            let mut state = self.state.borrow_mut();
            if state.phase == Phase::Connecting || state.phase == Phase::Connected {
                return;
            }
            let changed = state.partner_id != normalized;
            state.partner_id = normalized.clone();
            changed
        };
        if changed {
            self.fill_saved_password();
        }
    }

    pub fn set_password(&self, password: &str) {
        self.state.borrow_mut().password = password.to_string();
    }

    fn fill_saved_password(self: &Rc<Self>) {
        let id = {
            let mut state = self.state.borrow_mut();
            state.password.clear();
            state.credential_message = None;
            state.partner_id.clone()
        };
        if !settings::get().remember_password || id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_digit()) {
            self.notify(Change::State);
            return;
        }
        let model = self.clone();
        glib::spawn_future_local(async move {
            let result = runtime::run(crate::credentials::password(id.clone())).await;
            {
                let mut state = model.state.borrow_mut();
                if state.partner_id != id || matches!(state.phase, Phase::Connecting | Phase::Connected) {
                    return;
                }
                match result {
                    Ok(Some(password)) => state.password = password,
                    Ok(None) => {}
                    Err(error) => state.credential_message = Some(trf("Saved password unavailable: {} Enter it manually.", &[&error.to_string()])),
                }
            }
            model.notify(Change::State);
        });
    }

    pub fn select_recent(self: &Rc<Self>, id: &str) {
        if self.is_session_active() {
            return;
        }
        self.state.borrow_mut().partner_id = String::new();
        self.set_partner_id(id);
        self.state.borrow_mut().error = None;
        self.notify(Change::State);
    }

    pub fn remove_recent(&self, id: &str) {
        self.state.borrow_mut().recent.retain(|r| r != id);
        let recent = self.recent_partners();
        settings::update(|s| s.recent_partners = recent);
        self.notify(Change::State);
    }

    pub fn clear_recent(&self) {
        self.state.borrow_mut().recent.clear();
        settings::update(|s| s.recent_partners.clear());
        self.notify(Change::State);
    }

    fn remember_partner(&self, id: &str) {
        if !settings::get().remember_partners {
            return;
        }
        let recent = {
            let mut state = self.state.borrow_mut();
            state.recent.retain(|r| r != id);
            state.recent.insert(0, id.to_string());
            state.recent.truncate(10);
            state.recent.clone()
        };
        settings::update(|s| s.recent_partners = recent);
    }

    // ------------------------------------------------------------ connection

    pub fn connect(self: &Rc<Self>, local_screen: (i32, i32)) {
        if self.is_session_active() {
            return;
        }
        let (id, password) = {
            let state = self.state.borrow();
            (state.partner_id.clone(), state.password.clone())
        };
        if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_digit()) {
            self.state.borrow_mut().error = Some(tr("Enter the partner ID shown in UltraViewer on the other computer."));
            self.notify(Change::State);
            return;
        }
        if password.is_empty() {
            self.state.borrow_mut().error = Some(tr("Enter the password shown on the other computer."));
            self.notify(Change::State);
            return;
        }
        let generation = {
            let mut state = self.state.borrow_mut();
            state.password.clear();
            state.error = None;
            state.credential_message = None;
            state.info = None;
            state.frame = None;
            state.selected_monitor = 0;
            state.phase = Phase::Connecting;
            state.status = trf("Connecting to {}…", &[&format_id(&id)]);
            state.generation += 1;
            state.pending_credential = Some((id.clone(), password.clone()));
            state.captcha = None;
            state.quality = ImageQuality::Automatic;
            state.wallpaper_visible = true;
            state.generation
        };
        self.stop_audio();
        self.cursor.borrow_mut().reset();
        self.notify(Change::State);

        let profile = match crate::profile::load_or_create() {
            Ok(profile) => profile,
            Err(error) => return self.fail(tr(&error.to_string()), generation),
        };
        let mut configuration = SessionConfiguration::new(profile, local_screen.0.max(1), local_screen.1.max(1), "LinuxViewer");
        configuration.enable_lan_direct = settings::get().enable_lan_direct;
        configuration.shared_command = self.command_port.borrow().as_ref().and_then(|provider| provider());
        let streams = {
            let _guard = runtime::runtime().enter();
            session::connect(configuration, &id, &password)
        };
        let streams = match streams {
            Ok(streams) => streams,
            Err(error) => return self.fail(error_text(&error), generation),
        };
        self.state.borrow_mut().session = Some(streams.handle.clone());

        // Frames: always the newest; stale ones are skipped.
        let model = Rc::downgrade(self);
        let mut frames = streams.frames;
        glib::spawn_future_local(async move {
            while frames.changed().await.is_ok() {
                let update = frames.borrow_and_update().clone();
                let Some(model) = model.upgrade() else { return };
                if model.state.borrow().generation != generation {
                    return;
                }
                if let Some(update) = update {
                    if model.phase() == Phase::Connected {
                        model.state.borrow_mut().frame = Some(update);
                        model.notify(Change::Frame);
                    }
                }
            }
        });

        let model = Rc::downgrade(self);
        let mut events = streams.events;
        glib::spawn_future_local(async move {
            while let Some(event) = events.recv().await {
                let Some(model) = model.upgrade() else { return };
                if model.state.borrow().generation != generation {
                    return;
                }
                let closed = matches!(event, SessionEvent::Closed(_));
                model.receive(event, generation);
                if closed {
                    return;
                }
            }
        });
    }

    fn receive(self: &Rc<Self>, event: SessionEvent, generation: u64) {
        match event {
            SessionEvent::Status(message) => {
                self.state.borrow_mut().status = message;
                self.notify(Change::State);
            }
            SessionEvent::Accepted(peer) => self.accepted(peer, generation),
            SessionEvent::ScreenChanged(peer) => {
                if self.phase() != Phase::Connected {
                    return;
                }
                {
                    let mut state = self.state.borrow_mut();
                    let partner_id = state.info.as_ref().map(|i| i.partner_id.clone()).unwrap_or_default();
                    state.info = Some(remote_info(&partner_id, &peer));
                    if !peer.monitors.iter().any(|m| m.index == state.selected_monitor) {
                        state.selected_monitor = 0;
                    }
                    state.allows_input.store(peer.allows_remote_input, Ordering::SeqCst);
                }
                self.input_permission_changed(peer.allows_remote_input);
                self.notify(Change::State);
            }
            SessionEvent::InputPermission(allowed) => {
                if self.phase() != Phase::Connected {
                    return;
                }
                self.state.borrow().allows_input.store(allowed, Ordering::SeqCst);
                self.input_permission_changed(allowed);
                self.notify(Change::State);
            }
            SessionEvent::Chat(text) => self.tools.receive_chat(text),
            SessionEvent::Typing(typing) => self.tools.receive_typing(typing),
            SessionEvent::Clipboard(text) => self.tools.receive_clipboard(text),
            SessionEvent::ComputerAudio(pcm) => {
                if self.phase() != Phase::Connected || !self.audio_enabled() {
                    return;
                }
                let result = self.audio.borrow_mut().play(pcm);
                if let Err(error) = result {
                    self.audio_failed(error);
                }
            }
            SessionEvent::Cursor(message) => self.cursor_message(message),
            SessionEvent::Closed(error) => {
                let phase = self.phase();
                match (phase, error) {
                    (Phase::Connecting, Some(SessionError::CaptchaRequired(challenge))) => {
                        let credential = self.state.borrow().pending_credential.clone();
                        self.fail(crate::profile::captcha_message("Solve UltraViewer's captcha to continue connecting."), generation);
                        if let Some((id, password)) = credential {
                            self.state.borrow_mut().captcha = Some((challenge, id, password));
                            self.notify(Change::State);
                        }
                    }
                    (Phase::Connecting, Some(error)) => self.fail(error_text(&error), generation),
                    (Phase::Connecting, None) => self.fail(tr("The connection ended before the other computer authenticated the session."), generation),
                    (Phase::Connected, Some(error)) => {
                        let text = error_text(&error);
                        self.end_session(tr("The remote session ended with an error."), Some(text));
                    }
                    (Phase::Connected, None) => self.end_session(tr("The remote session ended."), None),
                    _ => {}
                }
            }
        }
    }

    fn accepted(self: &Rc<Self>, peer: PeerSummary, generation: u64) {
        if self.phase() != Phase::Connecting {
            return;
        }
        let (id, handle) = {
            let mut state = self.state.borrow_mut();
            let Some((id, _)) = state.pending_credential.clone() else { return };
            let info = remote_info(&id, &peer);
            state.status = trf("Connected to {}.", &[&info.title()]);
            state.info = Some(info);
            state.phase = Phase::Connected;
            state.allows_input.store(peer.allows_remote_input, Ordering::SeqCst);
            state.supports_binary_keyboard = peer.supports_binary_keyboard;
            (id, state.session.clone())
        };
        self.remember_partner(&id);
        if let Some((pending_id, password)) = self.state.borrow_mut().pending_credential.take() {
            if settings::get().remember_password && pending_id == id {
                let model = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    if let Err(error) = runtime::run(crate::credentials::store(pending_id, password)).await {
                        if let Some(model) = model.upgrade() {
                            model.state.borrow_mut().credential_message = Some(trf("Connected, but the password could not be saved: {}", &[&error.to_string()]));
                            model.notify(Change::State);
                        }
                    }
                });
            }
        }
        self.start_input(false);
        let show_cursor = self.show_remote_cursor();
        self.send_commands(vec![RemoteControlCommands::cursor_positions(show_cursor)], false);
        if let Some(handle) = handle {
            self.activate_tools(handle, generation);
        }
        self.notify(Change::State);
    }

    fn activate_tools(self: &Rc<Self>, handle: SessionHandle, generation: u64) {
        let model = Rc::downgrade(self);
        let alive = move || model.upgrade().is_some_and(|m| m.state.borrow().generation == generation && m.phase() == Phase::Connected);
        let (h1, h2, h3, h4) = (handle.clone(), handle.clone(), handle.clone(), handle);
        let (a1, a2, a3, a4) = (alive.clone(), alive.clone(), alive.clone(), alive);
        self.tools.activate(ToolsBackend {
            send_chat: Rc::new(move |text| {
                let handle = h1.clone();
                let alive = a1();
                Box::pin(async move {
                    if !alive {
                        return Err(String::new());
                    }
                    handle.send_chat(text).await.map_err(|e| e.to_string())
                })
            }),
            send_clipboard: Rc::new(move |text| {
                let handle = h2.clone();
                let alive = a2();
                Box::pin(async move {
                    if !alive {
                        return Err(String::new());
                    }
                    handle.send_clipboard(text).await.map_err(|e| e.to_string())
                })
            }),
            set_sharing: Rc::new(move |enabled| {
                let handle = h3.clone();
                let alive = a3();
                Box::pin(async move {
                    if !alive {
                        return Err(String::new());
                    }
                    handle.set_clipboard_sharing(enabled).await.map_err(|e| e.to_string())
                })
            }),
            send_typing: Rc::new(move |typing| {
                let handle = h4.clone();
                let alive = a4();
                Box::pin(async move {
                    if !alive {
                        return Err(String::new());
                    }
                    handle.send_typing(typing).await.map_err(|e| e.to_string())
                })
            }),
        });
    }

    fn input_permission_changed(self: &Rc<Self>, allowed: bool) {
        let was_allowed = self.state.borrow().info.as_ref().is_some_and(|i| i.allows_remote_input);
        if !allowed {
            self.enqueue(RemoteInput::ReleaseAll);
            self.release_through_encoder();
            self.state.borrow_mut().control_enabled = false;
        }
        if let Some(info) = self.state.borrow_mut().info.as_mut() {
            info.allows_remote_input = allowed;
        }
        if allowed && !was_allowed {
            // A screen refresh may have suspended an in-flight input write.
            self.start_input(true);
            self.state.borrow_mut().error = None;
        }
    }

    fn cursor_message(self: &Rc<Self>, message: CursorMessage) {
        if self.phase() != Phase::Connected {
            return;
        }
        let request = self.cursor.borrow_mut().consume(message);
        if let Some(hash) = request {
            self.send_commands(vec![CommandMessage { payload: CursorMessage::shape_request(hash), format_flag: false }], false);
        }
        self.notify(Change::Cursor);
    }

    pub fn disconnect(self: &Rc<Self>) {
        let handle = {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.pending_credential = None;
            state.phase = Phase::Disconnected;
            state.status = tr("You disconnected from the remote computer.");
            state.frame = None;
            state.info = None;
            state.selected_monitor = 0;
            state.password.clear();
            state.session.take()
        };
        let releases = self.stop_input();
        self.stop_audio();
        self.cursor.borrow_mut().reset();
        self.tools.deactivate();
        if let Some(handle) = handle {
            runtime::spawn(async move {
                for payload in releases {
                    let _ = handle.send(payload).await;
                }
                handle.close().await;
            });
        }
        self.notify(Change::State);
    }

    /// Disconnects and waits until the remote computer has been told.
    pub async fn shutdown(self: &Rc<Self>) {
        let handle = self.state.borrow().session.clone();
        let releases = self.stop_input();
        if let Some(handle) = handle {
            let _ = runtime::run(async move {
                for payload in releases {
                    let _ = handle.send(payload).await;
                }
                handle.close().await;
            })
            .await;
        }
        self.disconnect();
    }

    fn fail(self: &Rc<Self>, message: String, generation: u64) {
        if self.state.borrow().generation != generation {
            return;
        }
        let had_connected = self.phase() == Phase::Connected;
        let handle = {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.pending_credential = None;
            state.phase = Phase::Disconnected;
            state.status = if had_connected { tr("The remote session ended with an error.") } else { tr("The connection could not be established.") };
            state.error = Some(message);
            state.frame = None;
            state.info = None;
            state.selected_monitor = 0;
            state.session.take()
        };
        self.stop_input();
        self.stop_audio();
        self.cursor.borrow_mut().reset();
        self.tools.deactivate();
        if let Some(handle) = handle {
            runtime::spawn(async move { handle.close().await });
        }
        self.notify(Change::State);
    }

    fn end_session(self: &Rc<Self>, message: String, error: Option<String>) {
        let handle = {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.pending_credential = None;
            state.phase = Phase::Disconnected;
            state.status = message;
            if error.is_some() {
                state.error = error;
            }
            state.frame = None;
            state.info = None;
            state.selected_monitor = 0;
            state.session.take()
        };
        self.stop_input();
        self.stop_audio();
        self.cursor.borrow_mut().reset();
        self.tools.deactivate();
        if let Some(handle) = handle {
            runtime::spawn(async move { handle.close().await });
        }
        self.notify(Change::State);
    }

    // ------------------------------------------------------------ input

    pub fn set_control_enabled(self: &Rc<Self>, enabled: bool) {
        let allowed = self.phase() == Phase::Connected && self.info().is_some_and(|i| i.allows_remote_input);
        if !allowed {
            return;
        }
        if enabled {
            let needs_queue = self.state.borrow().queue.as_ref().is_none_or(|q| q.is_finished());
            if needs_queue {
                self.start_input(false);
                self.state.borrow_mut().error = None;
            }
        } else {
            self.enqueue(RemoteInput::ReleaseAll);
        }
        self.state.borrow_mut().control_enabled = enabled;
        self.notify(Change::State);
    }

    /// Input from the screen widget, in displayed (possibly cropped) pixels.
    pub fn send(self: &Rc<Self>, input: RemoteInput) {
        if self.phase() != Phase::Connected || !self.control_enabled() {
            if input == RemoteInput::ReleaseAll {
                self.enqueue(input);
            }
            return;
        }
        let region = self.selected_region();
        let info = self.info();
        let translate = |x: i64, y: i64| -> (i64, i64) {
            let (mut x, mut y) = (x, y);
            if let Some((rx, ry, _, _)) = region {
                x += rx;
                y += ry;
            }
            match &info {
                Some(info) => (
                    x.clamp(0, info.width as i64 - 1) + i64::from(info.virtual_left),
                    y.clamp(0, info.height as i64 - 1) + i64::from(info.virtual_top),
                ),
                None => (x, y),
            }
        };
        let translated = match input {
            RemoteInput::PointerMoved { x, y } => {
                let (x, y) = translate(x, y);
                RemoteInput::PointerMoved { x, y }
            }
            RemoteInput::PointerButton { button, pressed, x, y } => {
                let (x, y) = translate(x, y);
                RemoteInput::PointerButton { button, pressed, x, y }
            }
            other => other,
        };
        self.enqueue(translated);
    }

    pub fn release_remote_input(self: &Rc<Self>) {
        if self.phase() == Phase::Connected {
            self.enqueue(RemoteInput::ReleaseAll);
        }
    }

    fn enqueue(self: &Rc<Self>, input: RemoteInput) {
        let Some(queue) = self.state.borrow().queue.clone() else { return };
        if queue.is_finished() {
            return;
        }
        if let Err(QueueError::Overflow) = queue.enqueue(input) {
            // Keep the existing sender responsible for release after its
            // in-flight write completes.
            queue.finish();
            self.release_through_encoder();
            let mut state = self.state.borrow_mut();
            state.control_enabled = false;
            state.error = Some(tr("Remote input stopped because its queue filled. Turn on Control again to retry."));
            drop(state);
            self.notify(Change::State);
        }
    }

    /// Sends an ordered release from the shared encoder outside the queue.
    fn release_through_encoder(&self) {
        let (encoder, handle) = {
            let state = self.state.borrow();
            (state.encoder.clone(), state.session.clone())
        };
        let (Some(encoder), Some(handle)) = (encoder, handle) else { return };
        let payloads = encoder.lock().unwrap_or_else(|e| e.into_inner()).encode(&RemoteInput::ReleaseAll);
        if payloads.is_empty() {
            return;
        }
        runtime::spawn(async move {
            for payload in payloads {
                let _ = handle.send(payload).await;
            }
        });
    }

    fn start_input(self: &Rc<Self>, releasing_held: bool) {
        self.stop_input_queue();
        let Some(handle) = self.state.borrow().session.clone() else { return };
        let queue = InputQueue::new(8192);
        let encoder = {
            let mut state = self.state.borrow_mut();
            let supports = state.supports_binary_keyboard;
            let encoder = state.encoder.get_or_insert_with(|| Arc::new(Mutex::new(RemoteInputEncoder::new(supports)))).clone();
            state.queue = Some(queue.clone());
            encoder
        };
        if releasing_held {
            let _ = queue.enqueue(RemoteInput::ReleaseAll);
        }
        let allows = self.state.borrow().allows_input.clone();
        let (failure_tx, failure_rx) = tokio::sync::oneshot::channel::<String>();
        let worker_queue = queue.clone();
        runtime::spawn(async move {
            while let Some(input) = worker_queue.next().await {
                if input != RemoteInput::ReleaseAll && !allows.load(Ordering::SeqCst) {
                    continue;
                }
                let payloads = encoder.lock().unwrap_or_else(|e| e.into_inner()).encode(&input);
                for payload in payloads {
                    if let Err(error) = handle.send(payload).await {
                        worker_queue.finish();
                        let _ = failure_tx.send(error_text(&error));
                        return;
                    }
                }
            }
        });
        let model = Rc::downgrade(self);
        let generation = self.state.borrow().generation;
        glib::spawn_future_local(async move {
            if let Ok(message) = failure_rx.await {
                let Some(model) = model.upgrade() else { return };
                if model.state.borrow().generation != generation || model.phase() != Phase::Connected {
                    return;
                }
                {
                    let mut state = model.state.borrow_mut();
                    state.control_enabled = false;
                    if state.info.as_ref().is_some_and(|i| i.allows_remote_input) {
                        state.error = Some(trf("Remote input stopped: {}", &[&message]));
                    }
                }
                model.release_through_encoder();
                model.notify(Change::State);
            }
        });
    }

    fn stop_input_queue(&self) {
        if let Some(queue) = self.state.borrow_mut().queue.take() {
            queue.finish();
        }
    }

    /// Stops the sender and returns release payloads for held input.
    fn stop_input(&self) -> Vec<Vec<u8>> {
        self.stop_input_queue();
        let mut state = self.state.borrow_mut();
        state.control_enabled = false;
        state.encoder.take().map(|e| e.lock().unwrap_or_else(|e| e.into_inner()).encode(&RemoteInput::ReleaseAll)).unwrap_or_default()
    }

    // ------------------------------------------------------------ commands

    pub fn select_monitor(self: &Rc<Self>, index: usize) {
        self.release_remote_input();
        self.state.borrow_mut().selected_monitor = index;
        self.notify(Change::State);
        self.notify(Change::Frame);
    }

    pub fn perform_action(self: &Rc<Self>, action: RemoteAction) {
        self.send_commands(vec![RemoteControlCommands::action(action)], true);
    }

    pub fn set_quality(self: &Rc<Self>, quality: ImageQuality) {
        self.state.borrow_mut().quality = quality;
        let messages = match quality {
            ImageQuality::Automatic => RemoteControlCommands::quality_preset(QualityPreset::Automatic),
            ImageQuality::Speed => RemoteControlCommands::quality_preset(QualityPreset::Speed),
            ImageQuality::Quality => RemoteControlCommands::quality_preset(QualityPreset::Quality),
            ImageQuality::Custom(value) => vec![RemoteControlCommands::rgb555_color_mode(), RemoteControlCommands::quality(value)],
        };
        self.send_commands(messages, false);
        self.notify(Change::State);
    }

    pub fn set_wallpaper_visible(self: &Rc<Self>, visible: bool) {
        self.state.borrow_mut().wallpaper_visible = visible;
        self.send_commands(vec![RemoteControlCommands::wallpaper(visible)], true);
        self.notify(Change::State);
    }

    pub fn set_show_remote_cursor(self: &Rc<Self>, enabled: bool) {
        self.state.borrow_mut().show_remote_cursor = enabled;
        settings::update(|s| s.show_remote_cursor = enabled);
        self.send_commands(vec![RemoteControlCommands::cursor_positions(enabled)], false);
        self.notify(Change::Cursor);
        self.notify(Change::State);
    }

    pub fn refresh_screen(self: &Rc<Self>) {
        let Some(handle) = self.state.borrow().session.clone() else { return };
        runtime::spawn(async move {
            let _ = handle.refresh_screen().await;
        });
    }

    pub fn send_commands(self: &Rc<Self>, messages: Vec<CommandMessage>, requires_control: bool) {
        if self.phase() != Phase::Connected || (requires_control && !self.control_enabled()) {
            return;
        }
        let Some(handle) = self.state.borrow().session.clone() else { return };
        let generation = self.state.borrow().generation;
        let model = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            for message in messages {
                if let Err(error) = handle.send_command(message, requires_control).await {
                    if let Some(model) = model.upgrade() {
                        if model.state.borrow().generation == generation {
                            model.state.borrow_mut().error = Some(trf("The remote action could not be completed: {}", &[&error_text(&error)]));
                            model.notify(Change::State);
                        }
                    }
                    return;
                }
            }
        });
    }

    // ------------------------------------------------------------ audio

    pub fn set_computer_audio(self: &Rc<Self>, enabled: bool) {
        if self.phase() != Phase::Connected {
            return;
        }
        let Some(handle) = self.state.borrow().session.clone() else { return };
        {
            let mut state = self.state.borrow_mut();
            state.error = None;
            state.desired_audio = enabled;
            state.changing_audio = true;
            if !enabled {
                state.audio_enabled = false;
            }
        }
        if !enabled {
            self.audio.borrow_mut().stop();
        }
        self.notify(Change::State);
        let generation = self.state.borrow().generation;
        let model = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = handle.set_computer_audio(enabled).await;
            let Some(model) = model.upgrade() else { return };
            if model.state.borrow().generation != generation || model.state.borrow().desired_audio != enabled {
                return;
            }
            let outcome = match result {
                Ok(()) => model.audio.borrow_mut().set_enabled(enabled),
                Err(error) => Err(error_text(&error)),
            };
            {
                let mut state = model.state.borrow_mut();
                state.changing_audio = false;
                match outcome {
                    Ok(()) => state.audio_enabled = enabled,
                    Err(message) => {
                        state.audio_enabled = false;
                        state.error = Some(trf("Computer audio stopped: {}", &[&message]));
                    }
                }
            }
            if !model.audio_enabled() && enabled {
                // One serialized disable after a failed enable.
                let _ = handle.set_computer_audio(false).await;
            }
            model.notify(Change::State);
        });
    }

    fn audio_failed(self: &Rc<Self>, message: String) {
        self.audio.borrow_mut().stop();
        {
            let mut state = self.state.borrow_mut();
            state.audio_enabled = false;
            state.error = Some(trf("Computer audio stopped: {}", &[&message]));
        }
        if let Some(handle) = self.state.borrow().session.clone() {
            runtime::spawn(async move {
                let _ = handle.set_computer_audio(false).await;
            });
        }
        self.notify(Change::State);
    }

    fn stop_audio(&self) {
        self.audio.borrow_mut().stop();
        let mut state = self.state.borrow_mut();
        state.audio_enabled = false;
        state.changing_audio = false;
        state.desired_audio = false;
    }
}

fn remote_info(partner_id: &str, peer: &PeerSummary) -> RemoteInfo {
    RemoteInfo {
        partner_id: partner_id.to_string(),
        display_name: peer.display_name.clone(),
        allows_remote_input: peer.allows_remote_input,
        monitors: peer.monitors.clone(),
        virtual_left: peer.virtual_left,
        virtual_top: peer.virtual_top,
        width: peer.width,
        height: peer.height,
    }
}
