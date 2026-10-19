//! Displays decoded peer frames and turns local pointer/keyboard events into
//! [`RemoteInput`]. It never captures the local screen.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use super::input::{is_modifier_key, scroll_detents, Modifiers, RemoteInput};

/// Region of the texture shown by the widget (monitor selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

#[derive(Clone)]
pub struct CursorDraw {
    pub texture: gdk::Texture,
    pub hot_spot: (f64, f64),
    /// In the displayed (cropped) image's pixel coordinates.
    pub position: (f64, f64),
}

pub type InputCallback = Rc<dyn Fn(RemoteInput)>;

/// Maps a key symbol to the US-layout evdev position, so virtual keyboards
/// and remapped layouts still send the key the user meant. `None` falls back
/// to the hardware key code.
fn evdev_from_keyval(keyval: gdk::Key) -> Option<u16> {
    use gdk::Key as K;
    use glib::translate::IntoGlib;
    let named = match keyval {
        K::Escape => 1,
        K::BackSpace => 14,
        K::Tab | K::ISO_Left_Tab => 15,
        K::Return => 28,
        K::space => 57,
        K::Shift_L => 42,
        K::Shift_R => 54,
        K::Control_L => 29,
        K::Control_R => 97,
        K::Alt_L | K::Meta_L => 56,
        K::Alt_R | K::ISO_Level3_Shift => 100,
        K::Super_L => 125,
        K::Super_R => 126,
        K::Menu => 127,
        K::Caps_Lock => 58,
        K::Num_Lock => 69,
        K::Scroll_Lock => 70,
        K::Print => 99,
        K::Pause => 119,
        K::Home => 102,
        K::Up => 103,
        K::Page_Up => 104,
        K::Left => 105,
        K::Right => 106,
        K::End => 107,
        K::Down => 108,
        K::Page_Down => 109,
        K::Insert => 110,
        K::Delete => 111,
        K::KP_Enter => 96,
        K::KP_Divide => 98,
        K::KP_Multiply => 55,
        K::KP_Subtract => 74,
        K::KP_Add => 78,
        K::KP_Decimal | K::KP_Delete => 83,
        K::KP_Equal => 117,
        K::KP_0 | K::KP_Insert => 82,
        K::KP_1 | K::KP_End => 79,
        K::KP_2 | K::KP_Down => 80,
        K::KP_3 | K::KP_Page_Down => 81,
        K::KP_4 | K::KP_Left => 75,
        K::KP_5 | K::KP_Begin => 76,
        K::KP_6 | K::KP_Right => 77,
        K::KP_7 | K::KP_Home => 71,
        K::KP_8 | K::KP_Up => 72,
        K::KP_9 | K::KP_Page_Up => 73,
        K::AudioMute => 113,
        K::AudioLowerVolume => 114,
        K::AudioRaiseVolume => 115,
        K::AudioNext => 163,
        K::AudioPlay | K::AudioPause => 164,
        K::AudioPrev => 165,
        K::AudioStop => 166,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    let raw = keyval.into_glib();
    if (K::F1.into_glib()..=K::F10.into_glib()).contains(&raw) {
        return Some((raw - K::F1.into_glib()) as u16 + 59);
    }
    if raw == K::F11.into_glib() {
        return Some(87);
    }
    if raw == K::F12.into_glib() {
        return Some(88);
    }
    if (K::F13.into_glib()..=K::F20.into_glib()).contains(&raw) {
        return Some((raw - K::F13.into_glib()) as u16 + 183);
    }
    const LETTERS: [u16; 26] = [30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44];
    let character = keyval.to_unicode()?;
    Some(match character.to_ascii_lowercase() {
        c @ 'a'..='z' => LETTERS[(c as u8 - b'a') as usize],
        c @ '1'..='9' => u16::from(c as u8 - b'1') + 2,
        '0' | ')' => 11,
        '!' => 2,
        '@' => 3,
        '#' => 4,
        '$' => 5,
        '%' => 6,
        '^' => 7,
        '&' => 8,
        '*' => 9,
        '(' => 10,
        '-' | '_' => 12,
        '=' | '+' => 13,
        '[' | '{' => 26,
        ']' | '}' => 27,
        ';' | ':' => 39,
        '\'' | '"' => 40,
        '`' | '~' => 41,
        '\\' | '|' => 43,
        ',' | '<' => 51,
        '.' | '>' => 52,
        '/' | '?' => 53,
        _ => return None,
    })
}

/// The evdev code for a key event: key symbol first, hardware code second.
fn key_code(keyval: gdk::Key, keycode: u32) -> u16 {
    evdev_from_keyval(keyval).unwrap_or_else(|| keycode.saturating_sub(8) as u16)
}


/// Shares a frame buffer with GTK without copying.
struct SharedPixels(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedPixels {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RemoteScreen {
        pub texture: RefCell<Option<gdk::Texture>>,
        pub crop: Cell<Option<Crop>>,
        pub smooth: Cell<bool>,
        pub actual_size: Cell<bool>,
        pub cursor: RefCell<Option<CursorDraw>>,
        pub input_enabled: Cell<bool>,
        pub forward_shortcuts: Cell<bool>,
        pub sensitivity: Cell<f64>,
        pub preedit: RefCell<String>,
        pub on_input: RefCell<Option<InputCallback>>,
        pub raw_keys_down: RefCell<HashSet<u16>>,
        /// Key currently being filtered by the input method: (evdev, text).
        pub key_in_flight: RefCell<Option<(u16, Option<String>, Modifiers)>>,
        pub consumed: Cell<bool>,
        pub im: RefCell<Option<gtk::IMMulticontext>>,
        pub pointer: Cell<Option<(f64, f64)>>,
        pub shortcuts_inhibited: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RemoteScreen {
        const NAME: &'static str = "LvRemoteScreen";
        type Type = super::RemoteScreen;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_css_name("remote-screen");
            class.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for RemoteScreen {
        fn constructed(&self) {
            self.parent_constructed();
            self.smooth.set(true);
            self.sensitivity.set(1.0);
            self.forward_shortcuts.set(true);
            let widget = self.obj();
            widget.set_focusable(true);
            widget.set_hexpand(true);
            widget.set_vexpand(true);
            widget.set_overflow(gtk::Overflow::Hidden);
            widget.setup_controllers();
        }
    }

    impl WidgetImpl for RemoteScreen {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            if !self.actual_size.get() {
                return (0, 0, -1, -1);
            }
            let (width, height) = self.obj().image_size();
            let scale = self.obj().device_scale();
            let size = if orientation == gtk::Orientation::Horizontal { width } else { height };
            let logical = (size as f64 / scale).ceil() as i32;
            (logical, logical, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (width, height) = (widget.width() as f32, widget.height() as f32);
            snapshot.append_color(&gdk::RGBA::BLACK, &graphene::Rect::new(0.0, 0.0, width, height));
            let Some(texture) = self.texture.borrow().clone() else { return };
            let Some(rect) = widget.image_rect() else { return };
            let crop = widget.crop();
            let scale = rect.width() / crop.width as f32;
            let filter = if self.smooth.get() { gsk::ScalingFilter::Linear } else { gsk::ScalingFilter::Nearest };
            snapshot.push_clip(&rect);
            let texture_rect = graphene::Rect::new(
                rect.x() - crop.x as f32 * scale,
                rect.y() - crop.y as f32 * scale,
                texture.width() as f32 * scale,
                texture.height() as f32 * scale,
            );
            snapshot.append_scaled_texture(&texture, filter, &texture_rect);
            if let Some(cursor) = self.cursor.borrow().as_ref() {
                let (x, y) = cursor.position;
                if x >= 0.0 && y >= 0.0 && x < crop.width as f64 && y < crop.height as f64 {
                    let cursor_rect = graphene::Rect::new(
                        rect.x() + ((x - cursor.hot_spot.0) as f32) * scale,
                        rect.y() + ((y - cursor.hot_spot.1) as f32) * scale,
                        cursor.texture.width() as f32 * scale.max(0.5),
                        cursor.texture.height() as f32 * scale.max(0.5),
                    );
                    snapshot.append_scaled_texture(&cursor.texture, gsk::ScalingFilter::Linear, &cursor_rect);
                }
            }
            snapshot.pop();
            let preedit = self.preedit.borrow();
            if !preedit.is_empty() {
                let layout = widget.create_pango_layout(Some(&preedit));
                let (text_width, text_height) = layout.pixel_size();
                let padding = 8.0f32;
                let box_width = text_width as f32 + padding * 2.0;
                let box_height = text_height as f32 + padding;
                let x = (width - box_width) / 2.0;
                let y = height - box_height - 24.0;
                let background = gsk::RoundedRect::from_rect(graphene::Rect::new(x, y, box_width, box_height), 6.0);
                snapshot.push_rounded_clip(&background);
                snapshot.append_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.75), background.bounds());
                snapshot.pop();
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x + padding, y + padding / 2.0));
                snapshot.append_layout(&layout, &gdk::RGBA::WHITE);
                snapshot.restore();
            }
        }

        fn unrealize(&self) {
            self.obj().restore_shortcuts();
            self.parent_unrealize();
        }
    }
}

glib::wrapper! {
    pub struct RemoteScreen(ObjectSubclass<imp::RemoteScreen>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for RemoteScreen {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl RemoteScreen {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_on_input(&self, callback: Option<InputCallback>) {
        *self.imp().on_input.borrow_mut() = callback;
    }

    fn emit(&self, input: RemoteInput) {
        let callback = self.imp().on_input.borrow().clone();
        if let Some(callback) = callback {
            callback(input);
        }
    }

    /// Replaces the frame. `damage` is the changed region of the full frame,
    /// used to upload only those pixels when the previous texture matches.
    pub fn set_frame(&self, width: usize, height: usize, pixels: Arc<Vec<u8>>, damage: Option<(usize, usize, usize, usize)>) {
        let imp = self.imp();
        let bytes = glib::Bytes::from_owned(SharedPixels(pixels));
        let texture = Self::frame_texture(imp.texture.borrow().as_ref(), width, height, &bytes, damage);
        let size_changed = imp.texture.borrow().as_ref().map(|t| (t.width(), t.height())) != Some((width as i32, height as i32));
        *imp.texture.borrow_mut() = Some(texture);
        if size_changed && imp.actual_size.get() {
            self.queue_resize();
        }
        self.queue_draw();
    }

    #[cfg(feature = "gtk_4_16")]
    fn frame_texture(previous: Option<&gdk::Texture>, width: usize, height: usize, bytes: &glib::Bytes,
                     damage: Option<(usize, usize, usize, usize)>) -> gdk::Texture {
        let mut builder = gdk::MemoryTextureBuilder::new()
            .set_bytes(Some(bytes))
            .set_width(width as i32)
            .set_height(height as i32)
            .set_stride(width * 4)
            .set_format(gdk::MemoryFormat::B8g8r8a8Premultiplied);
        if let (Some(previous), Some((x, y, w, h))) = (previous, damage) {
            if previous.width() == width as i32 && previous.height() == height as i32 {
                let region = gtk::cairo::Region::create_rectangle(&gtk::cairo::RectangleInt::new(x as i32, y as i32, w as i32, h as i32));
                builder = builder.set_update_texture(Some(previous)).set_update_region(Some(&region));
            }
        }
        builder.build()
    }

    /// GTK 4.14 cannot update a texture in place; the whole frame is uploaded.
    #[cfg(not(feature = "gtk_4_16"))]
    fn frame_texture(_previous: Option<&gdk::Texture>, width: usize, height: usize, bytes: &glib::Bytes,
                     _damage: Option<(usize, usize, usize, usize)>) -> gdk::Texture {
        gdk::MemoryTexture::new(width as i32, height as i32, gdk::MemoryFormat::B8g8r8a8Premultiplied, bytes, width * 4).upcast()
    }

    pub fn clear(&self) {
        *self.imp().texture.borrow_mut() = None;
        *self.imp().cursor.borrow_mut() = None;
        self.queue_draw();
    }

    pub fn texture(&self) -> Option<gdk::Texture> {
        self.imp().texture.borrow().clone()
    }

    pub fn set_crop(&self, crop: Option<Crop>) {
        if self.imp().crop.get() != crop {
            self.imp().crop.set(crop);
            self.queue_resize();
            self.queue_draw();
        }
    }

    pub fn crop(&self) -> Crop {
        if let Some(crop) = self.imp().crop.get() {
            return crop;
        }
        let (width, height) = self
            .imp()
            .texture
            .borrow()
            .as_ref()
            .map(|t| (i64::from(t.width()), i64::from(t.height())))
            .unwrap_or((1, 1));
        Crop { x: 0, y: 0, width, height }
    }

    pub fn image_size(&self) -> (i64, i64) {
        let crop = self.crop();
        (crop.width, crop.height)
    }

    pub fn set_smooth(&self, smooth: bool) {
        self.imp().smooth.set(smooth);
        self.queue_draw();
    }

    pub fn set_actual_size(&self, actual: bool) {
        self.imp().actual_size.set(actual);
        self.queue_resize();
    }

    pub fn set_sensitivity(&self, sensitivity: f64) {
        self.imp().sensitivity.set(sensitivity);
    }

    pub fn set_forward_shortcuts(&self, forward: bool) {
        self.imp().forward_shortcuts.set(forward);
        self.update_shortcut_inhibit();
    }

    pub fn set_cursor_draw(&self, cursor: Option<CursorDraw>) {
        *self.imp().cursor.borrow_mut() = cursor;
        self.queue_draw();
    }

    pub fn input_enabled(&self) -> bool {
        self.imp().input_enabled.get()
    }

    pub fn set_input_enabled(&self, enabled: bool) {
        let imp = self.imp();
        let was = imp.input_enabled.replace(enabled);
        if was && !enabled {
            self.discard_composition();
            imp.raw_keys_down.borrow_mut().clear();
            self.emit(RemoteInput::ReleaseAll);
        }
        if enabled {
            self.grab_focus();
        }
        self.update_shortcut_inhibit();
    }

    fn device_scale(&self) -> f64 {
        self.native()
            .and_then(|native| native.surface())
            .map(|surface| surface.scale())
            .filter(|scale| *scale > 0.0)
            .unwrap_or_else(|| f64::from(self.scale_factor().max(1)))
    }

    /// The displayed image rectangle in widget coordinates.
    pub fn image_rect(&self) -> Option<graphene::Rect> {
        self.imp().texture.borrow().as_ref()?;
        let crop = self.crop();
        let (width, height) = (self.width() as f32, self.height() as f32);
        if width <= 0.0 || height <= 0.0 || crop.width <= 0 || crop.height <= 0 {
            return None;
        }
        let (iw, ih) = (crop.width as f32, crop.height as f32);
        let scale = if self.imp().actual_size.get() { 1.0 / self.device_scale() as f32 } else { (width / iw).min(height / ih) };
        let (w, h) = (iw * scale, ih * scale);
        let (x, y) = if self.imp().actual_size.get() { (((width - w) / 2.0).max(0.0), ((height - h) / 2.0).max(0.0)) } else { ((width - w) / 2.0, (height - h) / 2.0) };
        Some(graphene::Rect::new(x, y, w, h))
    }

    fn remote_point(&self, x: f64, y: f64) -> Option<(i64, i64)> {
        let rect = self.image_rect()?;
        let crop = self.crop();
        let (rx, ry, rw, rh) = (f64::from(rect.x()), f64::from(rect.y()), f64::from(rect.width()), f64::from(rect.height()));
        if rw <= 0.0 || rh <= 0.0 || x < rx || y < ry || x >= rx + rw || y >= ry + rh {
            return None;
        }
        let px = (((x - rx) / rw * crop.width as f64) as i64).clamp(0, crop.width - 1);
        let py = (((y - ry) / rh * crop.height as f64) as i64).clamp(0, crop.height - 1);
        Some((px, py))
    }

    fn modifiers_from(state: gdk::ModifierType) -> Modifiers {
        let mut result = Modifiers::empty();
        if state.contains(gdk::ModifierType::SHIFT_MASK) {
            result.insert(Modifiers::SHIFT);
        }
        if state.contains(gdk::ModifierType::CONTROL_MASK) {
            result.insert(Modifiers::CONTROL);
        }
        if state.contains(gdk::ModifierType::ALT_MASK) {
            result.insert(Modifiers::ALT);
        }
        if state.contains(gdk::ModifierType::SUPER_MASK) || state.contains(gdk::ModifierType::META_MASK) {
            result.insert(Modifiers::SUPER);
        }
        if state.contains(gdk::ModifierType::LOCK_MASK) {
            result.insert(Modifiers::CAPS_LOCK);
        }
        result
    }

    /// Modifier state after a modifier key event (GTK reports the state before).
    fn modifiers_after(code: u16, pressed: bool, state: gdk::ModifierType) -> Modifiers {
        let mut modifiers = Self::modifiers_from(state);
        let flag = match code {
            42 | 54 => Modifiers::SHIFT,
            29 | 97 => Modifiers::CONTROL,
            56 | 100 => Modifiers::ALT,
            125 | 126 => Modifiers::SUPER,
            _ => return modifiers,
        };
        if pressed {
            modifiers.insert(flag);
        } else {
            modifiers.remove(flag);
        }
        modifiers
    }

    fn send_raw_key(&self, code: u16, characters: Option<String>, pressed: bool, modifiers: Modifiers) {
        if pressed {
            self.imp().raw_keys_down.borrow_mut().insert(code);
        } else {
            self.imp().raw_keys_down.borrow_mut().remove(&code);
        }
        self.emit(RemoteInput::Key { code, characters, pressed, modifiers });
    }

    fn discard_composition(&self) {
        let imp = self.imp();
        if !imp.preedit.borrow().is_empty() {
            imp.preedit.borrow_mut().clear();
            if let Some(im) = imp.im.borrow().as_ref() {
                im.reset();
            }
            self.queue_draw();
        }
    }

    fn release_local(&self) {
        let imp = self.imp();
        if imp.input_enabled.get() {
            self.discard_composition();
            imp.raw_keys_down.borrow_mut().clear();
            self.emit(RemoteInput::ReleaseAll);
        }
    }

    fn toplevel(&self) -> Option<gdk::Toplevel> {
        self.native()?.surface()?.downcast::<gdk::Toplevel>().ok()
    }

    /// While controlling with focus, ask the compositor to deliver system
    /// shortcuts (Super, Alt+Tab…) to this window so they reach the peer.
    pub fn update_shortcut_inhibit(&self) {
        let imp = self.imp();
        let wanted = imp.input_enabled.get() && imp.forward_shortcuts.get() && self.has_focus();
        if wanted == imp.shortcuts_inhibited.get() {
            return;
        }
        let Some(toplevel) = self.toplevel() else { return };
        if wanted {
            toplevel.inhibit_system_shortcuts(None::<&gdk::Event>);
        } else {
            toplevel.restore_system_shortcuts();
        }
        imp.shortcuts_inhibited.set(wanted);
    }

    fn restore_shortcuts(&self) {
        if self.imp().shortcuts_inhibited.replace(false) {
            if let Some(toplevel) = self.toplevel() {
                toplevel.restore_system_shortcuts();
            }
        }
    }

    fn setup_controllers(&self) {
        // Pointer motion.
        let motion = gtk::EventControllerMotion::new();
        let widget = self.downgrade();
        motion.connect_motion(move |_, x, y| {
            let Some(widget) = widget.upgrade() else { return };
            widget.imp().pointer.set(Some((x, y)));
            if widget.input_enabled() {
                if let Some((px, py)) = widget.remote_point(x, y) {
                    widget.emit(RemoteInput::PointerMoved { x: px, y: py });
                }
            }
        });
        let widget = self.downgrade();
        motion.connect_enter(move |_, x, y| {
            if let Some(widget) = widget.upgrade() {
                widget.imp().pointer.set(Some((x, y)));
            }
        });
        self.add_controller(motion);

        // Buttons: a legacy controller sees every button, including chords.
        let buttons = gtk::EventControllerLegacy::new();
        let widget = self.downgrade();
        buttons.connect_event(move |_, event| {
            let Some(widget) = widget.upgrade() else { return glib::Propagation::Proceed };
            let pressed = match event.event_type() {
                gdk::EventType::ButtonPress => true,
                gdk::EventType::ButtonRelease => false,
                _ => return glib::Propagation::Proceed,
            };
            if !widget.input_enabled() {
                return glib::Propagation::Proceed;
            }
            let Some(button_event) = event.downcast_ref::<gdk::ButtonEvent>() else { return glib::Propagation::Proceed };
            let button = match button_event.button() {
                1 => 0,
                3 => 1,
                2 => 2,
                _ => return glib::Propagation::Stop,
            };
            if pressed {
                widget.grab_focus();
            }
            let position = widget.event_position(event).or_else(|| widget.imp().pointer.get());
            match position.and_then(|(x, y)| widget.remote_point(x, y)) {
                Some((x, y)) => widget.emit(RemoteInput::PointerButton { button, pressed, x, y }),
                None => {
                    if !pressed {
                        widget.emit(RemoteInput::ReleaseAll);
                    }
                }
            }
            glib::Propagation::Stop
        });
        self.add_controller(buttons);

        // Scrolling.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let widget = self.downgrade();
        scroll.connect_scroll(move |controller, _dx, dy| {
            let Some(widget) = widget.upgrade() else { return glib::Propagation::Proceed };
            if !widget.input_enabled() {
                return glib::Propagation::Proceed;
            }
            let inside = widget.imp().pointer.get().and_then(|(x, y)| widget.remote_point(x, y)).is_some();
            if !inside {
                return glib::Propagation::Proceed;
            }
            let precise = controller.unit() == gdk::ScrollUnit::Surface;
            let detents = scroll_detents(-dy, precise, widget.imp().sensitivity.get());
            widget.emit(RemoteInput::Scroll { delta_y: detents });
            glib::Propagation::Stop
        });
        self.add_controller(scroll);

        // Input method.
        let im = gtk::IMMulticontext::new();
        im.set_client_widget(Some(self));
        im.set_use_preedit(true);
        let widget = self.downgrade();
        im.connect_commit(move |_, text| {
            let Some(widget) = widget.upgrade() else { return };
            let imp = widget.imp();
            imp.preedit.borrow_mut().clear();
            imp.consumed.set(true);
            widget.queue_draw();
            if !widget.input_enabled() || text.is_empty() {
                return;
            }
            // A key that produced its own ASCII character keeps the VK/scan
            // path, so plain typing behaves exactly like a key press.
            let in_flight = imp.key_in_flight.borrow().clone();
            if let Some((code, characters, modifiers)) = in_flight {
                let single_ascii = text.len() == 1 && (0x20..=0x7e).contains(&text.as_bytes()[0]);
                if single_ascii && characters.as_deref() == Some(text) {
                    widget.send_raw_key(code, characters, true, modifiers);
                    return;
                }
            }
            widget.emit(RemoteInput::Text(text.to_string()));
        });
        let widget = self.downgrade();
        im.connect_preedit_changed(move |im| {
            let Some(widget) = widget.upgrade() else { return };
            let (text, _, _) = im.preedit_string();
            *widget.imp().preedit.borrow_mut() = text.to_string();
            widget.imp().consumed.set(true);
            widget.queue_draw();
        });
        *self.imp().im.borrow_mut() = Some(im.clone());

        // Keyboard.
        let keys = gtk::EventControllerKey::new();
        let widget = self.downgrade();
        keys.connect_key_pressed(move |controller, keyval, keycode, state| {
            let Some(widget) = widget.upgrade() else { return glib::Propagation::Proceed };
            if !widget.input_enabled() {
                return glib::Propagation::Proceed;
            }
            let code = key_code(keyval, keycode);
            let imp = widget.imp();
            if is_modifier_key(code) {
                widget.emit(RemoteInput::Modifiers(Self::modifiers_after(code, true, state)));
                return glib::Propagation::Stop;
            }
            let modifiers = Self::modifiers_from(state);
            let characters = keyval.to_unicode().map(|c| c.to_string());
            let shortcut = modifiers.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER);
            // Shortcuts keep the physical-key path. Other keys go through the
            // input method first so Vietnamese (Telex/VNI) and other IMEs work.
            if !shortcut {
                if let (Some(event), Some(im)) = (controller.current_event(), imp.im.borrow().clone()) {
                    *imp.key_in_flight.borrow_mut() = Some((code, characters.clone(), modifiers));
                    imp.consumed.set(false);
                    let handled = im.filter_keypress(&event);
                    *imp.key_in_flight.borrow_mut() = None;
                    if handled || imp.consumed.get() {
                        return glib::Propagation::Stop;
                    }
                }
            }
            widget.send_raw_key(code, characters, true, modifiers);
            glib::Propagation::Stop
        });
        let widget = self.downgrade();
        keys.connect_key_released(move |controller, keyval, keycode, state| {
            let Some(widget) = widget.upgrade() else { return };
            if !widget.input_enabled() {
                return;
            }
            let code = key_code(keyval, keycode);
            if is_modifier_key(code) {
                widget.emit(RemoteInput::Modifiers(Self::modifiers_after(code, false, state)));
                return;
            }
            if let (Some(event), Some(im)) = (controller.current_event(), widget.imp().im.borrow().clone()) {
                im.filter_keypress(&event);
            }
            if widget.imp().raw_keys_down.borrow().contains(&code) {
                let characters = keyval.to_unicode().map(|c| c.to_string());
                widget.send_raw_key(code, characters, false, Self::modifiers_from(state));
            }
        });
        // Modifier state changes without key events (virtual keyboards,
        // latched modifiers) still reach the peer.
        let widget = self.downgrade();
        keys.connect_modifiers(move |_, state| {
            if let Some(widget) = widget.upgrade() {
                if widget.input_enabled() {
                    widget.emit(RemoteInput::Modifiers(Self::modifiers_from(state)));
                }
            }
            glib::Propagation::Proceed
        });
        self.add_controller(keys);

        // Focus.
        let focus = gtk::EventControllerFocus::new();
        let widget = self.downgrade();
        focus.connect_enter(move |_| {
            if let Some(widget) = widget.upgrade() {
                if let Some(im) = widget.imp().im.borrow().as_ref() {
                    im.focus_in();
                }
                widget.update_shortcut_inhibit();
            }
        });
        let widget = self.downgrade();
        focus.connect_leave(move |_| {
            if let Some(widget) = widget.upgrade() {
                if let Some(im) = widget.imp().im.borrow().as_ref() {
                    im.focus_out();
                }
                widget.release_local();
                widget.update_shortcut_inhibit();
            }
        });
        self.add_controller(focus);
    }

    /// Converts an event's surface position into widget coordinates.
    fn event_position(&self, event: &gdk::Event) -> Option<(f64, f64)> {
        let (x, y) = event.position()?;
        let native = self.native()?;
        let (nx, ny) = native.surface_transform();
        let point = native.compute_point(self, &graphene::Point::new((x - nx) as f32, (y - ny) as f32))?;
        Some((f64::from(point.x()), f64::from(point.y())))
    }
}
