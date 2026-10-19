//! Main window: home dashboard (connect/share cards, recent computers) and
//! the remote session page.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::chat::{clipboard_button, ChatPanel};
use super::session::SessionPage;
use crate::host::model::{input_support, HostPhase, HostingModel};
use crate::i18n::{tr, trf};
use crate::settings;
use crate::viewer::model::{format_id, normalize_id, Change, Phase, ViewerModel};

pub struct MainWindow {
    pub window: adw::ApplicationWindow,
    viewer: Rc<ViewerModel>,
    hosting: Rc<HostingModel>,
    stack: gtk::Stack,
    toasts: adw::ToastOverlay,
    session: RefCell<Option<Rc<SessionPage>>>,
    // Connect card
    id_entry: gtk::Entry,
    id_display: gtk::Label,
    password_entry: gtk::PasswordEntry,
    password_display: gtk::Label,
    remember: gtk::CheckButton,
    connect_button: gtk::Button,
    open_button: gtk::Button,
    disconnect_button: gtk::Button,
    connect_error: gtk::Label,
    credential_label: gtk::Label,
    connect_status: gtk::Label,
    // Share card
    share_idle: gtk::Box,
    share_active: gtk::Box,
    share_id: gtk::Label,
    share_password: gtk::Label,
    reveal_password: gtk::ToggleButton,
    share_status: gtk::Label,
    share_error: gtk::Label,
    share_button: gtk::Button,
    share_spinner: gtk::Spinner,
    // Header
    host_indicator: gtk::MenuButton,
    host_paused: gtk::Label,
    host_clipboard: gtk::MenuButton,
    host_chat: gtk::ToggleButton,
    // Recent
    recent_group: gtk::Box,
    recent_list: gtk::ListBox,
    updating: Cell<bool>,
    request_dialog: RefCell<Option<adw::AlertDialog>>,
    host_allow: RefCell<Option<gio::SimpleAction>>,
    /// The incoming request already answered, so its dialog is not reopened.
    answered_request: Cell<Option<u64>>,
    side_switch_dialog: RefCell<Option<adw::AlertDialog>>,
    host_chat_split: adw::OverlaySplitView,
    host_chat_panel: Rc<ChatPanel>,
}

fn card(icon: &str, kicker: &str, title: &str, accent: &str) -> (gtk::Box, gtk::Box, gtk::Box) {
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("card");
    frame.add_css_class("home-card");
    frame.set_hexpand(true);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    header.add_css_class("card-header");
    let badge = gtk::Image::from_icon_name(icon);
    badge.set_pixel_size(26);
    badge.add_css_class("card-badge");
    badge.add_css_class(accent);
    header.append(&badge);
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
    titles.set_valign(gtk::Align::Center);
    let kicker = gtk::Label::new(Some(kicker));
    kicker.add_css_class("card-kicker");
    kicker.add_css_class(accent);
    kicker.set_xalign(0.0);
    let title = gtk::Label::new(Some(title));
    title.add_css_class("title-4");
    title.set_xalign(0.0);
    titles.append(&kicker);
    titles.append(&title);
    header.append(&titles);
    frame.append(&header);
    frame.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.add_css_class("card-body");
    body.set_vexpand(true);
    body.set_valign(gtk::Align::Start);
    frame.append(&body);
    frame.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    footer.add_css_class("card-footer");
    frame.append(&footer);
    (frame, body, footer)
}

fn field_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("heading");
    label.set_xalign(0.0);
    label
}

fn caption(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}

fn error_label() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class("error");
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_visible(false);
    label
}

fn wide_button(icon: &str, text: &str, style: &str) -> gtk::Button {
    let content = adw::ButtonContent::new();
    content.set_icon_name(icon);
    content.set_label(text);
    content.set_can_shrink(true);
    let button = gtk::Button::new();
    button.set_child(Some(&content));
    button.add_css_class("pill");
    button.add_css_class(style);
    button.set_hexpand(true);
    button
}

fn set_error(label: &gtk::Label, text: Option<String>) {
    match text {
        Some(text) => {
            label.set_text(&text);
            label.set_visible(true);
        }
        None => label.set_visible(false),
    }
}

impl MainWindow {
    pub fn new(app: &adw::Application) -> Rc<Self> {
        let viewer = ViewerModel::new();
        let hosting = HostingModel::new();
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(tr("LinuxViewer"))
            .default_width(1120)
            .default_height(780)
            .width_request(360)
            .height_request(480)
            .icon_name(crate::APP_ID)
            .build();

        // ---------------------------------------------------------- header
        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new(&tr("LinuxViewer"), &tr("Works with UltraViewer"));
        header.set_title_widget(Some(&title));
        let logo = gtk::Image::from_icon_name(crate::APP_ID);
        logo.set_pixel_size(28);
        header.pack_start(&logo);
        let menu = gio::Menu::new();
        menu.append(Some(&tr("Settings")), Some("app.preferences"));
        menu.append(Some(&tr("About LinuxViewer")), Some("app.about"));
        menu.append(Some(&tr("Quit")), Some("app.quit"));
        let menu_button = gtk::MenuButton::new();
        menu_button.set_icon_name("open-menu-symbolic");
        menu_button.set_menu_model(Some(&menu));
        menu_button.set_tooltip_text(Some(&tr("Settings")));
        header.pack_end(&menu_button);
        let host_indicator = gtk::MenuButton::new();
        host_indicator.add_css_class("flat");
        host_indicator.add_css_class("host-indicator");
        let host_paused = gtk::Label::new(None);
        host_paused.add_css_class("dim-label");
        header.pack_end(&host_paused);
        header.pack_end(&host_indicator);
        let host_chat = gtk::ToggleButton::new();
        host_chat.set_icon_name("chat-message-new-symbolic");
        host_chat.add_css_class("flat");
        host_chat.set_tooltip_text(Some(&tr("Chat")));
        header.pack_end(&host_chat);
        let host_clipboard = clipboard_button(&hosting.tools);
        header.pack_end(&host_clipboard);

        // ---------------------------------------------------------- connect card
        let (connect_card, body, footer) = card("computer-symbolic", &tr("CONNECT"), &tr("Connect to another computer"), "accent-blue");
        body.append(&field_label(&tr("Partner ID")));
        let id_entry = gtk::Entry::new();
        id_entry.set_placeholder_text(Some(&tr("Partner's UltraViewer ID")));
        id_entry.add_css_class("id-entry");
        id_entry.set_input_purpose(gtk::InputPurpose::Digits);
        id_entry.set_max_length(80);
        let id_display = gtk::Label::new(None);
        id_display.add_css_class("id-display");
        id_display.set_xalign(0.0);
        id_display.set_selectable(true);
        body.append(&id_entry);
        body.append(&id_display);
        body.append(&field_label(&tr("Partner password")));
        let password_entry = gtk::PasswordEntry::new();
        password_entry.set_show_peek_icon(true);
        password_entry.set_property("placeholder-text", tr("Partner's password"));
        let password_display = gtk::Label::new(Some(&tr("Password cleared")));
        password_display.add_css_class("dim-label");
        password_display.set_xalign(0.0);
        body.append(&password_entry);
        body.append(&password_display);
        let remember = gtk::CheckButton::with_label(&tr("Remember password on this computer"));
        remember.set_active(settings::get().remember_password);
        body.append(&remember);
        body.append(&caption(&tr("Passwords of successful connections are saved in the system keyring.")));
        let credential_label = caption("");
        credential_label.set_visible(false);
        body.append(&credential_label);
        let connect_error = error_label();
        body.append(&connect_error);
        let connect_button = wide_button("network-transmit-receive-symbolic", &tr("Connect to another computer"), "suggested-action");
        let active_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let open_button = wide_button("video-display-symbolic", &tr("Open screen"), "suggested-action");
        let disconnect_button = wide_button("process-stop-symbolic", &tr("Disconnect"), "destructive-action");
        active_row.append(&open_button);
        active_row.append(&disconnect_button);
        footer.append(&connect_button);
        footer.append(&active_row);
        let connect_status = caption(&tr("Turn on Control in the session toolbar to use the keyboard and mouse."));
        footer.append(&connect_status);

        // ---------------------------------------------------------- share card
        let (share_card, body, footer) = card("preferences-desktop-remote-desktop-symbolic", &tr("SHARE"), &tr("Share this computer"), "accent-teal");
        let share_idle = gtk::Box::new(gtk::Orientation::Vertical, 10);
        let setup = gtk::ListBox::new();
        setup.add_css_class("boxed-list");
        setup.set_selection_mode(gtk::SelectionMode::None);
        let capture_row = adw::ActionRow::builder()
            .title(tr("Screen capture"))
            .subtitle(tr("The desktop asks which screen to share when a connection is approved."))
            .build();
        capture_row.add_prefix(&gtk::Image::from_icon_name("video-display-symbolic"));
        setup.append(&capture_row);
        let input_row = adw::ActionRow::builder().title(tr("Keyboard and mouse control")).build();
        input_row.add_prefix(&gtk::Image::from_icon_name("input-keyboard-symbolic"));
        match input_support() {
            Some(backend) => {
                input_row.set_subtitle(&format!("{} · {backend}", tr("Ready")));
                let ok = gtk::Image::from_icon_name("emblem-ok-symbolic");
                ok.add_css_class("success");
                input_row.add_suffix(&ok);
            }
            None => input_row.set_subtitle(&format!(
                "{} {}",
                tr("Asks when sharing starts."),
                tr("Remote control needs write access to /dev/uinput (add your user to the input group) or a desktop with remote-desktop portal support.")
            )),
        }
        setup.append(&input_row);
        share_idle.append(&setup);
        body.append(&share_idle);

        let share_active = gtk::Box::new(gtk::Orientation::Vertical, 6);
        share_active.append(&field_label(&tr("Your ID")));
        let id_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let share_id = gtk::Label::new(None);
        share_id.add_css_class("id-display");
        share_id.set_selectable(true);
        share_id.set_hexpand(true);
        share_id.set_xalign(0.0);
        let copy_id = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy_id.add_css_class("flat");
        copy_id.set_tooltip_text(Some(&tr("Copy ID")));
        id_row.append(&share_id);
        id_row.append(&copy_id);
        share_active.append(&id_row);
        share_active.append(&field_label(&tr("Your password")));
        let password_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let share_password = gtk::Label::new(None);
        share_password.add_css_class("id-display");
        share_password.set_selectable(true);
        share_password.set_hexpand(true);
        share_password.set_xalign(0.0);
        let reveal_password = gtk::ToggleButton::new();
        reveal_password.set_icon_name("view-reveal-symbolic");
        reveal_password.add_css_class("flat");
        reveal_password.set_tooltip_text(Some(&tr("Show")));
        let copy_password = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy_password.add_css_class("flat");
        copy_password.set_tooltip_text(Some(&tr("Copy password")));
        let new_password = gtk::Button::from_icon_name("view-refresh-symbolic");
        new_password.add_css_class("flat");
        new_password.set_tooltip_text(Some(&tr("New password")));
        password_row.append(&share_password);
        password_row.append(&reveal_password);
        password_row.append(&copy_password);
        password_row.append(&new_password);
        share_active.append(&password_row);
        body.append(&share_active);
        let share_status_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let share_spinner = gtk::Spinner::new();
        share_spinner.set_spinning(true);
        let share_status = gtk::Label::new(None);
        share_status.add_css_class("dim-label");
        share_status.set_wrap(true);
        share_status.set_xalign(0.0);
        share_status_row.append(&share_spinner);
        share_status_row.append(&share_status);
        body.append(&share_status_row);
        let share_error = error_label();
        body.append(&share_error);
        let share_button = wide_button("network-wireless-symbolic", &tr("Start sharing this computer"), "share-action");
        footer.append(&share_button);
        footer.append(&caption(&tr("Enter this ID and password on the other computer. Your screen is shared only after you approve.")));

        // ---------------------------------------------------------- layout
        let cards = gtk::Box::new(gtk::Orientation::Horizontal, 20);
        cards.set_homogeneous(true);
        cards.append(&connect_card);
        cards.append(&share_card);

        let recent_group = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let recent_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let recent_titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let recent_title = gtk::Label::new(Some(&tr("Recent computers")));
        recent_title.add_css_class("title-4");
        recent_title.set_xalign(0.0);
        recent_titles.append(&recent_title);
        recent_titles.append(&caption(&tr("Choose a computer to reuse its connection details.")));
        recent_titles.set_hexpand(true);
        let clear_history = gtk::Button::with_label(&tr("Clear history"));
        clear_history.add_css_class("flat");
        clear_history.set_valign(gtk::Align::Center);
        recent_header.append(&recent_titles);
        recent_header.append(&clear_history);
        recent_group.append(&recent_header);
        let recent_list = gtk::ListBox::new();
        recent_list.add_css_class("boxed-list");
        recent_list.set_selection_mode(gtk::SelectionMode::None);
        recent_group.append(&recent_list);

        let page = gtk::Box::new(gtk::Orientation::Vertical, 18);
        page.set_valign(gtk::Align::Start);
        page.set_margin_top(24);
        page.set_margin_bottom(24);
        page.set_margin_start(16);
        page.set_margin_end(16);
        let heading = gtk::Label::new(Some(&tr("Connect and share")));
        heading.add_css_class("title-1");
        heading.set_xalign(0.0);
        page.append(&heading);
        let subheading = gtk::Label::new(Some(&tr("Control another computer, or share this computer.")));
        subheading.add_css_class("dim-label");
        subheading.set_xalign(0.0);
        subheading.set_wrap(true);
        page.append(&subheading);
        page.append(&cards);
        page.append(&recent_group);
        let clamp = adw::Clamp::builder().maximum_size(980).tightening_threshold(600).child(&page).build();
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();

        let host_chat_panel = ChatPanel::new(&hosting.tools, {
            let host_chat = host_chat.clone();
            move || host_chat.set_active(false)
        });
        let host_chat_split = adw::OverlaySplitView::new();
        host_chat_split.set_sidebar_position(gtk::PackType::End);
        host_chat_split.set_content(Some(&scroller));
        host_chat_split.set_sidebar(Some(&host_chat_panel.root));
        host_chat_split.set_show_sidebar(false);

        let home = adw::ToolbarView::new();
        home.add_top_bar(&header);
        home.set_content(Some(&host_chat_split));

        let stack = gtk::Stack::new();
        stack.set_hhomogeneous(false);
        stack.set_vhomogeneous(false);
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.add_named(&home, Some("home"));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));
        window.set_content(Some(&toasts));

        // Narrow windows stack the cards.
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(adw::BreakpointConditionLengthType::MaxWidth, 900.0, adw::LengthUnit::Sp));
        breakpoint.add_setter(&cards, "orientation", Some(&gtk::Orientation::Vertical.to_value()));
        breakpoint.add_setter(&cards, "homogeneous", Some(&false.to_value()));
        window.add_breakpoint(breakpoint);

        let this = Rc::new(Self {
            window,
            viewer,
            hosting,
            stack,
            toasts,
            session: RefCell::new(None),
            id_entry,
            id_display,
            password_entry,
            password_display,
            remember,
            connect_button,
            open_button,
            disconnect_button,
            connect_error,
            credential_label,
            connect_status,
            share_idle,
            share_active,
            share_id,
            share_password,
            reveal_password,
            share_status,
            share_error,
            share_button,
            share_spinner,
            host_indicator,
            host_paused,
            host_clipboard,
            host_chat,
            recent_group,
            recent_list,
            updating: Cell::new(false),
            request_dialog: RefCell::new(None),
            host_allow: RefCell::new(None),
            answered_request: Cell::new(None),
            side_switch_dialog: RefCell::new(None),
            host_chat_split,
            host_chat_panel,
        });
        this.build_session_page();
        this.wire(&copy_id, &copy_password, &new_password, &clear_history);
        this.refresh_viewer();
        this.refresh_hosting();
        this.refresh_recent();
        this.id_entry.grab_focus();
        this
    }

    pub fn toast(&self, text: &str) {
        let toast = adw::Toast::new(text);
        toast.set_timeout(4);
        self.toasts.add_toast(toast);
    }

    fn build_session_page(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let on_home: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(this) = weak.upgrade() {
                this.viewer.release_remote_input();
                this.stack.set_visible_child_name("home");
            }
        });
        let weak = Rc::downgrade(self);
        let toast: Rc<dyn Fn(String)> = Rc::new(move |text| {
            if let Some(this) = weak.upgrade() {
                this.toast(&text);
            }
        });
        let weak = Rc::downgrade(self);
        let on_switch_sides: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(this) = weak.upgrade() {
                this.request_side_switch();
            }
        });
        let page = SessionPage::new(self.viewer.clone(), on_home, toast, on_switch_sides);
        self.stack.add_named(&page.root, Some("session"));
        let compact = adw::Breakpoint::new(adw::BreakpointCondition::new_length(adw::BreakpointConditionLengthType::MaxWidth, 1100.0, adw::LengthUnit::Sp));
        for label in &page.compact_labels {
            compact.add_setter(label, "visible", Some(&false.to_value()));
        }
        self.window.add_breakpoint(compact);
        *self.session.borrow_mut() = Some(page);
    }

    /// Starts sharing if needed, then sends this computer's ID and password
    /// to the connected peer so it can connect back (UltraViewer command 54).
    fn request_side_switch(self: &Rc<Self>) {
        if self.viewer.phase() != Phase::Connected {
            return;
        }
        let peer = self.viewer.info().map(|i| i.partner_id);
        if !self.hosting.is_running() {
            self.hosting.start();
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            for _ in 0..120 {
                if this.viewer.phase() != Phase::Connected || this.viewer.info().map(|i| i.partner_id) != peer {
                    return;
                }
                if let Some(id) = this.hosting.identifier() {
                    let password = this.hosting.password();
                    if !password.is_empty() {
                        if let Ok(message) = ultraviewer_protocol::RemoteControlCommands::side_switch(&id, &password) {
                            this.viewer.send_commands(vec![message], false);
                        }
                        return;
                    }
                }
                if this.hosting.phase() == HostPhase::Failed {
                    return;
                }
                glib::timeout_future(std::time::Duration::from_millis(250)).await;
            }
        });
    }

    pub fn apply_settings(self: &Rc<Self>) {
        if let Some(page) = self.session.borrow().as_ref() {
            page.apply_settings();
        }
        self.refresh_recent();
    }

    fn local_screen_size(&self) -> (i32, i32) {
        let display = gtk::prelude::WidgetExt::display(&self.window);
        let monitor = self
            .window
            .surface()
            .and_then(|surface| display.monitor_at_surface(&surface))
            .or_else(|| display.monitors().item(0).and_then(|m| m.downcast::<gdk::Monitor>().ok()));
        match monitor {
            Some(monitor) => {
                let geometry = monitor.geometry();
                let scale = monitor.scale();
                ((f64::from(geometry.width()) * scale) as i32, (f64::from(geometry.height()) * scale) as i32)
            }
            None => (1920, 1080),
        }
    }

    fn connect(self: &Rc<Self>) {
        self.viewer.set_partner_id(&self.id_entry.text());
        self.viewer.set_password(&self.password_entry.text());
        let size = self.local_screen_size();
        self.viewer.connect(size);
        // The model has taken the password; clear the field without echoing.
        self.updating.set(true);
        self.password_entry.set_text("");
        self.updating.set(false);
    }

    fn wire(self: &Rc<Self>, copy_id: &gtk::Button, copy_password: &gtk::Button, new_password: &gtk::Button, clear_history: &gtk::Button) {
        let weak = Rc::downgrade(self);
        self.id_entry.connect_changed(move |entry| {
            if let Some(this) = weak.upgrade() {
                if !this.updating.get() {
                    this.viewer.set_partner_id(&entry.text());
                    this.viewer.clear_error();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.id_entry.connect_activate(move |_| {
            if let Some(this) = weak.upgrade() {
                this.password_entry.grab_focus();
            }
        });
        let weak = Rc::downgrade(self);
        self.password_entry.connect_changed(move |entry| {
            if let Some(this) = weak.upgrade() {
                if !this.updating.get() {
                    this.viewer.set_password(&entry.text());
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.password_entry.connect_activate(move |_| {
            if let Some(this) = weak.upgrade() {
                this.connect();
            }
        });
        let weak = Rc::downgrade(self);
        self.connect_button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.connect();
            }
        });
        let weak = Rc::downgrade(self);
        self.open_button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.stack.set_visible_child_name("session");
            }
        });
        let viewer = self.viewer.clone();
        self.disconnect_button.connect_clicked(move |_| viewer.disconnect());
        self.remember.connect_toggled(|check| {
            let active = check.is_active();
            settings::update(|s| s.remember_password = active);
        });

        let weak = Rc::downgrade(self);
        self.viewer.connect_changed(move |change| {
            if let Some(this) = weak.upgrade() {
                if change == Change::State {
                    this.refresh_viewer();
                }
            }
        });

        // Sharing.
        let weak = Rc::downgrade(self);
        self.share_button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                if this.hosting.is_running() {
                    this.hosting.stop();
                } else {
                    this.hosting.start();
                }
            }
        });
        let weak = Rc::downgrade(self);
        copy_id.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                if let Some(id) = this.hosting.identifier() {
                    this.window.clipboard().set_text(&id);
                    this.toast(&tr("Copy ID"));
                }
            }
        });
        let weak = Rc::downgrade(self);
        copy_password.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.window.clipboard().set_text(&this.hosting.password());
                this.toast(&tr("Copy password"));
            }
        });
        let hosting = self.hosting.clone();
        new_password.connect_clicked(move |_| hosting.reset_password());
        let weak = Rc::downgrade(self);
        self.reveal_password.connect_toggled(move |_| {
            if let Some(this) = weak.upgrade() {
                this.refresh_hosting();
            }
        });
        let weak = Rc::downgrade(self);
        self.hosting.connect_changed(move || {
            if let Some(this) = weak.upgrade() {
                this.refresh_hosting();
            }
        });
        let weak = Rc::downgrade(self);
        self.host_chat.connect_toggled(move |toggle| {
            if let Some(this) = weak.upgrade() {
                this.host_chat_split.set_show_sidebar(toggle.is_active());
                if toggle.is_active() {
                    this.viewer.release_remote_input();
                    this.hosting.tools.mark_read();
                    this.host_chat_panel.focus_entry();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.hosting.tools.connect_changed(move || {
            if let Some(this) = weak.upgrade() {
                if this.host_chat.is_active() {
                    this.hosting.tools.mark_read();
                }
                if this.hosting.tools.unread() > 0 {
                    this.host_chat.add_css_class("accent");
                } else {
                    this.host_chat.remove_css_class("accent");
                }
            }
        });
        // A side switch connects this computer to the same peer.
        let weak = Rc::downgrade(self);
        *self.hosting.on_side_switch.borrow_mut() = Some(Rc::new(move |id, password| {
            let Some(this) = weak.upgrade() else { return };
            this.viewer.disconnect();
            this.viewer.set_partner_id(&id);
            this.viewer.set_password(&password);
            let size = this.local_screen_size();
            this.viewer.connect(size);
        }));
        let viewer = self.viewer.clone();
        clear_history.connect_clicked(move |_| viewer.clear_recent());
        let hosting = Rc::downgrade(&self.hosting);
        self.viewer.set_command_port_provider(Rc::new(move || hosting.upgrade().and_then(|h| h.command_port())));

        // Host indicator menu actions.
        let actions = gio::SimpleActionGroup::new();
        let allow = gio::SimpleAction::new_stateful("allow-control", None, &false.to_variant());
        let hosting = self.hosting.clone();
        allow.connect_activate(move |action, _| {
            let next = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            hosting.set_control_allowed(next);
        });
        actions.add_action(&allow);
        *self.host_allow.borrow_mut() = Some(allow.clone());
        let end = gio::SimpleAction::new("end", None);
        let hosting = self.hosting.clone();
        end.connect_activate(move |_, _| hosting.end_viewer());
        actions.add_action(&end);
        let pause = gio::SimpleAction::new("pause", None);
        let hosting = self.hosting.clone();
        pause.connect_activate(move |_, _| hosting.stop());
        actions.add_action(&pause);
        self.window.insert_action_group("host", Some(&actions));

        // Release remote input when the window loses focus.
        let weak = Rc::downgrade(self);
        self.window.connect_is_active_notify(move |window| {
            if let Some(this) = weak.upgrade() {
                if !window.is_active() {
                    this.viewer.release_remote_input();
                }
            }
        });
        // Window actions.
        let fullscreen = gio::SimpleAction::new("fullscreen", None);
        let window = self.window.clone();
        fullscreen.connect_activate(move |_, _| {
            if window.is_fullscreen() {
                window.unfullscreen();
            } else {
                window.fullscreen();
            }
        });
        self.window.add_action(&fullscreen);
    }

    fn refresh_viewer(self: &Rc<Self>) {
        self.updating.set(true);
        let phase = self.viewer.phase();
        let active = self.viewer.is_session_active();
        let id = self.viewer.partner_id();
        if !active && normalize_id(&self.id_entry.text()) != id {
            self.id_entry.set_text(&id);
        }
        if !active && self.password_entry.text() != self.viewer.password() {
            self.password_entry.set_text(&self.viewer.password());
        }
        self.id_entry.set_visible(!active);
        self.id_display.set_visible(active);
        self.id_display.set_text(&format_id(&self.viewer.info().map(|i| i.partner_id).unwrap_or(id)));
        self.password_entry.set_visible(!active);
        self.password_display.set_visible(active);
        self.connect_button.set_visible(!active);
        self.open_button.set_visible(active);
        self.disconnect_button.set_visible(active);
        self.open_button.set_sensitive(phase == Phase::Connected || phase == Phase::Connecting);
        set_error(&self.connect_error, self.viewer.error());
        match self.viewer.credential_message() {
            Some(message) => {
                self.credential_label.set_text(&message);
                self.credential_label.set_visible(true);
            }
            None => self.credential_label.set_visible(false),
        }
        self.connect_status.set_text(&if active { self.viewer.status() } else { tr("Turn on Control in the session toolbar to use the keyboard and mouse.") });
        // Show the session when a connection starts; return home when it ends.
        match phase {
            Phase::Connecting => {
                if self.stack.visible_child_name().as_deref() != Some("session") {
                    self.stack.set_visible_child_name("session");
                }
            }
            Phase::Disconnected | Phase::Idle => {
                if self.stack.visible_child_name().as_deref() == Some("session") {
                    self.stack.set_visible_child_name("home");
                    if self.window.is_fullscreen() {
                        self.window.unfullscreen();
                    }
                }
            }
            Phase::Connected => {}
        }
        self.updating.set(false);
        self.refresh_recent();
    }

    fn refresh_recent(self: &Rc<Self>) {
        while let Some(child) = self.recent_list.first_child() {
            self.recent_list.remove(&child);
        }
        let recent = self.viewer.recent_partners();
        self.recent_group.set_visible(!recent.is_empty() && settings::get().remember_partners);
        for id in recent {
            let row = adw::ActionRow::new();
            row.set_title(&format_id(&id));
            row.add_css_class("recent-row");
            row.set_subtitle(&tr("Remote computer"));
            let icon = gtk::Image::from_icon_name("computer-symbolic");
            icon.set_pixel_size(24);
            icon.add_css_class("card-badge");
            icon.add_css_class("accent-blue");
            row.add_prefix(&icon);
            let select = gtk::Button::with_label(&tr("Select"));
            select.add_css_class("flat");
            select.set_valign(gtk::Align::Center);
            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.add_css_class("flat");
            remove.set_valign(gtk::Align::Center);
            remove.set_tooltip_text(Some(&tr("Remove from history")));
            row.add_suffix(&select);
            row.add_suffix(&remove);
            row.set_activatable_widget(Some(&select));
            let weak = Rc::downgrade(self);
            let chosen = id.clone();
            select.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.viewer.select_recent(&chosen);
                    this.password_entry.grab_focus();
                }
            });
            let viewer = self.viewer.clone();
            remove.connect_clicked(move |_| viewer.remove_recent(&id));
            self.recent_list.append(&row);
        }
    }

    fn refresh_hosting(self: &Rc<Self>) {
        let hosting = &self.hosting;
        let phase = hosting.phase();
        let running = hosting.is_running();
        let id = hosting.identifier();
        self.share_idle.set_visible(!running || id.is_none());
        self.share_active.set_visible(running && id.is_some());
        self.share_id.set_text(&id.as_deref().map(format_id).unwrap_or_default());
        let revealed = self.reveal_password.is_active();
        self.share_password.set_text(&if revealed { hosting.password() } else { "••••••••".into() });
        self.reveal_password.set_tooltip_text(Some(&if revealed { tr("Hide") } else { tr("Show") }));
        self.share_status.set_text(&hosting.status());
        self.share_spinner.set_visible(phase == HostPhase::Registering || hosting.is_approving());
        set_error(&self.share_error, hosting.error());
        let content = adw::ButtonContent::new();
        content.set_can_shrink(true);
        if running {
            content.set_icon_name("media-playback-stop-symbolic");
            content.set_label(&tr("Stop sharing"));
            self.share_button.remove_css_class("share-action");
        } else {
            content.set_icon_name("network-wireless-symbolic");
            content.set_label(&if phase == HostPhase::Failed { tr("Try sharing again") } else { tr("Start sharing this computer") });
            self.share_button.add_css_class("share-action");
        }
        self.share_button.set_child(Some(&content));

        // Header indicator.
        let sharing = phase == HostPhase::Sharing;
        self.host_indicator.set_visible(sharing);
        self.host_clipboard.set_visible(sharing);
        self.host_chat.set_visible(sharing);
        if !sharing && self.host_chat.is_active() {
            self.host_chat.set_active(false);
        }
        self.host_paused.set_visible(!sharing);
        self.host_paused.set_text(&if running { tr("Incoming access on") } else { tr("Incoming access paused") });
        if sharing {
            let viewer = hosting.viewer();
            let name = viewer.as_ref().and_then(|v| v.display_name.clone()).or_else(|| viewer.as_ref().map(|v| format_id(&v.viewer_id))).unwrap_or_else(|| tr("Another computer"));
            self.host_chat_panel.set_peer_name(&name);
            let menu = gio::Menu::new();
            let info = gio::Menu::new();
            info.append(Some(&trf("Viewing: {}", &[&name])), Some("host.noop"));
            menu.append_section(None, &info);
            let controls = gio::Menu::new();
            controls.append(Some(&tr("Allow control of this computer")), Some("host.allow-control"));
            menu.append_section(None, &controls);
            let end = gio::Menu::new();
            end.append(Some(&tr("End incoming session")), Some("host.end"));
            end.append(Some(&tr("Pause incoming access")), Some("host.pause"));
            menu.append_section(None, &end);
            self.host_indicator.set_menu_model(Some(&menu));
            let label = adw::ButtonContent::new();
            label.set_icon_name("network-wireless-signal-excellent-symbolic");
            label.set_label(&if hosting.is_ending_viewer() { tr("Ending the incoming session…") } else { tr("Screen shared") });
            self.host_indicator.set_child(Some(&label));
            if let Some(action) = self.host_allow.borrow().as_ref() {
                action.set_state(&hosting.allows_control().to_variant());
            }
        }

        // Incoming request dialog.
        let pending = hosting.pending_request();
        let showing = self.request_dialog.borrow().is_some();
        match (pending, showing) {
            (Some(request), false) if self.answered_request.get() != Some(request.id) => self.show_request(&request),
            (None, true) => {
                if let Some(dialog) = self.request_dialog.borrow_mut().take() {
                    dialog.force_close();
                }
            }
            _ => {}
        }
        if let Some(dialog) = self.request_dialog.borrow().as_ref() {
            let approving = hosting.is_approving();
            dialog.set_response_enabled("view", !approving);
            dialog.set_response_enabled("control", !approving);
        }
        let switch = hosting.pending_side_switch();
        let showing = self.side_switch_dialog.borrow().is_some();
        match (switch, showing) {
            (Some(_), false) => self.show_side_switch(),
            (None, true) => {
                if let Some(dialog) = self.side_switch_dialog.borrow_mut().take() {
                    dialog.force_close();
                }
            }
            _ => {}
        }
    }

    fn show_request(self: &Rc<Self>, request: &ultraviewer_protocol::host::IncomingRequest) {
        let name = request.display_name.clone().map(|n| format!("{n} ({})", format_id(&request.viewer_id))).unwrap_or_else(|| format_id(&request.viewer_id));
        let dialog = adw::AlertDialog::new(Some(&tr("Incoming connection")), Some(&trf("{} wants to access this computer.", &[&name])));
        dialog.set_body(&format!(
            "{}\n\n{}",
            trf("{} wants to access this computer.", &[&name]),
            tr("Allow viewing shares your screen. Allow control also gives access to your mouse and keyboard.")
        ));
        dialog.add_response("decline", &tr("Decline"));
        dialog.add_response("view", &tr("Allow viewing"));
        dialog.add_response("control", &tr("Allow control"));
        dialog.set_response_appearance("decline", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("control", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("view"));
        dialog.set_close_response("decline");
        #[cfg(feature = "gtk_4_16")]
        dialog.set_prefer_wide_layout(true);
        let request_id = request.id;
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(this) = weak.upgrade() else { return };
            this.request_dialog.borrow_mut().take();
            this.answered_request.set(Some(request_id));
            match response {
                "view" => this.hosting.approve(false),
                "control" => this.hosting.approve(true),
                _ => this.hosting.decline(),
            }
        });
        // Approving keeps the dialog open until the session starts.
        dialog.connect_closed({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(this) = weak.upgrade() {
                    this.request_dialog.borrow_mut().take();
                }
            }
        });
        *self.request_dialog.borrow_mut() = Some(dialog.clone());
        self.window.present();
        dialog.present(Some(&self.window));
    }

    fn show_side_switch(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(
            Some(&tr("The other computer wants to switch control sides.")),
            Some(&tr("End this incoming session and connect to the same computer? Control will stay off until you enable it.")),
        );
        dialog.add_response("decline", &tr("Decline"));
        dialog.add_response("connect", &tr("Connect to it"));
        dialog.set_response_appearance("connect", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("decline");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(this) = weak.upgrade() else { return };
            this.side_switch_dialog.borrow_mut().take();
            if response == "connect" {
                this.hosting.approve_side_switch();
            } else {
                this.hosting.decline_side_switch();
            }
        });
        *self.side_switch_dialog.borrow_mut() = Some(dialog.clone());
        dialog.present(Some(&self.window));
    }

    pub fn models(&self) -> (Rc<ViewerModel>, Rc<HostingModel>) {
        (self.viewer.clone(), self.hosting.clone())
    }
}

pub fn present_about(parent: &impl IsA<gtk::Widget>) {
    let about = adw::AboutDialog::builder()
        .application_name(tr("LinuxViewer"))
        .application_icon(crate::APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .comments(format!(
            "{}\n\n{}",
            tr("An independent Linux client for UltraViewer interoperability, ported from MacViewer."),
            tr("LinuxViewer is an independent, community-built implementation. It is not an official UltraViewer product. Only connect to computers you are authorized to access.")
        ))
        .license_type(gtk::License::MitX11)
        .website("https://ultraviewer.net")
        .developer_name("LinuxViewer contributors")
        .build();
    about.add_link("MacViewer", "https://github.com/DeanOpen/MacViewer");
    about.present(Some(parent));
}

pub fn present_preferences(parent: &Rc<MainWindow>) {
    let settings_now = settings::get();
    let dialog = adw::PreferencesDialog::new();
    dialog.set_search_enabled(false);
    let page = adw::PreferencesPage::new();
    page.set_title(&tr("General"));
    page.set_icon_name(Some("preferences-system-symbolic"));

    let language_group = adw::PreferencesGroup::new();
    language_group.set_title(&tr("Language"));
    let languages = gtk::StringList::new(&[&tr("System language"), "English", "Tiếng Việt"]);
    let language = adw::ComboRow::builder().title(tr("App language")).model(&languages).build();
    language.set_selected(match settings_now.language.as_str() {
        "en" => 1,
        "vi" => 2,
        _ => 0,
    });
    language_group.add(&language);
    let restart = adw::ActionRow::builder().title(tr("Restart LinuxViewer to apply the new language.")).visible(false).build();
    let restart_button = gtk::Button::with_label(&tr("Restart now"));
    restart_button.set_valign(gtk::Align::Center);
    restart_button.add_css_class("suggested-action");
    restart.add_suffix(&restart_button);
    language_group.add(&restart);
    let r = restart.clone();
    language.connect_selected_notify(move |row| {
        let value = match row.selected() {
            1 => "en",
            2 => "vi",
            _ => "system",
        };
        settings::update(|s| s.language = value.into());
        r.set_visible(true);
    });
    restart_button.connect_clicked(|button| {
        if let Some(app) = gio::Application::default() {
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).spawn();
            }
            app.quit();
        }
        button.set_sensitive(false);
    });
    page.add(&language_group);

    let history = adw::PreferencesGroup::new();
    history.set_title(&tr("History"));
    history.set_description(Some(&tr("Passwords are kept separately in the system keyring.")));
    let remember = adw::SwitchRow::builder().title(tr("Remember successfully connected partner IDs")).active(settings_now.remember_partners).build();
    remember.connect_active_notify(|row| {
        let active = row.is_active();
        settings::update(|s| s.remember_partners = active);
    });
    history.add(&remember);
    let clear = adw::ActionRow::builder().title(tr("Clear recent partners")).activatable(true).build();
    let viewer = parent.viewer.clone();
    clear.connect_activated(move |_| viewer.clear_recent());
    history.add(&clear);
    let forget = adw::ActionRow::builder().title(tr("Forget saved passwords")).activatable(true).build();
    forget.add_css_class("error");
    let weak = Rc::downgrade(parent);
    forget.connect_activated(move |_| {
        let weak = weak.clone();
        glib::spawn_future_local(async move {
            let result = crate::runtime::run(crate::credentials::remove_all()).await;
            if let Some(this) = weak.upgrade() {
                match result {
                    Ok(()) => this.toast(&tr("Saved partner passwords were removed from the keyring.")),
                    Err(error) => this.toast(&trf("Saved passwords could not be removed: {}", &[&error.to_string()])),
                }
            }
        });
    });
    history.add(&forget);
    page.add(&history);

    let screen = adw::PreferencesGroup::new();
    screen.set_title(&tr("Remote screen"));
    let smooth = adw::SwitchRow::builder().title(tr("Smooth image scaling")).active(settings_now.smooth_scaling).build();
    let weak = Rc::downgrade(parent);
    smooth.connect_active_notify(move |row| {
        let active = row.is_active();
        settings::update(|s| s.smooth_scaling = active);
        if let Some(this) = weak.upgrade() {
            this.apply_settings();
        }
    });
    screen.add(&smooth);
    let speed = adw::SpinRow::with_range(0.25, 2.0, 0.25);
    speed.set_title(&tr("Scroll speed"));
    speed.set_digits(2);
    speed.set_value(settings_now.scroll_sensitivity);
    let weak = Rc::downgrade(parent);
    speed.connect_value_notify(move |row| {
        let value = row.value();
        settings::update(|s| s.scroll_sensitivity = value);
        if let Some(this) = weak.upgrade() {
            this.apply_settings();
        }
    });
    screen.add(&speed);
    let shortcuts = adw::SwitchRow::builder()
        .title(tr("Send system shortcuts to the other computer"))
        .subtitle(tr("While controlling, Super and Alt+Tab go to the remote computer when the desktop allows it."))
        .active(settings_now.forward_system_shortcuts)
        .build();
    let weak = Rc::downgrade(parent);
    shortcuts.connect_active_notify(move |row| {
        let active = row.is_active();
        settings::update(|s| s.forward_system_shortcuts = active);
        if let Some(this) = weak.upgrade() {
            this.apply_settings();
        }
    });
    screen.add(&shortcuts);
    page.add(&screen);

    let connection = adw::PreferencesGroup::new();
    connection.set_title(&tr("Connection"));
    let clipboard = adw::SwitchRow::builder()
        .title(tr("Share clipboard with the other computer"))
        .subtitle(tr("Text copied on either computer can be pasted on the other. Applies to new sessions."))
        .active(settings_now.share_clipboard)
        .build();
    clipboard.connect_active_notify(|row| {
        let active = row.is_active();
        settings::update(|s| s.share_clipboard = active);
    });
    connection.add(&clipboard);
    let lan = adw::SwitchRow::builder()
        .title(tr("Direct connection on the local network"))
        .subtitle(tr("Uses a faster encrypted LAN path when both computers are on the same network, and falls back to the relay."))
        .active(settings_now.enable_lan_direct)
        .build();
    lan.connect_active_notify(|row| {
        let active = row.is_active();
        settings::update(|s| s.enable_lan_direct = active);
    });
    connection.add(&lan);
    let folder = adw::ActionRow::builder().title(tr("Capture folder")).subtitle(super::capture::capture_folder().display().to_string()).build();
    let choose = gtk::Button::with_label(&tr("Choose…"));
    choose.set_valign(gtk::Align::Center);
    folder.add_suffix(&choose);
    let f = folder.clone();
    let weak = Rc::downgrade(parent);
    choose.connect_clicked(move |_| {
        let Some(this) = weak.upgrade() else { return };
        let dialog = gtk::FileDialog::builder().title(tr("Capture folder")).build();
        let row = f.clone();
        dialog.select_folder(Some(&this.window), gio::Cancellable::NONE, move |result| {
            if let Ok(file) = result {
                if let Some(path) = file.path() {
                    let text = path.display().to_string();
                    settings::update(|s| s.capture_folder = Some(text.clone()));
                    row.set_subtitle(&text);
                }
            }
        });
    });
    connection.add(&folder);
    page.add(&connection);

    dialog.add(&page);
    dialog.present(Some(&parent.window));
}
