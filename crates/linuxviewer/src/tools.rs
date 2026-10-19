//! Per-session text tools: chat and opt-in clipboard sharing. Nothing is
//! persisted. Existing clipboard contents are not sent when sharing starts.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::i18n::tr;

pub type LocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;
pub type TextSender = Rc<dyn Fn(String) -> LocalFuture<Result<(), String>>>;
pub type FlagSender = Rc<dyn Fn(bool) -> LocalFuture<Result<(), String>>>;

#[derive(Clone)]
pub struct ToolsBackend {
    pub send_chat: TextSender,
    pub send_clipboard: TextSender,
    pub set_sharing: FlagSender,
    pub send_typing: FlagSender,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub text: String,
    pub outgoing: bool,
    pub time: glib::DateTime,
}

#[derive(Debug, Clone)]
pub struct ClipboardEntry {
    pub text: String,
    pub outgoing: bool,
}

#[derive(Default)]
struct State {
    active: bool,
    generation: u64,
    clipboard_generation: u64,
    backend: Option<ToolsBackend>,
    messages: Vec<ChatMessage>,
    sending: bool,
    peer_typing: bool,
    peer_typing_timer: Option<glib::SourceId>,
    typing_timer: Option<glib::SourceId>,
    clipboard_enabled: bool,
    updating_clipboard: bool,
    clipboard_status: String,
    history: Vec<ClipboardEntry>,
    notice: Option<String>,
    notice_timer: Option<glib::SourceId>,
    error: Option<String>,
    latest_clipboard_text: Option<String>,
    clipboard_handler: Option<(gdk::Clipboard, glib::SignalHandlerId)>,
    unread: usize,
}

/// Observable tools state; listeners refresh their widgets on change.
pub struct SessionTools {
    state: RefCell<State>,
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
    pub share_by_default: Rc<dyn Fn() -> bool>,
}

impl SessionTools {
    pub fn new(share_by_default: Rc<dyn Fn() -> bool>) -> Rc<Self> {
        let tools = Rc::new(Self { state: RefCell::default(), listeners: RefCell::default(), share_by_default });
        tools.state.borrow_mut().clipboard_status = tr("Clipboard sharing is off.");
        tools
    }

    pub fn connect_changed(&self, listener: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Rc::new(listener));
    }

    fn changed(&self) {
        let listeners: Vec<Rc<dyn Fn()>> = self.listeners.borrow().clone();
        for listener in listeners {
            listener();
        }
    }

    pub fn is_active(&self) -> bool {
        self.state.borrow().active
    }
    pub fn messages(&self) -> Vec<ChatMessage> {
        self.state.borrow().messages.clone()
    }
    pub fn is_sending(&self) -> bool {
        self.state.borrow().sending
    }
    pub fn peer_is_typing(&self) -> bool {
        self.state.borrow().peer_typing
    }
    pub fn clipboard_enabled(&self) -> bool {
        self.state.borrow().clipboard_enabled
    }
    pub fn updating_clipboard(&self) -> bool {
        self.state.borrow().updating_clipboard
    }
    pub fn clipboard_status(&self) -> String {
        self.state.borrow().clipboard_status.clone()
    }
    pub fn clipboard_history(&self) -> Vec<ClipboardEntry> {
        self.state.borrow().history.clone()
    }
    pub fn notice(&self) -> Option<String> {
        self.state.borrow().notice.clone()
    }
    pub fn error(&self) -> Option<String> {
        self.state.borrow().error.clone()
    }
    pub fn unread(&self) -> usize {
        self.state.borrow().unread
    }
    pub fn mark_read(&self) {
        if std::mem::take(&mut self.state.borrow_mut().unread) > 0 {
            self.changed();
        }
    }

    pub fn activate(self: &Rc<Self>, backend: ToolsBackend) {
        self.deactivate();
        {
            let mut state = self.state.borrow_mut();
            state.backend = Some(backend);
            state.messages.clear();
            state.error = None;
            state.unread = 0;
            state.active = true;
        }
        self.changed();
        if (self.share_by_default)() {
            self.set_clipboard_enabled(true);
        }
    }

    pub fn deactivate(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.clipboard_generation += 1;
            for timer in [state.typing_timer.take(), state.peer_typing_timer.take(), state.notice_timer.take()].into_iter().flatten() {
                timer.remove();
            }
            if let Some((clipboard, handler)) = state.clipboard_handler.take() {
                clipboard.disconnect(handler);
            }
            state.backend = None;
            state.active = false;
            state.sending = false;
            state.peer_typing = false;
            state.clipboard_enabled = false;
            state.updating_clipboard = false;
            state.latest_clipboard_text = None;
            state.clipboard_status = tr("Clipboard sharing is off.");
            state.history.clear();
            state.notice = None;
        }
        self.changed();
    }

    pub fn send_chat(self: &Rc<Self>, text: String) {
        let (backend, generation) = {
            let state = self.state.borrow();
            match (&state.backend, state.active, state.sending) {
                (Some(backend), true, false) => (backend.clone(), state.generation),
                _ => return,
            }
        };
        if text.trim().is_empty() {
            return;
        }
        if text.encode_utf16().count() > 16_384 {
            self.state.borrow_mut().error = Some(tr("Keep chat messages under 16,384 characters."));
            self.changed();
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.sending = true;
            state.error = None;
        }
        self.changed();
        let tools = self.clone();
        glib::spawn_future_local(async move {
            let result = (backend.send_chat)(text.clone()).await;
            let current = {
                let state = tools.state.borrow();
                state.generation == generation && state.active
            };
            if !current {
                return;
            }
            {
                let mut state = tools.state.borrow_mut();
                state.sending = false;
                match result {
                    Ok(()) => append(&mut state.messages, text, true),
                    Err(_) => state.error = Some(tr("The chat message could not be sent.")),
                }
            }
            tools.changed();
        });
    }

    pub fn receive_chat(self: &Rc<Self>, text: String) {
        if !self.is_active() || text.encode_utf16().count() > 16_384 {
            return;
        }
        self.receive_typing(false);
        {
            let mut state = self.state.borrow_mut();
            append(&mut state.messages, text, false);
            state.unread += 1;
        }
        self.changed();
    }

    pub fn receive_typing(self: &Rc<Self>, typing: bool) {
        let generation = {
            let mut state = self.state.borrow_mut();
            if !state.active {
                return;
            }
            if let Some(timer) = state.peer_typing_timer.take() {
                timer.remove();
            }
            state.peer_typing = typing;
            state.generation
        };
        if typing {
            let tools = Rc::downgrade(self);
            let timer = glib::timeout_add_local_once(Duration::from_secs(4), move || {
                if let Some(tools) = tools.upgrade() {
                    let mut state = tools.state.borrow_mut();
                    state.peer_typing_timer = None;
                    if state.generation == generation {
                        state.peer_typing = false;
                    }
                    drop(state);
                    tools.changed();
                }
            });
            self.state.borrow_mut().peer_typing_timer = Some(timer);
        }
        self.changed();
    }

    /// Debounced typing indicator for the local draft.
    pub fn draft_changed(self: &Rc<Self>, draft_empty: bool) {
        let (backend, generation) = {
            let mut state = self.state.borrow_mut();
            if let Some(timer) = state.typing_timer.take() {
                timer.remove();
            }
            match (&state.backend, state.active) {
                (Some(backend), true) => (backend.clone(), state.generation),
                _ => return,
            }
        };
        let typing = !draft_empty;
        let tools = Rc::downgrade(self);
        let timer = glib::timeout_add_local_once(Duration::from_millis(250), move || {
            let Some(tools) = tools.upgrade() else { return };
            tools.state.borrow_mut().typing_timer = None;
            if tools.state.borrow().generation != generation {
                return;
            }
            let send = backend.send_typing.clone();
            glib::spawn_future_local(async move {
                let _ = send(typing).await;
                if typing {
                    glib::timeout_future(Duration::from_secs(2)).await;
                    if tools.state.borrow().generation == generation {
                        let _ = send(false).await;
                    }
                }
            });
        });
        self.state.borrow_mut().typing_timer = Some(timer);
    }

    pub fn set_clipboard_enabled(self: &Rc<Self>, enabled: bool) {
        let (backend, generation, revision) = {
            let mut state = self.state.borrow_mut();
            let Some(backend) = state.backend.clone() else { return };
            if !state.active {
                return;
            }
            state.clipboard_generation += 1;
            if let Some((clipboard, handler)) = state.clipboard_handler.take() {
                clipboard.disconnect(handler);
            }
            state.clipboard_enabled = false;
            state.latest_clipboard_text = None;
            state.updating_clipboard = true;
            state.clipboard_status = if enabled { tr("Enabling clipboard sharing…") } else { tr("Clipboard sharing is off.") };
            (backend, state.generation, state.clipboard_generation)
        };
        self.changed();
        let tools = self.clone();
        glib::spawn_future_local(async move {
            let result = (backend.set_sharing)(enabled).await;
            let current = {
                let state = tools.state.borrow();
                state.generation == generation && state.clipboard_generation == revision && state.active
            };
            if !current {
                return;
            }
            match result {
                Ok(()) => {
                    {
                        let mut state = tools.state.borrow_mut();
                        state.clipboard_enabled = enabled;
                        state.updating_clipboard = false;
                        if enabled {
                            state.clipboard_status = tr("Newly copied text is shared in both directions.");
                        }
                    }
                    if enabled {
                        tools.watch_clipboard(generation, revision);
                    }
                }
                Err(_) => {
                    let mut state = tools.state.borrow_mut();
                    state.updating_clipboard = false;
                    state.clipboard_enabled = false;
                    state.clipboard_status = tr("Clipboard sharing is off.");
                    state.error = Some(tr("Clipboard sharing could not be enabled."));
                }
            }
            tools.changed();
        });
    }

    fn watch_clipboard(self: &Rc<Self>, generation: u64, revision: u64) {
        let Some(display) = gdk::Display::default() else { return };
        let clipboard = display.clipboard();
        let tools = Rc::downgrade(self);
        let handler = clipboard.connect_changed(move |clipboard| {
            let Some(tools) = tools.upgrade() else { return };
            // Our own writes are local; only newly copied text is shared.
            if clipboard.is_local() {
                return;
            }
            let current = {
                let state = tools.state.borrow();
                state.generation == generation && state.clipboard_generation == revision && state.clipboard_enabled
            };
            if !current {
                return;
            }
            let tools = tools.clone();
            let clipboard = clipboard.clone();
            glib::spawn_future_local(async move {
                if let Ok(Some(text)) = clipboard.read_text_future().await {
                    tools.send_clipboard_text(text.to_string(), generation, revision).await;
                }
            });
        });
        self.state.borrow_mut().clipboard_handler = Some((clipboard, handler));
    }

    async fn send_clipboard_text(self: &Rc<Self>, text: String, generation: u64, revision: u64) {
        let backend = {
            let mut state = self.state.borrow_mut();
            if state.latest_clipboard_text.as_deref() == Some(text.as_str()) {
                return;
            }
            if text.encode_utf16().count() > 524_288 {
                state.error = Some(tr("Clipboard text exceeds the 1 MB sharing limit."));
                drop(state);
                self.changed();
                return;
            }
            state.latest_clipboard_text = Some(text.clone());
            match state.backend.clone() {
                Some(backend) => backend,
                None => return,
            }
        };
        let result = (backend.send_clipboard)(text.clone()).await;
        let current = {
            let state = self.state.borrow();
            state.generation == generation && state.clipboard_generation == revision
        };
        if !current {
            return;
        }
        match result {
            Ok(()) => self.record_clipboard(text, true),
            Err(_) => {
                self.state.borrow_mut().error = Some(tr("Clipboard text could not be sent."));
                self.changed();
            }
        }
    }

    pub fn receive_clipboard(self: &Rc<Self>, text: String) {
        {
            let mut state = self.state.borrow_mut();
            if !state.active || !state.clipboard_enabled || text.encode_utf16().count() > 524_288 {
                return;
            }
            state.latest_clipboard_text = Some(text.clone());
        }
        if let Some(display) = gdk::Display::default() {
            display.clipboard().set_text(&text);
        }
        self.record_clipboard(text, false);
    }

    /// Puts an earlier shared item back on this computer's clipboard.
    pub fn copy_to_local(&self, entry: &ClipboardEntry) {
        self.state.borrow_mut().latest_clipboard_text = Some(entry.text.clone());
        if let Some(display) = gdk::Display::default() {
            display.clipboard().set_text(&entry.text);
        }
    }

    fn record_clipboard(self: &Rc<Self>, text: String, outgoing: bool) {
        {
            let mut state = self.state.borrow_mut();
            state.history.insert(0, ClipboardEntry { text, outgoing });
            state.history.truncate(20);
            state.notice = Some(if outgoing { tr("Sent to the other computer") } else { tr("Copied from the other computer") });
            if let Some(timer) = state.notice_timer.take() {
                timer.remove();
            }
        }
        let tools = Rc::downgrade(self);
        let timer = glib::timeout_add_local_once(Duration::from_secs(3), move || {
            if let Some(tools) = tools.upgrade() {
                {
                    let mut state = tools.state.borrow_mut();
                    state.notice_timer = None;
                    state.notice = None;
                }
                tools.changed();
            }
        });
        self.state.borrow_mut().notice_timer = Some(timer);
        self.changed();
    }
}

fn append(messages: &mut Vec<ChatMessage>, text: String, outgoing: bool) {
    let time = glib::DateTime::now_local().unwrap_or_else(|_| glib::DateTime::now_utc().expect("time"));
    messages.push(ChatMessage { text, outgoing, time });
    // Bound both transcript item count and text storage.
    while messages.len() > 200 || messages.iter().map(|m| m.text.len()).sum::<usize>() > 1_048_576 {
        messages.remove(0);
    }
}
