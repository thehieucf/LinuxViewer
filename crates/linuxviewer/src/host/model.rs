//! Incoming access state for the UI: registration, explicit approval, screen
//! capture, input injection and per-session tools.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};
use ultraviewer_protocol::host::{self, HostConfiguration, HostError, HostEvent, HostHandle, HostedScreenFrame, IncomingRequest, SideSwitchApproval};
use ultraviewer_protocol::CaptchaChallenge;

use super::capture::{self, x11::X11Capture, PortalSession, RawFrame};
use super::inject::{InjectBackend, InputController, PortalBackend, UinputBackend, XTestBackend};
use crate::i18n::{tr, trf};
use crate::runtime;
use crate::settings;
use crate::tools::{SessionTools, ToolsBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPhase {
    Off,
    Registering,
    Ready,
    AwaitingApproval,
    Sharing,
    Failed,
}

/// Where shared frames come from.
enum Source {
    Portal(Arc<PortalSession>),
    X11(Arc<Mutex<X11Capture>>),
}

enum InjectorMessage {
    Payload(Vec<u8>),
    Frame(usize, usize),
    ReleaseAll,
}

/// Owns the input controller on a dedicated thread.
struct Injector {
    sender: mpsc::Sender<InjectorMessage>,
}

impl Injector {
    fn start(backend: Box<dyn InjectBackend>, failures: tokio::sync::mpsc::UnboundedSender<String>) -> Self {
        let (sender, receiver) = mpsc::channel::<InjectorMessage>();
        std::thread::Builder::new()
            .name("linuxviewer-input".into())
            .spawn(move || {
                let mut controller = InputController::new(backend);
                while let Ok(message) = receiver.recv() {
                    let result = match message {
                        InjectorMessage::Payload(payload) => controller.handle(&payload).map(|_| ()),
                        InjectorMessage::Frame(width, height) => {
                            controller.set_frame(width, height);
                            Ok(())
                        }
                        InjectorMessage::ReleaseAll => {
                            controller.release_all();
                            Ok(())
                        }
                    };
                    if let Err(error) = result {
                        controller.release_all();
                        let _ = failures.send(error);
                        return;
                    }
                }
            })
            .expect("input thread");
        Self { sender }
    }

    fn send(&self, message: InjectorMessage) {
        let _ = self.sender.send(message);
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        let _ = self.sender.send(InjectorMessage::ReleaseAll);
    }
}

struct State {
    phase: HostPhase,
    identifier: Option<String>,
    password: String,
    status: String,
    error: Option<String>,
    captcha: Option<CaptchaChallenge>,
    pending_request: Option<IncomingRequest>,
    viewer: Option<IncomingRequest>,
    allows_control: bool,
    approving: bool,
    ending_viewer: bool,
    pending_side_switch: Option<SideSwitchApproval>,
    approving_side_switch: bool,
    generation: u64,
    handle: Option<HostHandle>,
    source: Option<Source>,
    injector: Option<Injector>,
    frame_size: Option<(usize, usize)>,
}

pub struct HostingModel {
    state: RefCell<State>,
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
    pub tools: Rc<SessionTools>,
    /// Invoked after the local user approved a side switch: (id, password).
    pub on_side_switch: RefCell<Option<Rc<dyn Fn(String, String)>>>,
}

fn host_error_text(error: &HostError) -> String {
    tr(&error.to_string())
}

/// Logical desktop layout bounding box and the monitors' rectangles.
fn desktop_layout() -> ((f64, f64, f64, f64), Vec<(f64, f64, f64, f64)>) {
    let mut rects = Vec::new();
    if let Some(display) = gdk::Display::default() {
        let monitors = display.monitors();
        for index in 0..monitors.n_items() {
            if let Some(monitor) = monitors.item(index).and_then(|m| m.downcast::<gdk::Monitor>().ok()) {
                let g = monitor.geometry();
                rects.push((f64::from(g.x()), f64::from(g.y()), f64::from(g.width()), f64::from(g.height())));
            }
        }
    }
    if rects.is_empty() {
        return ((0.0, 0.0, 1920.0, 1080.0), vec![(0.0, 0.0, 1920.0, 1080.0)]);
    }
    let left = rects.iter().map(|r| r.0).fold(f64::MAX, f64::min);
    let top = rects.iter().map(|r| r.1).fold(f64::MAX, f64::min);
    let right = rects.iter().map(|r| r.0 + r.2).fold(f64::MIN, f64::max);
    let bottom = rects.iter().map(|r| r.1 + r.3).fold(f64::MIN, f64::max);
    ((left, top, right - left, bottom - top), rects)
}

/// Which input path this desktop offers, for the setup card.
pub fn input_support() -> Option<&'static str> {
    if std::env::var("XDG_SESSION_TYPE").ok().as_deref() == Some("x11") {
        Some("XTest")
    } else if UinputBackend::available() {
        Some("uinput")
    } else {
        None
    }
}

impl HostingModel {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            state: RefCell::new(State {
                phase: HostPhase::Off,
                identifier: None,
                password: String::new(),
                status: tr("Start sharing to get this computer's UltraViewer ID."),
                error: None,
                captcha: None,
                pending_request: None,
                viewer: None,
                allows_control: false,
                approving: false,
                ending_viewer: false,
                pending_side_switch: None,
                approving_side_switch: false,
                generation: 0,
                handle: None,
                source: None,
                injector: None,
                frame_size: None,
            }),
            listeners: RefCell::default(),
            tools: SessionTools::new(Rc::new(|| settings::get().share_clipboard)),
            on_side_switch: RefCell::new(None),
        })
    }

    pub fn connect_changed(&self, listener: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Rc::new(listener));
    }

    fn notify(&self) {
        let listeners: Vec<Rc<dyn Fn()>> = self.listeners.borrow().clone();
        for listener in listeners {
            listener();
        }
    }

    pub fn phase(&self) -> HostPhase {
        self.state.borrow().phase
    }
    pub fn is_running(&self) -> bool {
        !matches!(self.phase(), HostPhase::Off | HostPhase::Failed)
    }
    pub fn identifier(&self) -> Option<String> {
        self.state.borrow().identifier.clone()
    }
    pub fn password(&self) -> String {
        self.state.borrow().password.clone()
    }
    pub fn status(&self) -> String {
        tr(&self.state.borrow().status)
    }
    pub fn error(&self) -> Option<String> {
        self.state.borrow().error.clone()
    }
    pub fn pending_request(&self) -> Option<IncomingRequest> {
        self.state.borrow().pending_request.clone()
    }
    pub fn viewer(&self) -> Option<IncomingRequest> {
        self.state.borrow().viewer.clone()
    }
    pub fn allows_control(&self) -> bool {
        self.state.borrow().allows_control
    }
    pub fn is_approving(&self) -> bool {
        self.state.borrow().approving
    }
    pub fn is_ending_viewer(&self) -> bool {
        self.state.borrow().ending_viewer
    }
    pub fn pending_side_switch(&self) -> Option<SideSwitchApproval> {
        self.state.borrow().pending_side_switch.clone()
    }
    /// Registration stopped until the person solves UltraViewer's captcha.
    #[cfg_attr(not(feature = "captcha"), allow(dead_code))]
    pub fn captcha(&self) -> Option<CaptchaChallenge> {
        self.state.borrow().captcha.clone()
    }

    #[cfg_attr(not(feature = "captcha"), allow(dead_code))]
    pub fn solve_captcha(self: &Rc<Self>, token: String) {
        crate::profile::set_captcha_token(token);
        self.state.borrow_mut().captcha = None;
        self.start();
    }

    /// The captcha window was closed; "Try sharing again" asks for a new one.
    #[cfg_attr(not(feature = "captcha"), allow(dead_code))]
    pub fn dismiss_captcha(&self) {
        if self.state.borrow_mut().captcha.take().is_some() {
            self.notify();
        }
    }

    /// The sharing registration, for outgoing sessions to reuse.
    pub fn command_port(&self) -> Option<ultraviewer_protocol::host::CommandPort> {
        let state = self.state.borrow();
        state.identifier.as_ref()?;
        state.handle.as_ref().map(|h| h.command_port())
    }

    pub fn start(self: &Rc<Self>) {
        if self.is_running() {
            return;
        }
        let generation = {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.phase = HostPhase::Registering;
            state.error = None;
            state.captcha = None;
            state.status = "Getting this computer's UltraViewer ID…".into();
            state.generation
        };
        self.notify();
        let profile = match crate::profile::load_or_create() {
            Ok(profile) => profile,
            Err(error) => return self.failed(tr(&error.to_string()), generation),
        };
        self.state.borrow_mut().password = profile.password.clone();
        let streams = {
            let _guard = runtime::runtime().enter();
            host::start(HostConfiguration::new(profile))
        };
        let streams = match streams {
            Ok(streams) => streams,
            Err(error) => return self.failed(host_error_text(&error), generation),
        };
        self.state.borrow_mut().handle = Some(streams.handle.clone());
        let model = Rc::downgrade(self);
        let mut events = streams.events;
        glib::spawn_future_local(async move {
            while let Some(event) = events.recv().await {
                let Some(model) = model.upgrade() else { return };
                if model.state.borrow().generation != generation {
                    return;
                }
                let stopped = matches!(event, HostEvent::Stopped(_));
                model.receive(event, generation);
                if stopped {
                    return;
                }
            }
        });
    }

    fn receive(self: &Rc<Self>, event: HostEvent, generation: u64) {
        match event {
            HostEvent::Status(message) => {
                self.state.borrow_mut().status = message;
                self.notify();
            }
            HostEvent::Registered(id) => {
                let mut state = self.state.borrow_mut();
                state.identifier = Some(id);
                state.phase = HostPhase::Ready;
                state.status = "Waiting for another computer to connect.".into();
                drop(state);
                self.notify();
            }
            HostEvent::Request(request) => {
                {
                    let mut state = self.state.borrow_mut();
                    state.pending_side_switch = None;
                    state.pending_request = Some(request);
                    state.phase = HostPhase::AwaitingApproval;
                    state.status = "An incoming connection needs your approval.".into();
                }
                self.notify();
            }
            HostEvent::Connected(request) => {
                {
                    let mut state = self.state.borrow_mut();
                    state.viewer = Some(request.clone());
                    state.pending_request = None;
                    state.approving = false;
                    state.ending_viewer = false;
                    state.phase = HostPhase::Sharing;
                    state.status = if state.allows_control { "Your computer is being viewed and controlled.".into() } else { "Your computer is being viewed.".into() };
                }
                self.activate_tools(request.id, generation);
                self.notify();
            }
            HostEvent::CaptureRequested(ticket) => self.capture_next(ticket, generation),
            HostEvent::Input { session, payload } => {
                let state = self.state.borrow();
                if state.allows_control && state.viewer.as_ref().is_some_and(|v| v.id == session) {
                    if let Some(injector) = &state.injector {
                        injector.send(InjectorMessage::Payload(payload));
                    }
                }
            }
            HostEvent::ReleaseInput => {
                if let Some(injector) = &self.state.borrow().injector {
                    injector.send(InjectorMessage::ReleaseAll);
                }
            }
            HostEvent::Chat { session, text } => {
                if self.viewer().is_some_and(|v| v.id == session) {
                    self.tools.receive_chat(text);
                }
            }
            HostEvent::Typing { session, active } => {
                if self.viewer().is_some_and(|v| v.id == session) {
                    self.tools.receive_typing(active);
                }
            }
            HostEvent::Clipboard { session, text } => {
                if self.viewer().is_some_and(|v| v.id == session) {
                    self.tools.receive_clipboard(text);
                }
            }
            HostEvent::SideSwitchRequested { session, request } => {
                let accept = {
                    let state = self.state.borrow();
                    state.phase == HostPhase::Sharing
                        && !state.ending_viewer
                        && !state.approving_side_switch
                        && state.viewer.as_ref().is_some_and(|v| v.id == session && v.viewer_id == request.claimed_id)
                };
                if accept {
                    self.state.borrow_mut().pending_side_switch = Some(request);
                    self.notify();
                }
            }
            HostEvent::ViewerEnded(reason) => {
                self.tools.deactivate();
                {
                    let mut state = self.state.borrow_mut();
                    state.pending_side_switch = None;
                    state.ending_viewer = false;
                    state.viewer = None;
                    state.pending_request = None;
                    state.allows_control = false;
                    state.approving = false;
                    state.injector = None;
                    state.phase = HostPhase::Ready;
                    state.status = reason.unwrap_or_else(|| "The other computer disconnected.".into());
                }
                self.close_source();
                self.notify();
            }
            HostEvent::Stopped(error) => match error {
                Some(HostError::CaptchaRequired(challenge)) => {
                    self.state.borrow_mut().captcha = Some(challenge);
                    self.failed(crate::profile::captcha_message("Solve UltraViewer's captcha to get this computer's ID."), generation);
                }
                Some(error) => self.failed(host_error_text(&error), generation),
                None => self.stop(),
            },
        }
    }

    fn failed(self: &Rc<Self>, message: String, generation: u64) {
        if self.state.borrow().generation != generation {
            return;
        }
        self.tools.deactivate();
        let handle = {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.phase = HostPhase::Failed;
            state.identifier = None;
            state.pending_request = None;
            state.viewer = None;
            state.allows_control = false;
            state.approving = false;
            state.pending_side_switch = None;
            state.injector = None;
            state.error = Some(message);
            state.status = "This computer is unavailable for incoming connections.".into();
            state.handle.take()
        };
        self.close_source();
        if let Some(handle) = handle {
            runtime::spawn(async move { handle.stop().await });
        }
        self.notify();
    }

    pub fn stop(self: &Rc<Self>) {
        self.tools.deactivate();
        let handle = {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.phase = HostPhase::Off;
            state.identifier = None;
            state.password.clear();
            state.pending_request = None;
            state.viewer = None;
            state.allows_control = false;
            state.approving = false;
            state.ending_viewer = false;
            state.pending_side_switch = None;
            state.approving_side_switch = false;
            state.injector = None;
            state.status = "Incoming access is paused.".into();
            state.handle.take()
        };
        self.close_source();
        if let Some(handle) = handle {
            runtime::spawn(async move { handle.stop().await });
        }
        self.notify();
    }

    /// Stops sharing and waits until a connected viewer has been told.
    pub async fn shutdown(self: &Rc<Self>) {
        let handle = self.state.borrow().handle.clone();
        if let Some(handle) = handle {
            runtime::run(async move { handle.stop().await }).await;
        }
        self.stop();
    }

    fn close_source(&self) {
        let source = self.state.borrow_mut().source.take();
        self.state.borrow_mut().frame_size = None;
        if let Some(Source::Portal(portal)) = source {
            runtime::spawn(async move { portal.close().await });
        }
    }

    /// Replaces the random password and re-registers if sharing is on.
    pub fn reset_password(self: &Rc<Self>) {
        if self.viewer().is_some() || self.pending_request().is_some() {
            return;
        }
        let running = self.is_running();
        match crate::profile::reset_password() {
            Ok(profile) => {
                self.state.borrow_mut().password = profile.password;
                if running {
                    self.stop();
                    self.start();
                } else {
                    self.notify();
                }
            }
            Err(error) => {
                self.state.borrow_mut().error = Some(tr(&error.to_string()));
                self.notify();
            }
        }
    }

    // ------------------------------------------------------------ approval

    pub fn approve(self: &Rc<Self>, control: bool) {
        let (request, handle, generation) = {
            let mut state = self.state.borrow_mut();
            let (Some(request), Some(handle)) = (state.pending_request.clone(), state.handle.clone()) else { return };
            if state.approving {
                return;
            }
            state.approving = true;
            state.error = None;
            (request, handle, state.generation)
        };
        self.notify();
        let model = self.clone();
        glib::spawn_future_local(async move {
            let result = async {
                model.ensure_source(control).await?;
                let frame = model.capture_frame().await?;
                if control {
                    model.ensure_injector()?;
                }
                let still_pending = model.state.borrow().generation == generation
                    && model.state.borrow().pending_request.as_ref().is_some_and(|r| r.id == request.id);
                if !still_pending {
                    return Ok(());
                }
                model.state.borrow_mut().allows_control = control;
                handle.approve(request.id, frame, control).await.map_err(|e| host_error_text(&e))
            }
            .await;
            if let Err(message) = result {
                if model.state.borrow().generation != generation {
                    return;
                }
                {
                    let mut state = model.state.borrow_mut();
                    state.approving = false;
                    state.allows_control = false;
                    state.injector = None;
                    state.error = Some(message);
                }
                model.notify();
            }
        });
    }

    pub fn decline(self: &Rc<Self>) {
        let (request, handle) = {
            let mut state = self.state.borrow_mut();
            let request = state.pending_request.take();
            state.approving = false;
            state.allows_control = false;
            (request, state.handle.clone())
        };
        if let (Some(request), Some(handle)) = (request, handle) {
            handle.decline(request.id);
        }
        self.notify();
    }

    pub fn end_viewer(self: &Rc<Self>) {
        self.tools.deactivate();
        let (handle, viewer) = {
            let mut state = self.state.borrow_mut();
            if state.ending_viewer {
                return;
            }
            state.pending_side_switch = None;
            state.ending_viewer = true;
            state.allows_control = false;
            state.injector = None;
            state.status = "Ending the incoming session…".into();
            (state.handle.clone(), state.viewer.clone())
        };
        match (handle, viewer) {
            (Some(handle), Some(viewer)) => handle.end_viewer(Some(viewer.id), None),
            _ => self.state.borrow_mut().ending_viewer = false,
        }
        self.notify();
    }

    /// A local control change affects only the current viewer. Revocation
    /// releases input immediately, before notifying the viewer.
    pub fn set_control_allowed(self: &Rc<Self>, allowed: bool) {
        let (handle, viewer, generation) = {
            let state = self.state.borrow();
            let (Some(handle), Some(viewer)) = (state.handle.clone(), state.viewer.clone()) else { return };
            if state.phase != HostPhase::Sharing || state.ending_viewer || state.allows_control == allowed {
                return;
            }
            (handle, viewer, state.generation)
        };
        if allowed {
            if let Err(message) = self.ensure_injector() {
                self.state.borrow_mut().error = Some(message);
                self.notify();
                return;
            }
        } else {
            let mut state = self.state.borrow_mut();
            state.allows_control = false;
            state.injector = None;
            state.status = "Your computer is being viewed.".into();
        }
        self.notify();
        let model = self.clone();
        glib::spawn_future_local(async move {
            let result = handle.set_control_allowed(allowed, viewer.id).await;
            if model.state.borrow().generation != generation {
                return;
            }
            {
                let mut state = model.state.borrow_mut();
                match result {
                    Ok(()) => state.allows_control = allowed,
                    Err(error) => {
                        state.allows_control = false;
                        state.injector = None;
                        state.error = Some(trf("Remote control could not be updated: {}", &[&host_error_text(&error)]));
                    }
                }
                state.status = if state.allows_control { "Your computer is being viewed and controlled.".into() } else { "Your computer is being viewed.".into() };
            }
            model.notify();
        });
    }

    pub fn approve_side_switch(self: &Rc<Self>) {
        let (request, handle, generation) = {
            let mut state = self.state.borrow_mut();
            let (Some(request), Some(handle)) = (state.pending_side_switch.take(), state.handle.clone()) else { return };
            if state.approving_side_switch {
                return;
            }
            state.approving_side_switch = true;
            (request, handle, state.generation)
        };
        self.notify();
        let model = self.clone();
        glib::spawn_future_local(async move {
            let result = handle.approve_side_switch(request.id, request.session).await;
            if model.state.borrow().generation != generation {
                return;
            }
            model.state.borrow_mut().approving_side_switch = false;
            match result {
                Ok(credentials) => {
                    model.state.borrow_mut().allows_control = false;
                    model.state.borrow_mut().injector = None;
                    let callback = model.on_side_switch.borrow().clone();
                    if let Some(callback) = callback {
                        callback(credentials.claimed_id, credentials.password);
                    }
                    if model.viewer().is_some_and(|v| v.id == request.session) {
                        model.end_viewer();
                    }
                }
                Err(_) => {
                    model.state.borrow_mut().error = Some(tr("Could not switch control sides. Ask the other computer to try again."));
                }
            }
            model.notify();
        });
    }

    pub fn decline_side_switch(self: &Rc<Self>) {
        let (request, handle) = {
            let mut state = self.state.borrow_mut();
            if state.approving_side_switch {
                return;
            }
            (state.pending_side_switch.take(), state.handle.clone())
        };
        if let (Some(request), Some(handle)) = (request, handle) {
            handle.decline_side_switch(request.id, request.session);
        }
        self.notify();
    }

    // ------------------------------------------------------------ capture

    async fn ensure_source(self: &Rc<Self>, want_input: bool) -> Result<(), String> {
        let needs_input_portal = match &self.state.borrow().source {
            Some(Source::Portal(portal)) => want_input && !portal.has_input() && input_support().is_none(),
            Some(Source::X11(_)) => false,
            None => true,
        };
        if !needs_input_portal {
            return Ok(());
        }
        self.close_source();
        // X11 sessions without a portal backend capture the root window.
        if std::env::var("XDG_SESSION_TYPE").ok().as_deref() == Some("x11") {
            if let Ok(capture) = X11Capture::open() {
                if runtime::run(async { ashpd::desktop::screencast::Screencast::new().await.is_err() }).await {
                    self.state.borrow_mut().source = Some(Source::X11(Arc::new(Mutex::new(capture))));
                    return Ok(());
                }
            }
        }
        // Prefer a portal input grant only when no local backend exists.
        let portal_input = want_input && input_support().is_none();
        let token = settings::get().portal_restore_token;
        let (portal, new_token) = runtime::run(capture::start_portal(portal_input, token)).await?;
        settings::update(|s| s.portal_restore_token = new_token);
        self.state.borrow_mut().source = Some(Source::Portal(Arc::new(portal)));
        Ok(())
    }

    async fn latest_raw(&self) -> Result<RawFrame, String> {
        let source = match &self.state.borrow().source {
            Some(Source::Portal(portal)) => Source::Portal(portal.clone()),
            Some(Source::X11(capture)) => Source::X11(capture.clone()),
            None => return Err(tr("The screen is no longer available.")),
        };
        match source {
            Source::X11(capture) => runtime::run(async move {
                tokio::task::spawn_blocking(move || capture.lock().unwrap_or_else(|e| e.into_inner()).capture()).await.map_err(|e| e.to_string())?
            })
            .await
            .map_err(|e| trf("Screen sharing is unavailable: {}", &[&e])),
            Source::Portal(portal) => {
                for _ in 0..100 {
                    if let Some(error) = portal.state.failed.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                        return Err(trf("Screen sharing is unavailable: {}", &[&error]));
                    }
                    if let Some(frame) = portal.state.latest.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                        return Ok(frame);
                    }
                    glib::timeout_future(Duration::from_millis(50)).await;
                }
                Err(tr("The screen is no longer available."))
            }
        }
    }

    async fn capture_frame(self: &Rc<Self>) -> Result<HostedScreenFrame, String> {
        let raw = self.latest_raw().await?;
        let frame = runtime::run(async move { tokio::task::spawn_blocking(move || capture::convert(&raw)).await.ok().flatten() })
            .await
            .ok_or_else(|| tr("The screen is no longer available."))?;
        let size = (frame.width, frame.height);
        if self.state.borrow().frame_size != Some(size) {
            self.state.borrow_mut().frame_size = Some(size);
            if let Some(injector) = &self.state.borrow().injector {
                injector.send(InjectorMessage::Frame(size.0, size.1));
            }
        }
        Ok(frame)
    }

    fn capture_next(self: &Rc<Self>, ticket: u64, generation: u64) {
        let Some(handle) = self.state.borrow().handle.clone() else { return };
        let viewer = self.viewer();
        let model = self.clone();
        glib::spawn_future_local(async move {
            let result = model.capture_frame().await;
            if model.state.borrow().generation != generation || model.viewer().map(|v| v.id) != viewer.as_ref().map(|v| v.id) {
                return;
            }
            match result {
                Ok(frame) => handle.submit_frame(frame, ticket),
                Err(message) => {
                    {
                        let mut state = model.state.borrow_mut();
                        state.allows_control = false;
                        state.injector = None;
                        state.error = Some(message.clone());
                    }
                    handle.end_viewer(viewer.map(|v| v.id), Some(tr("The screen is no longer available.")));
                    model.notify();
                }
            }
        });
    }

    // ------------------------------------------------------------ input

    fn ensure_injector(self: &Rc<Self>) -> Result<(), String> {
        if self.state.borrow().injector.is_some() {
            return Ok(());
        }
        let frame = self.state.borrow().frame_size.map(|(w, h)| (w as f64, h as f64)).unwrap_or((1920.0, 1080.0));
        let backend: Box<dyn InjectBackend> = {
            let state = self.state.borrow();
            match &state.source {
                Some(Source::Portal(portal)) if portal.has_input() => {
                    let logical = portal.size.map(|(w, h)| (f64::from(w), f64::from(h))).unwrap_or(frame);
                    Box::new(PortalBackend { portal: portal.clone(), logical, frame, runtime: runtime::runtime().handle().clone() })
                }
                Some(Source::X11(_)) => Box::new(XTestBackend::new(frame).map_err(|e| trf("Keyboard and mouse control is unavailable: {}", &[&e]))?),
                Some(Source::Portal(portal)) => {
                    if std::env::var("XDG_SESSION_TYPE").ok().as_deref() == Some("x11") {
                        Box::new(XTestBackend::new(frame).map_err(|e| trf("Keyboard and mouse control is unavailable: {}", &[&e]))?)
                    } else {
                        if !UinputBackend::available() {
                            return Err(tr("Remote control needs write access to /dev/uinput (add your user to the input group) or a desktop with remote-desktop portal support."));
                        }
                        let (layout, monitors) = desktop_layout();
                        let target = match (portal.position, portal.size) {
                            (Some((x, y)), Some((w, h))) => (f64::from(x) - layout.0, f64::from(y) - layout.1, f64::from(w), f64::from(h)),
                            _ => monitors.first().map(|m| (m.0 - layout.0, m.1 - layout.1, m.2, m.3)).unwrap_or(layout),
                        };
                        Box::new(UinputBackend::new(target, (layout.2, layout.3), frame).map_err(|e| trf("Keyboard and mouse control is unavailable: {}", &[&e]))?)
                    }
                }
                None => return Err(tr("The screen is no longer available.")),
            }
        };
        let (failures, mut failure_events) = tokio::sync::mpsc::unbounded_channel::<String>();
        let injector = Injector::start(backend, failures);
        self.state.borrow_mut().injector = Some(injector);
        let model = Rc::downgrade(self);
        let generation = self.state.borrow().generation;
        glib::spawn_future_local(async move {
            if let Some(message) = failure_events.recv().await {
                let Some(model) = model.upgrade() else { return };
                if model.state.borrow().generation != generation {
                    return;
                }
                let (handle, viewer) = {
                    let mut state = model.state.borrow_mut();
                    state.injector = None;
                    state.allows_control = false;
                    state.error = Some(trf("Remote control paused: {}", &[&message]));
                    (state.handle.clone(), state.viewer.clone())
                };
                if let (Some(handle), Some(viewer)) = (handle, viewer) {
                    let _ = handle.set_control_allowed(false, viewer.id).await;
                }
                model.notify();
            }
        });
        Ok(())
    }

    fn activate_tools(self: &Rc<Self>, viewer: u64, generation: u64) {
        let Some(handle) = self.state.borrow().handle.clone() else { return };
        let model = Rc::downgrade(self);
        let alive = move || model.upgrade().is_some_and(|m| m.state.borrow().generation == generation && m.viewer().is_some_and(|v| v.id == viewer));
        let (h1, h2, h3, h4) = (handle.clone(), handle.clone(), handle.clone(), handle);
        let (a1, a2, a3, a4) = (alive.clone(), alive.clone(), alive.clone(), alive);
        self.tools.activate(ToolsBackend {
            send_chat: Rc::new(move |text| {
                let (handle, alive) = (h1.clone(), a1());
                Box::pin(async move {
                    if !alive {
                        return Err(String::new());
                    }
                    handle.send_chat(text, viewer).await.map_err(|e| e.to_string())
                })
            }),
            send_clipboard: Rc::new(move |text| {
                let (handle, alive) = (h2.clone(), a2());
                Box::pin(async move {
                    if !alive {
                        return Err(String::new());
                    }
                    handle.send_clipboard(text, viewer).await.map_err(|e| e.to_string())
                })
            }),
            set_sharing: Rc::new(move |enabled| {
                let (handle, alive) = (h3.clone(), a3());
                Box::pin(async move {
                    if !alive {
                        return Err(String::new());
                    }
                    handle.set_clipboard_sharing(enabled, viewer).await.map_err(|e| e.to_string())
                })
            }),
            send_typing: Rc::new(move |typing| {
                let (handle, alive) = (h4.clone(), a4());
                Box::pin(async move {
                    if alive {
                        handle.send_typing(typing, viewer);
                    }
                    Ok(())
                })
            }),
        });
    }
}
