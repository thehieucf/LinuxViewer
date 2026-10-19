//! Chat sidebar and clipboard popover, shared by outgoing and incoming sessions.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::i18n::{tr, trf};
use crate::tools::SessionTools;

pub struct ChatPanel {
    pub root: gtk::Box,
    title: gtk::Label,
    list: gtk::Box,
    scroller: gtk::ScrolledWindow,
    typing: gtk::Label,
    error: gtk::Label,
    entry: gtk::Entry,
    send: gtk::Button,
    empty: gtk::Label,
    peer_name: std::cell::RefCell<String>,
    rendered: std::cell::Cell<usize>,
}

impl ChatPanel {
    pub fn new(tools: &Rc<SessionTools>, on_close: impl Fn() + 'static) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("chat-panel");
        root.set_width_request(300);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_start(12);
        header.set_margin_end(6);
        header.set_margin_top(8);
        header.set_margin_bottom(8);
        let title = gtk::Label::new(Some(&tr("Chat")));
        title.add_css_class("heading");
        title.set_hexpand(true);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.set_tooltip_text(Some(&tr("Hide chat")));
        close.connect_clicked(move |_| on_close());
        header.append(&title);
        header.append(&close);
        root.append(&header);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        list.set_margin_start(12);
        list.set_margin_end(12);
        list.set_margin_top(12);
        list.set_margin_bottom(12);
        list.set_valign(gtk::Align::End);
        let empty = gtk::Label::new(Some(&tr("No messages yet.")));
        empty.add_css_class("dim-label");
        empty.set_vexpand(true);
        empty.set_valign(gtk::Align::Center);
        list.append(&empty);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        root.append(&scroller);

        let typing = gtk::Label::new(None);
        typing.add_css_class("dim-label");
        typing.add_css_class("caption");
        typing.set_xalign(0.0);
        typing.set_margin_start(12);
        typing.set_visible(false);
        root.append(&typing);
        let error = gtk::Label::new(None);
        error.add_css_class("error");
        error.add_css_class("caption");
        error.set_xalign(0.0);
        error.set_wrap(true);
        error.set_margin_start(12);
        error.set_margin_end(12);
        error.set_visible(false);
        root.append(&error);

        let composer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        composer.set_margin_start(12);
        composer.set_margin_end(12);
        composer.set_margin_top(6);
        composer.set_margin_bottom(12);
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some(&tr("Message")));
        entry.set_hexpand(true);
        let send = gtk::Button::from_icon_name("mail-send-symbolic");
        send.add_css_class("suggested-action");
        send.set_tooltip_text(Some(&tr("Send")));
        composer.append(&entry);
        composer.append(&send);
        root.append(&composer);

        let panel = Rc::new(Self {
            root,
            title,
            list,
            scroller,
            typing,
            error,
            entry,
            send,
            empty,
            peer_name: std::cell::RefCell::new(String::new()),
            rendered: std::cell::Cell::new(0),
        });

        let submit = {
            let tools = tools.clone();
            let entry = panel.entry.clone();
            move || {
                let text = entry.text().to_string();
                if text.trim().is_empty() {
                    return;
                }
                tools.send_chat(text);
                entry.set_text("");
            }
        };
        let submit = Rc::new(submit);
        let s = submit.clone();
        panel.entry.connect_activate(move |_| s());
        panel.send.connect_clicked(move |_| submit());
        let t = tools.clone();
        panel.entry.connect_changed(move |entry| t.draft_changed(entry.text().is_empty()));

        let weak = Rc::downgrade(&panel);
        let t = tools.clone();
        tools.connect_changed(move || {
            if let Some(panel) = weak.upgrade() {
                panel.refresh(&t);
            }
        });
        panel.refresh(tools);
        panel
    }

    pub fn set_peer_name(&self, name: &str) {
        *self.peer_name.borrow_mut() = name.to_string();
        self.title.set_text(&if name.is_empty() { tr("Chat") } else { trf("Chat with {}", &[name]) });
    }

    pub fn focus_entry(&self) {
        self.entry.grab_focus();
    }

    fn refresh(&self, tools: &Rc<SessionTools>) {
        let messages = tools.messages();
        if messages.len() < self.rendered.get() {
            while let Some(child) = self.list.first_child() {
                self.list.remove(&child);
            }
            self.list.append(&self.empty);
            self.rendered.set(0);
        }
        for message in messages.iter().skip(self.rendered.get()) {
            self.list.append(&bubble(&message.text, message.outgoing, &message.time.format("%H:%M").map(|s| s.to_string()).unwrap_or_default()));
        }
        let added = messages.len() > self.rendered.get();
        self.rendered.set(messages.len());
        self.empty.set_visible(messages.is_empty());
        if added {
            let adjustment = self.scroller.vadjustment();
            glib::idle_add_local_once(move || adjustment.set_value(adjustment.upper()));
        }
        let peer = self.peer_name.borrow().clone();
        self.typing.set_visible(tools.peer_is_typing());
        self.typing.set_text(&trf("{} is typing…", &[if peer.is_empty() { "…" } else { &peer }]));
        match tools.error() {
            Some(error) => {
                self.error.set_text(&error);
                self.error.set_visible(true);
            }
            None => self.error.set_visible(false),
        }
        let active = tools.is_active();
        self.entry.set_sensitive(active);
        self.send.set_sensitive(active && !tools.is_sending());
    }
}

fn bubble(text: &str, outgoing: bool, time: &str) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
    column.set_halign(if outgoing { gtk::Align::End } else { gtk::Align::Start });
    let label = gtk::Label::new(Some(text));
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_selectable(true);
    label.set_xalign(0.0);
    label.set_max_width_chars(30);
    label.add_css_class("chat-bubble");
    label.add_css_class(if outgoing { "outgoing" } else { "incoming" });
    let meta = gtk::Label::new(Some(&format!("{} · {}", if outgoing { tr("You") } else { tr("Remote computer") }, time)));
    meta.add_css_class("caption");
    meta.add_css_class("dim-label");
    meta.set_halign(if outgoing { gtk::Align::End } else { gtk::Align::Start });
    column.append(&label);
    column.append(&meta);
    column.upcast()
}

/// Toolbar button with a popover for clipboard sharing and its history.
pub fn clipboard_button(tools: &Rc<SessionTools>) -> gtk::MenuButton {
    let button = gtk::MenuButton::new();
    button.set_icon_name("edit-paste-symbolic");
    button.set_tooltip_text(Some(&tr("Clipboard")));
    button.add_css_class("flat");
    let popover = gtk::Popover::new();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_width_request(320);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let label = gtk::Label::new(Some(&tr("Share clipboard")));
    label.add_css_class("heading");
    label.set_hexpand(true);
    label.set_xalign(0.0);
    let toggle = gtk::Switch::new();
    toggle.set_valign(gtk::Align::Center);
    row.append(&label);
    row.append(&toggle);
    content.append(&row);
    let status = gtk::Label::new(None);
    status.add_css_class("dim-label");
    status.add_css_class("caption");
    status.set_wrap(true);
    status.set_xalign(0.0);
    content.append(&status);
    let history = gtk::ListBox::new();
    history.add_css_class("boxed-list");
    history.set_selection_mode(gtk::SelectionMode::None);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .max_content_height(320)
        .propagate_natural_height(true)
        .child(&history)
        .build();
    content.append(&scroller);
    let empty = gtk::Label::new(Some(&tr("Nothing shared yet. Copy text on either computer.")));
    empty.add_css_class("dim-label");
    empty.set_wrap(true);
    content.append(&empty);
    popover.set_child(Some(&content));
    button.set_popover(Some(&popover));

    let updating = Rc::new(std::cell::Cell::new(false));
    let t = tools.clone();
    let u = updating.clone();
    toggle.connect_active_notify(move |toggle| {
        if !u.get() {
            t.set_clipboard_enabled(toggle.is_active());
        }
    });
    let refresh = {
        let tools = tools.clone();
        let button = button.clone();
        move || {
            updating.set(true);
            toggle.set_active(tools.clipboard_enabled());
            toggle.set_sensitive(tools.is_active() && !tools.updating_clipboard());
            updating.set(false);
            status.set_text(&tools.notice().unwrap_or_else(|| tools.clipboard_status()));
            while let Some(child) = history.first_child() {
                history.remove(&child);
            }
            let entries = tools.clipboard_history();
            empty.set_visible(entries.is_empty());
            scroller.set_visible(!entries.is_empty());
            for entry in entries {
                let row = adw::ActionRow::new();
                let preview: String = entry.text.chars().take(120).collect::<String>().replace('\n', " ");
                row.set_title(&glib::markup_escape_text(&preview));
                row.set_title_lines(2);
                row.set_subtitle(&if entry.outgoing { tr("Sent from this computer. Click to copy again.") } else { tr("Received from the other computer. Click to copy.") });
                row.add_prefix(&gtk::Image::from_icon_name(if entry.outgoing { "go-up-symbolic" } else { "go-down-symbolic" }));
                row.set_activatable(true);
                let tools = tools.clone();
                row.connect_activated(move |_| tools.copy_to_local(&entry));
                history.append(&row);
            }
            if tools.clipboard_enabled() {
                button.add_css_class("accent");
            } else {
                button.remove_css_class("accent");
            }
            button.set_sensitive(tools.is_active());
        }
    };
    refresh();
    tools.connect_changed(refresh);
    button
}
