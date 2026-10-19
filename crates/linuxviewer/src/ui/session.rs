//! The remote session page: toolbar, remote screen and chat sidebar.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use ultraviewer_protocol::commands::RemoteAction;

use super::capture::{self, Capture, Recorder};
use super::chat::{clipboard_button, ChatPanel};
use crate::i18n::{tr, trf};
use crate::settings;
use crate::viewer::model::{format_id, Change, ImageQuality, Phase, ViewerModel};
use crate::viewer::screen::{Crop, CursorDraw, RemoteScreen};

pub struct SessionPage {
    pub root: adw::OverlaySplitView,
    model: Rc<ViewerModel>,
    screen: RemoteScreen,
    peer_label: gtk::Label,
    control: gtk::Switch,
    updating: Cell<bool>,
    status_box: gtk::Box,
    status_label: gtk::Label,
    toolbar_revealer: gtk::Revealer,
    show_toolbar: gtk::Button,
    chat: Rc<ChatPanel>,
    chat_toggle: gtk::ToggleButton,
    actions: gio::SimpleActionGroup,
    monitor_section: gio::Menu,
    capture_menu: gio::Menu,
    capture_button: gtk::MenuButton,
    recorder: RefCell<Option<Recorder>>,
    record_timer: RefCell<Option<glib::SourceId>>,
    toast: Rc<dyn Fn(String)>,
    monitors_shown: RefCell<Vec<usize>>,
    this: RefCell<std::rc::Weak<SessionPage>>,
    on_switch_sides: Rc<dyn Fn()>,
    /// Text labels hidden on narrow windows.
    pub compact_labels: Vec<gtk::Label>,
}

fn menu_button(icon: &str, label: &str, model: &gio::Menu, labels: &mut Vec<gtk::Label>) -> gtk::MenuButton {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(icon));
    let text = gtk::Label::new(Some(label));
    labels.push(text.clone());
    content.append(&text);
    let button = gtk::MenuButton::new();
    button.set_child(Some(&content));
    button.set_menu_model(Some(model));
    button.set_always_show_arrow(true);
    button.add_css_class("flat");
    button
}

impl SessionPage {
    pub fn new(model: Rc<ViewerModel>, on_home: Rc<dyn Fn()>, toast: Rc<dyn Fn(String)>, on_switch_sides: Rc<dyn Fn()>) -> Rc<Self> {
        let screen = RemoteScreen::new();
        let settings = settings::get();
        screen.set_smooth(settings.smooth_scaling);
        screen.set_sensitivity(settings.scroll_sensitivity);
        screen.set_forward_shortcuts(settings.forward_system_shortcuts);
        screen.update_property(&[gtk::accessible::Property::Label(&tr("Remote computer screen"))]);

        // ---------------------------------------------------------- toolbar
        let mut compact_labels: Vec<gtk::Label> = Vec::new();
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let home = gtk::Button::from_icon_name("go-previous-symbolic");
        home.add_css_class("flat");
        home.set_tooltip_text(Some(&tr("Return home without disconnecting")));
        let h = on_home.clone();
        home.connect_clicked(move |_| h());
        toolbar.append(&home);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.set_valign(gtk::Align::Center);
        let peer_label = gtk::Label::new(None);
        peer_label.add_css_class("heading");
        peer_label.set_xalign(0.0);
        peer_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        peer_label.set_max_width_chars(18);
        peer_label.set_width_chars(8);
        let subtitle = gtk::Label::new(Some(&tr("UltraViewer compatible")));
        subtitle.add_css_class("caption");
        subtitle.add_css_class("dim-label");
        subtitle.set_xalign(0.0);
        titles.append(&peer_label);
        titles.append(&subtitle);
        toolbar.append(&titles);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        toolbar.append(&spacer);

        let actions_menu = gio::Menu::new();
        let remote = gio::Menu::new();
        remote.append(Some(&tr("Send Ctrl-Alt-Delete")), Some("session.cad"));
        remote.append(Some(&tr("Lock remote screen")), Some("session.lock"));
        remote.append(Some(&tr("Restart remote computer…")), Some("session.restart"));
        actions_menu.append_section(None, &remote);
        let sides = gio::Menu::new();
        sides.append(Some(&tr("Switch control sides…")), Some("session.switch-sides"));
        actions_menu.append_section(None, &sides);
        let local = gio::Menu::new();
        local.append(Some(&tr("Refresh remote screen")), Some("session.refresh"));
        local.append(Some(&tr("Release pressed keys and buttons")), Some("session.release"));
        actions_menu.append_section(None, &local);
        toolbar.append(&menu_button("system-run-symbolic", &tr("Actions"), &actions_menu, &mut compact_labels));

        let configuration = gio::Menu::new();
        let monitor_section = gio::Menu::new();
        configuration.append_section(Some(&tr("Monitor")), &monitor_section);
        let display = gio::Menu::new();
        display.append(Some(&tr("Fit to window")), Some("session.scale::fit"));
        display.append(Some(&tr("Actual size (100%)")), Some("session.scale::actual"));
        display.append(Some(&tr("Smooth scaling")), Some("session.smooth"));
        configuration.append_section(Some(&tr("Display size")), &display);
        let options = gio::Menu::new();
        options.append(Some(&tr("View only")), Some("session.view-only"));
        options.append(Some(&tr("Show remote wallpaper")), Some("session.wallpaper"));
        options.append(Some(&tr("Show remote cursor")), Some("session.remote-cursor"));
        options.append(Some(&tr("Computer audio")), Some("session.audio"));
        configuration.append_section(None, &options);
        let quality = gio::Menu::new();
        quality.append(Some(&tr("Automatic")), Some("session.quality::automatic"));
        quality.append(Some(&tr("Prefer speed")), Some("session.quality::speed"));
        quality.append(Some(&tr("Prefer image quality")), Some("session.quality::quality"));
        quality.append(Some(&tr("Custom…")), Some("session.custom-quality"));
        configuration.append_section(Some(&tr("Image quality")), &quality);
        toolbar.append(&menu_button("preferences-system-symbolic", &tr("Configuration"), &configuration, &mut compact_labels));

        let capture_menu = gio::Menu::new();
        let capture_button = menu_button("camera-photo-symbolic", &tr("Capture"), &capture_menu, &mut compact_labels);
        toolbar.append(&capture_button);

        toolbar.append(&clipboard_button(&model.tools));
        let chat_toggle = gtk::ToggleButton::new();
        chat_toggle.set_icon_name("chat-message-new-symbolic");
        chat_toggle.set_tooltip_text(Some(&tr("Show chat")));
        chat_toggle.add_css_class("flat");
        toolbar.append(&chat_toggle);

        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let control_label = gtk::Label::new(Some(&tr("Control")));
        compact_labels.push(subtitle.clone());
        let control = gtk::Switch::new();
        control.set_valign(gtk::Align::Center);
        toolbar.append(&control_label);
        toolbar.append(&control);
        let disconnect = gtk::Button::with_label(&tr("Disconnect"));
        disconnect.add_css_class("destructive-action");
        disconnect.set_tooltip_text(Some(&tr("End the connection to this computer")));
        toolbar.append(&disconnect);
        let fullscreen = gtk::Button::from_icon_name("view-fullscreen-symbolic");
        fullscreen.add_css_class("flat");
        fullscreen.set_action_name(Some("win.fullscreen"));
        toolbar.append(&fullscreen);
        let hide = gtk::Button::from_icon_name("go-up-symbolic");
        hide.add_css_class("flat");
        hide.set_tooltip_text(Some(&tr("Hide session toolbar")));
        toolbar.append(&hide);

        // Narrow windows can scroll the toolbar instead of forcing a width.
        let toolbar_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&toolbar)
            .build();
        toolbar_scroller.add_css_class("session-toolbar");
        let toolbar_revealer = gtk::Revealer::builder().child(&toolbar_scroller).reveal_child(true).build();

        // ---------------------------------------------------------- screen
        let scroller = gtk::ScrolledWindow::builder().child(&screen).hexpand(true).vexpand(true).build();
        scroller.add_css_class("remote-background");
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&scroller));
        let status_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        status_box.set_halign(gtk::Align::Center);
        status_box.set_valign(gtk::Align::Center);
        status_box.add_css_class("osd");
        status_box.add_css_class("session-status");
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        spinner.set_size_request(32, 32);
        let status_label = gtk::Label::new(None);
        status_label.set_wrap(true);
        status_label.set_max_width_chars(40);
        status_label.set_justify(gtk::Justification::Center);
        status_box.append(&spinner);
        status_box.append(&status_label);
        overlay.add_overlay(&status_box);
        let show_toolbar = gtk::Button::from_icon_name("go-down-symbolic");
        show_toolbar.add_css_class("osd");
        show_toolbar.add_css_class("circular");
        show_toolbar.set_halign(gtk::Align::Center);
        show_toolbar.set_valign(gtk::Align::Start);
        show_toolbar.set_margin_top(4);
        show_toolbar.set_tooltip_text(Some(&tr("Show toolbar")));
        show_toolbar.set_visible(false);
        overlay.add_overlay(&show_toolbar);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&toolbar_revealer);
        content.append(&overlay);

        let root = adw::OverlaySplitView::new();
        root.set_sidebar_position(gtk::PackType::End);
        root.set_content(Some(&content));
        root.set_show_sidebar(false);
        root.set_max_sidebar_width(380.0);
        root.set_min_sidebar_width(280.0);

        let page_ref: Rc<RefCell<Option<std::rc::Weak<SessionPage>>>> = Rc::default();
        let close_chat = {
            let page_ref = page_ref.clone();
            move || {
                if let Some(page) = page_ref.borrow().as_ref().and_then(|w| w.upgrade()) {
                    page.chat_toggle.set_active(false);
                }
            }
        };
        let chat = ChatPanel::new(&model.tools, close_chat);
        root.set_sidebar(Some(&chat.root));

        let page = Rc::new(Self {
            root,
            model: model.clone(),
            screen,
            peer_label,
            control,
            updating: Cell::new(false),
            status_box,
            status_label,
            toolbar_revealer,
            show_toolbar,
            chat,
            chat_toggle,
            actions: gio::SimpleActionGroup::new(),
            monitor_section,
            capture_menu,
            capture_button,
            recorder: RefCell::new(None),
            record_timer: RefCell::new(None),
            toast,
            monitors_shown: RefCell::new(Vec::new()),
            this: RefCell::new(std::rc::Weak::new()),
            on_switch_sides,
            compact_labels,
        });
        *page.this.borrow_mut() = Rc::downgrade(&page);
        *page_ref.borrow_mut() = Some(Rc::downgrade(&page));
        page.root.insert_action_group("session", Some(&page.actions));
        page.setup_actions();
        page.rebuild_capture_menu();

        // Wiring.
        let m = model.clone();
        page.screen.set_on_input(Some(Rc::new(move |input| m.send(input))));
        let weak = Rc::downgrade(&page);
        page.control.connect_active_notify(move |switch| {
            if let Some(page) = weak.upgrade() {
                if !page.updating.get() {
                    page.model.set_control_enabled(switch.is_active());
                }
            }
        });
        let m = model.clone();
        disconnect.connect_clicked(move |_| m.disconnect());
        let weak = Rc::downgrade(&page);
        hide.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.toolbar_revealer.set_reveal_child(false);
                page.show_toolbar.set_visible(true);
            }
        });
        let weak = Rc::downgrade(&page);
        page.show_toolbar.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.toolbar_revealer.set_reveal_child(true);
                page.show_toolbar.set_visible(false);
            }
        });
        let weak = Rc::downgrade(&page);
        page.chat_toggle.connect_active_notify(move |toggle| {
            if let Some(page) = weak.upgrade() {
                page.root.set_show_sidebar(toggle.is_active());
                settings::update(|s| s.chat_sidebar = toggle.is_active());
                if toggle.is_active() {
                    page.model.release_remote_input();
                    page.model.tools.mark_read();
                    page.chat.focus_entry();
                }
            }
        });
        if settings.chat_sidebar {
            page.chat_toggle.set_active(true);
        }
        let weak = Rc::downgrade(&page);
        page.root.connect_show_sidebar_notify(move |split| {
            if let Some(page) = weak.upgrade() {
                if page.chat_toggle.is_active() != split.shows_sidebar() {
                    page.chat_toggle.set_active(split.shows_sidebar());
                }
            }
        });
        let weak = Rc::downgrade(&page);
        model.tools.connect_changed(move || {
            if let Some(page) = weak.upgrade() {
                if page.chat_toggle.is_active() {
                    page.model.tools.mark_read();
                }
                let unread = page.model.tools.unread();
                if unread > 0 {
                    page.chat_toggle.add_css_class("accent");
                } else {
                    page.chat_toggle.remove_css_class("accent");
                }
            }
        });

        let weak = Rc::downgrade(&page);
        model.connect_changed(move |change| {
            if let Some(page) = weak.upgrade() {
                match change {
                    Change::State => page.refresh(),
                    Change::Frame => page.refresh_frame(),
                    Change::Cursor => page.refresh_cursor(),
                }
            }
        });
        page.refresh();
        page
    }

    pub fn apply_settings(&self) {
        let settings = settings::get();
        self.screen.set_smooth(settings.smooth_scaling);
        self.screen.set_sensitivity(settings.scroll_sensitivity);
        self.screen.set_forward_shortcuts(settings.forward_system_shortcuts);
        self.set_state("smooth", &settings.smooth_scaling.to_variant());
    }

    fn set_state(&self, name: &str, value: &glib::Variant) {
        if let Some(action) = self.actions.lookup_action(name) {
            if let Ok(action) = action.downcast::<gio::SimpleAction>() {
                action.set_state(value);
            }
        }
    }

    fn set_enabled(&self, name: &str, enabled: bool) {
        if let Some(action) = self.actions.lookup_action(name) {
            if let Ok(action) = action.downcast::<gio::SimpleAction>() {
                action.set_enabled(enabled);
            }
        }
    }

    fn setup_actions(self: &Rc<Self>) {
        let add = |name: &str, f: Box<dyn Fn(&Rc<SessionPage>)>| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(page) = weak.upgrade() {
                    f(&page);
                }
            });
            self.actions.add_action(&action);
        };
        add("cad", Box::new(|p| {
            p.model.release_remote_input();
            p.model.perform_action(RemoteAction::ControlAltDelete);
        }));
        add("lock", Box::new(|p| {
            p.model.release_remote_input();
            p.model.perform_action(RemoteAction::LockScreen);
        }));
        add("restart", Box::new(|p| p.confirm_restart()));
        add("release", Box::new(|p| p.model.release_remote_input()));
        add("switch-sides", Box::new(|p| p.confirm_switch_sides()));
        add("refresh", Box::new(|p| p.model.refresh_screen()));
        add("custom-quality", Box::new(|p| p.custom_quality()));
        add("screenshot-save", Box::new(|p| p.save_screenshot()));
        add("screenshot-as", Box::new(|p| p.save_screenshot_as()));
        add("screenshot-copy", Box::new(|p| {
            if let Some(capture) = p.current_capture() {
                capture::copy_to_clipboard(&capture);
                (p.toast)(tr("Screenshot copied"));
            }
        }));
        add("open-folder", Box::new(|_| capture::open_capture_folder()));
        add("annotate", Box::new(|p| p.open_editor(false)));
        add("region", Box::new(|p| p.open_editor(true)));
        add("record", Box::new(|p| p.toggle_recording()));

        let stateful = |name: &str, initial: glib::Variant, f: Box<dyn Fn(&Rc<SessionPage>, &glib::Variant)>| {
            let action = gio::SimpleAction::new_stateful(name, None, &initial);
            let weak = Rc::downgrade(self);
            action.connect_change_state(move |action, value| {
                let Some(value) = value else { return };
                action.set_state(value);
                if let Some(page) = weak.upgrade() {
                    if !page.updating.get() {
                        f(&page, value);
                    }
                }
            });
            action.connect_activate(|action, _| {
                if let Some(state) = action.state().and_then(|s| s.get::<bool>()) {
                    action.change_state(&(!state).to_variant());
                }
            });
            self.actions.add_action(&action);
        };
        stateful("smooth", settings::get().smooth_scaling.to_variant(), Box::new(|p, v| {
            let smooth = v.get::<bool>().unwrap_or(true);
            settings::update(|s| s.smooth_scaling = smooth);
            p.screen.set_smooth(smooth);
        }));
        stateful("view-only", true.to_variant(), Box::new(|p, v| {
            p.model.set_control_enabled(!v.get::<bool>().unwrap_or(true));
        }));
        stateful("wallpaper", true.to_variant(), Box::new(|p, v| p.model.set_wallpaper_visible(v.get::<bool>().unwrap_or(true))));
        stateful("remote-cursor", settings::get().show_remote_cursor.to_variant(), Box::new(|p, v| {
            p.model.set_show_remote_cursor(v.get::<bool>().unwrap_or(true));
        }));
        stateful("audio", false.to_variant(), Box::new(|p, v| p.model.set_computer_audio(v.get::<bool>().unwrap_or(false))));

        let radio = |name: &str, initial: glib::Variant, f: Box<dyn Fn(&Rc<SessionPage>, &glib::Variant)>| {
            let action = gio::SimpleAction::new_stateful(name, Some(initial.type_()), &initial);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |action, parameter| {
                let Some(parameter) = parameter else { return };
                action.set_state(parameter);
                if let Some(page) = weak.upgrade() {
                    if !page.updating.get() {
                        f(&page, parameter);
                    }
                }
            });
            self.actions.add_action(&action);
        };
        radio("scale", "fit".to_variant(), Box::new(|p, v| {
            p.model.release_remote_input();
            p.screen.set_actual_size(v.get::<String>().as_deref() == Some("actual"));
        }));
        radio("quality", "automatic".to_variant(), Box::new(|p, v| {
            let quality = match v.get::<String>().as_deref() {
                Some("speed") => ImageQuality::Speed,
                Some("quality") => ImageQuality::Quality,
                _ => ImageQuality::Automatic,
            };
            p.model.set_quality(quality);
        }));
        radio("monitor", 0i32.to_variant(), Box::new(|p, v| {
            p.model.select_monitor(v.get::<i32>().unwrap_or(0).max(0) as usize);
        }));
    }

    fn rebuild_capture_menu(&self) {
        self.capture_menu.remove_all();
        let images = gio::Menu::new();
        images.append(Some(&tr("Capture and annotate…")), Some("session.annotate"));
        images.append(Some(&tr("Capture a region…")), Some("session.region"));
        images.append(Some(&tr("Save screenshot as PNG…")), Some("session.screenshot-as"));
        images.append(Some(&tr("Copy screenshot")), Some("session.screenshot-copy"));
        self.capture_menu.append_section(None, &images);
        let folder = gio::Menu::new();
        folder.append(Some(&tr("Open capture folder")), Some("session.open-folder"));
        self.capture_menu.append_section(None, &folder);
        let video = gio::Menu::new();
        let label = match self.recorder.borrow().as_ref() {
            Some(recorder) => {
                let seconds = recorder.started.elapsed().as_secs();
                trf("Stop recording ({})", &[&format!("{}:{:02}", seconds / 60, seconds % 60)])
            }
            None => tr("Record remote screen"),
        };
        video.append(Some(&label), Some("session.record"));
        self.capture_menu.append_section(None, &video);
    }

    fn refresh(&self) {
        self.updating.set(true);
        let phase = self.model.phase();
        let info = self.model.info();
        let connected = phase == Phase::Connected;
        let title = info.as_ref().map(|i| i.title()).unwrap_or_else(|| format_id(&self.model.partner_id()));
        self.peer_label.set_text(&title);
        self.chat.set_peer_name(&title);
        let allows = info.as_ref().is_some_and(|i| i.allows_remote_input);
        let control = self.model.control_enabled();
        self.control.set_active(control);
        self.control.set_sensitive(connected && allows);
        self.screen.set_input_enabled(connected && control);
        self.status_box.set_visible(phase == Phase::Connecting);
        self.status_label.set_text(&self.model.status());
        for name in ["cad", "lock", "restart"] {
            self.set_enabled(name, connected && control);
        }
        self.set_enabled("wallpaper", connected && control);
        self.set_enabled("view-only", allows && connected);
        self.set_enabled("release", control);
        for name in ["annotate", "region", "switch-sides", "refresh", "audio", "remote-cursor", "quality", "custom-quality", "screenshot-as", "screenshot-copy", "record", "monitor"] {
            self.set_enabled(name, connected);
        }
        self.set_enabled("audio", connected && !self.model.changing_audio());
        self.set_state("view-only", &(!control).to_variant());
        self.set_state("wallpaper", &self.model.wallpaper_visible().to_variant());
        self.set_state("remote-cursor", &self.model.show_remote_cursor().to_variant());
        self.set_state("audio", &self.model.audio_enabled().to_variant());
        let quality = match self.model.quality() {
            ImageQuality::Automatic => "automatic",
            ImageQuality::Speed => "speed",
            ImageQuality::Quality => "quality",
            ImageQuality::Custom(_) => "custom",
        };
        self.set_state("quality", &quality.to_variant());
        self.set_state("monitor", &(self.model.selected_monitor() as i32).to_variant());
        let monitors: Vec<usize> = info.as_ref().map(|i| i.monitors.iter().map(|m| m.index).collect()).unwrap_or_default();
        if *self.monitors_shown.borrow() != monitors {
            self.monitor_section.remove_all();
            if let Some(info) = &info {
                if !info.monitors.is_empty() {
                    let item = gio::MenuItem::new(Some(&tr("All monitors")), None);
                    item.set_action_and_target_value(Some("session.monitor"), Some(&0i32.to_variant()));
                    self.monitor_section.append_item(&item);
                    for monitor in &info.monitors {
                        let label = trf("Monitor {} · {} × {}", &[&monitor.index.to_string(), &monitor.width().to_string(), &monitor.height().to_string()]);
                        let item = gio::MenuItem::new(Some(&label), None);
                        item.set_action_and_target_value(Some("session.monitor"), Some(&(monitor.index as i32).to_variant()));
                        self.monitor_section.append_item(&item);
                    }
                }
            }
            *self.monitors_shown.borrow_mut() = monitors;
        }
        if !connected {
            self.screen.clear();
            self.stop_recording();
        }
        self.updating.set(false);
    }

    fn refresh_frame(&self) {
        let Some(frame) = self.model.frame() else { return };
        let bitmap = &frame.bitmap;
        let crop = self.model.selected_region().map(|(x, y, width, height)| Crop { x, y, width, height });
        self.screen.set_crop(crop);
        self.screen.set_frame(bitmap.width, bitmap.height, bitmap.bgra.clone(), frame.damage);
        self.refresh_cursor();
    }

    fn self_weak(&self) -> Option<std::rc::Weak<SessionPage>> {
        Some(self.this.borrow().clone())
    }

    fn refresh_cursor(&self) {
        let info = self.model.info();
        if !self.model.show_remote_cursor() || self.model.phase() != Phase::Connected || info.is_none() {
            self.screen.set_cursor_draw(None);
            return;
        }
        let info = info.unwrap();
        let snapshot = self.model.cursor_snapshot();
        let Some((x, y)) = snapshot.position else {
            self.screen.set_cursor_draw(None);
            return;
        };
        let (rx, ry) = self.model.selected_region().map(|(x, y, _, _)| (x as f64, y as f64)).unwrap_or((0.0, 0.0));
        self.screen.set_cursor_draw(Some(CursorDraw {
            texture: snapshot.shape.texture.clone(),
            hot_spot: snapshot.shape.hot_spot,
            position: (x - f64::from(info.virtual_left) - rx, y - f64::from(info.virtual_top) - ry),
        }));
    }

    fn current_capture(&self) -> Option<Capture> {
        let frame = self.model.frame()?;
        Some(Capture::from_frame(frame.bitmap.width, frame.bitmap.height, &frame.bitmap.bgra, self.model.selected_region()))
    }

    fn peer_title(&self) -> String {
        self.model.info().map(|i| i.title()).unwrap_or_else(|| self.model.partner_id())
    }

    fn window(&self) -> Option<gtk::Window> {
        self.root.root().and_then(|r| r.downcast::<gtk::Window>().ok())
    }

    fn open_editor(&self, select_region: bool) {
        self.model.release_remote_input();
        let (Some(capture), Some(window)) = (self.current_capture(), self.window()) else { return };
        super::editor::open(&window, capture, &self.peer_title(), select_region, self.toast.clone());
    }

    fn save_screenshot(&self) {
        let Some(capture) = self.current_capture() else { return };
        match capture::save_png(&capture, &self.peer_title()) {
            Ok(path) => (self.toast)(trf("Screenshot saved to {}", &[&path.display().to_string()])),
            Err(error) => (self.toast)(format!("{}: {error}", tr("Capture failed"))),
        }
    }

    fn save_screenshot_as(&self) {
        self.model.release_remote_input();
        let (Some(capture), Some(window)) = (self.current_capture(), self.window()) else { return };
        let toast = self.toast.clone();
        capture::save_png_as(capture, self.peer_title(), &window, move |result| match result {
            Ok(path) => toast(trf("Screenshot saved to {}", &[&path.display().to_string()])),
            Err(error) => toast(format!("{}: {error}", tr("Capture failed"))),
        });
    }

    fn toggle_recording(&self) {
        if self.recorder.borrow().is_some() {
            self.stop_recording();
            return;
        }
        let Some(capture) = self.current_capture() else { return };
        match Recorder::start(capture.width, capture.height, &self.peer_title()) {
            Ok(recorder) => {
                *self.recorder.borrow_mut() = Some(recorder);
                self.capture_button.add_css_class("error");
                self.rebuild_capture_menu();
                // Frames are written at a constant 15 fps (repeating the last
                // decoded frame), so the video keeps real time even when the
                // remote screen is idle.
                let weak = self.self_weak();
                let mut written: u64 = 0;
                let mut last_label = 0;
                let timer = glib::timeout_add_local(Duration::from_millis(1000 / 15), move || {
                    let Some(page) = weak.as_ref().and_then(|w| w.upgrade()) else { return glib::ControlFlow::Break };
                    let guard = page.recorder.borrow();
                    let Some(recorder) = guard.as_ref() else { return glib::ControlFlow::Break };
                    let Some(capture) = page.current_capture() else { return glib::ControlFlow::Continue };
                    let elapsed = recorder.started.elapsed();
                    let due = (elapsed.as_secs_f64() * 15.0) as u64 + 1;
                    let mut ok = true;
                    while written < due && ok {
                        ok = recorder.push(capture.width, capture.height, capture.pixels.clone());
                        written += 1;
                    }
                    drop(guard);
                    if !ok {
                        glib::idle_add_local_once(move || page.stop_recording());
                        return glib::ControlFlow::Break;
                    }
                    if elapsed.as_secs() != last_label {
                        last_label = elapsed.as_secs();
                        page.rebuild_capture_menu();
                    }
                    glib::ControlFlow::Continue
                });
                *self.record_timer.borrow_mut() = Some(timer);
            }
            Err(error) => (self.toast)(error),
        }
    }

    fn stop_recording(&self) {
        let Some(recorder) = self.recorder.borrow_mut().take() else { return };
        if let Some(timer) = self.record_timer.borrow_mut().take() {
            timer.remove();
        }
        self.capture_button.remove_css_class("error");
        self.rebuild_capture_menu();
        let toast = self.toast.clone();
        recorder.finish(move |result| match result {
            Ok(path) => toast(trf("Recording saved to {}", &[&path.display().to_string()])),
            Err(error) => toast(trf("The recording could not be saved: {}", &[&error])),
        });
    }

    fn confirm_restart(&self) {
        self.model.release_remote_input();
        let dialog = adw::AlertDialog::new(
            Some(&trf("Restart {}?", &[&self.peer_title()])),
            Some(&tr("The remote session will disconnect. Unsaved work on that computer may be lost.")),
        );
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("restart", &tr("Restart remote computer"));
        dialog.set_response_appearance("restart", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        let model = self.model.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "restart" {
                model.perform_action(RemoteAction::Restart);
            }
        });
        dialog.present(Some(&self.root));
    }

    fn confirm_switch_sides(&self) {
        self.model.release_remote_input();
        let dialog = adw::AlertDialog::new(
            Some(&tr("Switch control sides?")),
            Some(&tr("LinuxViewer will share this computer's connection ID and password with the connected computer. You will approve its incoming request here.")),
        );
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("switch", &tr("Let the other computer connect to this computer"));
        dialog.set_response_appearance("switch", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("cancel"));
        let callback = self.on_switch_sides.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "switch" {
                callback();
            }
        });
        dialog.present(Some(&self.root));
    }

    fn custom_quality(&self) {
        self.model.release_remote_input();
        let current = match self.model.quality() {
            ImageQuality::Custom(value) => value,
            ImageQuality::Speed => 30,
            ImageQuality::Quality => 70,
            ImageQuality::Automatic => 45,
        };
        let dialog = adw::AlertDialog::new(
            Some(&tr("Image quality")),
            Some(&tr("Lower quality uses less bandwidth. Higher quality keeps more image detail.")),
        );
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 100.0, 1.0);
        scale.set_value(f64::from(current));
        scale.set_draw_value(true);
        scale.set_digits(0);
        scale.set_width_request(280);
        dialog.set_extra_child(Some(&scale));
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("apply", &tr("Apply"));
        dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
        let model = self.model.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "apply" {
                model.set_quality(ImageQuality::Custom(scale.value().round() as i32));
            }
        });
        dialog.present(Some(&self.root));
    }
}
