//! Region selection and annotation for a captured remote frame.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{cairo, gio};

use super::capture::{self, Capture};
use crate::i18n::{tr, trf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Region,
    Pen,
    Rectangle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ink {
    Red,
    Yellow,
    Blue,
}

impl Ink {
    fn rgb(self) -> (f64, f64, f64) {
        match self {
            Ink::Red => (1.0, 0.23, 0.19),
            Ink::Yellow => (1.0, 0.8, 0.0),
            Ink::Blue => (0.0, 0.48, 1.0),
        }
    }
}

struct Mark {
    tool: Tool,
    ink: Ink,
    points: Vec<(f64, f64)>,
}

struct Model {
    capture: Capture,
    surface: cairo::ImageSurface,
    tool: Tool,
    ink: Ink,
    /// (x, y, width, height) in image pixels.
    selection: Option<(f64, f64, f64, f64)>,
    marks: Vec<Mark>,
    origin: Option<(f64, f64)>,
}

impl Model {
    fn stroke_width(&self) -> f64 {
        (self.capture.width.min(self.capture.height) as f64 / 350.0).max(2.0)
    }

    fn begin(&mut self, point: (f64, f64)) {
        self.origin = Some(point);
        if self.tool == Tool::Region {
            self.selection = Some((point.0, point.1, 0.0, 0.0));
        } else {
            if self.marks.len() >= 200 {
                self.marks.remove(0);
            }
            self.marks.push(Mark { tool: self.tool, ink: self.ink, points: vec![point] });
        }
    }

    fn drag(&mut self, point: (f64, f64)) {
        let Some(origin) = self.origin else { return };
        if self.tool == Tool::Region {
            self.selection = Some((origin.0.min(point.0), origin.1.min(point.1), (point.0 - origin.0).abs(), (point.1 - origin.1).abs()));
        } else if let Some(last) = self.marks.last_mut() {
            if self.tool == Tool::Rectangle {
                last.points = vec![origin, point];
            } else if last.points.len() < 50_000 {
                last.points.push(point);
            }
        }
    }

    /// The selection clamped to the image, in whole pixels.
    fn region(&self) -> Option<(i32, i32, i32, i32)> {
        let (x, y, w, h) = self.selection?;
        let (iw, ih) = (self.capture.width as f64, self.capture.height as f64);
        let left = x.floor().clamp(0.0, iw);
        let top = y.floor().clamp(0.0, ih);
        let right = (x + w).ceil().clamp(0.0, iw);
        let bottom = (y + h).ceil().clamp(0.0, ih);
        let (w, h) = (right - left, bottom - top);
        (w >= 2.0 && h >= 2.0).then_some((left as i32, top as i32, w as i32, h as i32))
    }

    fn draw_marks(&self, context: &cairo::Context) {
        let width = self.stroke_width();
        context.set_line_width(width);
        context.set_line_cap(cairo::LineCap::Round);
        context.set_line_join(cairo::LineJoin::Round);
        for mark in &self.marks {
            let (r, g, b) = mark.ink.rgb();
            context.set_source_rgb(r, g, b);
            let Some(&first) = mark.points.first() else { continue };
            if mark.tool == Tool::Rectangle {
                let last = *mark.points.last().unwrap_or(&first);
                context.rectangle(first.0.min(last.0), first.1.min(last.1), (last.0 - first.0).abs(), (last.1 - first.1).abs());
                let _ = context.stroke();
            } else if mark.points.len() == 1 {
                context.arc(first.0, first.1, width / 2.0, 0.0, std::f64::consts::TAU);
                let _ = context.fill();
            } else {
                context.move_to(first.0, first.1);
                for point in &mark.points[1..] {
                    context.line_to(point.0, point.1);
                }
                let _ = context.stroke();
            }
        }
    }

    /// Renders the selected region with its annotations.
    fn export(&self) -> Option<Capture> {
        let (x, y, w, h) = self.region()?;
        let output = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).ok()?;
        {
            let context = cairo::Context::new(&output).ok()?;
            context.set_source_surface(&self.surface, -f64::from(x), -f64::from(y)).ok()?;
            context.paint().ok()?;
            context.translate(-f64::from(x), -f64::from(y));
            self.draw_marks(&context);
        }
        output.flush();
        let stride = output.stride() as usize;
        let data = output.take_data().ok()?;
        let (w, h) = (w as usize, h as usize);
        let mut pixels = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            pixels.extend_from_slice(&data[row * stride..row * stride + w * 4]);
        }
        Some(Capture { width: w, height: h, pixels: std::sync::Arc::new(pixels) })
    }
}

/// Opens the editor. `select_region` starts with the region tool and no
/// selection; otherwise the full image is selected and the pen is active.
pub fn open(parent: &impl IsA<gtk::Window>, capture: Capture, peer: &str, select_region: bool, toast: Rc<dyn Fn(String)>) {
    let mut data = vec![0u8; capture.width * capture.height * 4];
    data.copy_from_slice(&capture.pixels[..capture.width * capture.height * 4]);
    let Ok(surface) = cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, capture.width as i32, capture.height as i32, (capture.width * 4) as i32) else {
        return;
    };
    let model = Rc::new(RefCell::new(Model {
        selection: (!select_region).then_some((0.0, 0.0, capture.width as f64, capture.height as f64)),
        capture,
        surface,
        tool: if select_region { Tool::Region } else { Tool::Pen },
        ink: Ink::Red,
        marks: Vec::new(),
        origin: None,
    }));

    let title = if select_region { trf("Capture a region — {}", &[peer]) } else { trf("Annotate capture — {}", &[peer]) };
    let window = adw::Window::builder().title(&title).transient_for(parent).modal(true).default_width(1000).default_height(720).build();
    // Same application identity as the main window (desktop grouping/icon).
    window.set_application(parent.as_ref().application().as_ref());
    let header = adw::HeaderBar::new();

    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tools.add_css_class("linked");
    let region_button = gtk::ToggleButton::builder().icon_name("edit-select-all-symbolic").tooltip_text(tr("Select region")).build();
    let pen_button = gtk::ToggleButton::builder().icon_name("document-edit-symbolic").tooltip_text(tr("Draw")).group(&region_button).build();
    let rectangle_button = gtk::ToggleButton::builder().icon_name("checkbox-symbolic").tooltip_text(tr("Rectangle")).group(&region_button).build();
    tools.append(&region_button);
    tools.append(&pen_button);
    tools.append(&rectangle_button);
    header.pack_start(&tools);

    let inks = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    inks.add_css_class("linked");
    let mut ink_buttons = Vec::new();
    for (ink, name) in [(Ink::Red, "Red"), (Ink::Yellow, "Yellow"), (Ink::Blue, "Blue")] {
        let swatch = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        swatch.set_size_request(16, 16);
        swatch.add_css_class("ink-swatch");
        swatch.add_css_class(match ink {
            Ink::Red => "ink-red",
            Ink::Yellow => "ink-yellow",
            Ink::Blue => "ink-blue",
        });
        let button = gtk::ToggleButton::builder().child(&swatch).tooltip_text(tr(name)).build();
        if let Some(first) = ink_buttons.first() {
            button.set_group(Some(first));
        }
        let model = model.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                model.borrow_mut().ink = ink;
            }
        });
        inks.append(&button);
        ink_buttons.push(button);
    }
    ink_buttons[0].set_active(true);
    header.pack_start(&inks);

    let full = gtk::Button::with_label(&tr("Full image"));
    let undo = gtk::Button::from_icon_name("edit-undo-symbolic");
    undo.set_tooltip_text(Some(&tr("Undo drawing")));
    header.pack_start(&full);
    header.pack_start(&undo);
    let save = gtk::Button::with_label(&tr("Save PNG…"));
    save.add_css_class("suggested-action");
    let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
    copy.set_tooltip_text(Some(&tr("Copy screenshot")));
    header.pack_end(&save);
    header.pack_end(&copy);

    let canvas = gtk::DrawingArea::new();
    canvas.set_hexpand(true);
    canvas.set_vexpand(true);
    canvas.set_cursor_from_name(Some("crosshair"));
    let hint = gtk::Label::new(Some(&tr("Drag over the image to select a region.")));
    hint.add_css_class("dim-label");
    hint.set_margin_top(6);
    hint.set_margin_bottom(6);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&canvas);
    content.append(&hint);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&content));
    window.set_content(Some(&view));

    // Image placement: scale-to-fit, centered.
    let layout = |model: &Model, width: f64, height: f64| -> (f64, f64, f64) {
        let (iw, ih) = (model.capture.width as f64, model.capture.height as f64);
        let scale = (width / iw).min(height / ih).min(1.0).max(0.01);
        ((width - iw * scale) / 2.0, (height - ih * scale) / 2.0, scale)
    };
    let draw_model = model.clone();
    canvas.set_draw_func(move |_, context, width, height| {
        let model = draw_model.borrow();
        context.set_source_rgb(0.12, 0.12, 0.13);
        let _ = context.paint();
        let (ox, oy, scale) = layout(&model, f64::from(width), f64::from(height));
        context.save().ok();
        context.translate(ox, oy);
        context.scale(scale, scale);
        let _ = context.set_source_surface(&model.surface, 0.0, 0.0);
        context.source().set_filter(cairo::Filter::Good);
        let _ = context.paint();
        model.draw_marks(context);
        let (iw, ih) = (model.capture.width as f64, model.capture.height as f64);
        if let Some((x, y, w, h)) = model.selection {
            // Dim everything outside the selection.
            context.set_fill_rule(cairo::FillRule::EvenOdd);
            context.rectangle(0.0, 0.0, iw, ih);
            context.rectangle(x, y, w, h);
            context.set_source_rgba(0.0, 0.0, 0.0, 0.45);
            let _ = context.fill();
            context.set_line_width(2.0 / scale);
            context.set_source_rgb(1.0, 1.0, 1.0);
            context.rectangle(x, y, w, h);
            let _ = context.stroke();
        }
        context.restore().ok();
    });

    let to_image = {
        let canvas = canvas.clone();
        let model = model.clone();
        move |x: f64, y: f64| -> (f64, f64) {
            let model = model.borrow();
            let (ox, oy, scale) = layout(&model, f64::from(canvas.width()), f64::from(canvas.height()));
            let px = ((x - ox) / scale).clamp(0.0, model.capture.width as f64);
            let py = ((y - oy) / scale).clamp(0.0, model.capture.height as f64);
            (px, py)
        }
    };
    let refresh = {
        let canvas = canvas.clone();
        let model = model.clone();
        let undo = undo.clone();
        let save = save.clone();
        let copy = copy.clone();
        let hint = hint.clone();
        Rc::new(move || {
            let model = model.borrow();
            undo.set_sensitive(!model.marks.is_empty());
            let ready = model.region().is_some();
            save.set_sensitive(ready);
            copy.set_sensitive(ready);
            hint.set_visible(model.tool == Tool::Region);
            canvas.queue_draw();
        })
    };

    let drag = gtk::GestureDrag::new();
    let start = Rc::new(RefCell::new((0.0, 0.0)));
    {
        let (model, to_image, refresh, start) = (model.clone(), to_image.clone(), refresh.clone(), start.clone());
        drag.connect_drag_begin(move |_, x, y| {
            *start.borrow_mut() = (x, y);
            let point = to_image(x, y);
            model.borrow_mut().begin(point);
            refresh();
        });
    }
    {
        let (model, to_image, refresh, start) = (model.clone(), to_image.clone(), refresh.clone(), start.clone());
        drag.connect_drag_update(move |_, dx, dy| {
            let (x, y) = *start.borrow();
            let point = to_image(x + dx, y + dy);
            model.borrow_mut().drag(point);
            refresh();
        });
    }
    {
        let (model, refresh) = (model.clone(), refresh.clone());
        drag.connect_drag_end(move |_, _, _| {
            model.borrow_mut().origin = None;
            refresh();
        });
    }
    canvas.add_controller(drag);

    for (button, tool) in [(&region_button, Tool::Region), (&pen_button, Tool::Pen), (&rectangle_button, Tool::Rectangle)] {
        let (model, refresh) = (model.clone(), refresh.clone());
        button.connect_toggled(move |button| {
            if button.is_active() {
                model.borrow_mut().tool = tool;
                refresh();
            }
        });
    }
    let initial_tool = model.borrow().tool;
    match initial_tool {
        Tool::Region => region_button.set_active(true),
        _ => pen_button.set_active(true),
    }
    {
        let (model, refresh) = (model.clone(), refresh.clone());
        full.connect_clicked(move |_| {
            let mut model = model.borrow_mut();
            model.selection = Some((0.0, 0.0, model.capture.width as f64, model.capture.height as f64));
            drop(model);
            refresh();
        });
    }
    {
        let (model, refresh) = (model.clone(), refresh.clone());
        undo.connect_clicked(move |_| {
            model.borrow_mut().marks.pop();
            refresh();
        });
    }
    {
        let (model, toast) = (model.clone(), toast.clone());
        copy.connect_clicked(move |_| match model.borrow().export() {
            Some(image) => {
                capture::copy_to_clipboard(&image);
                toast(tr("Screenshot copied"));
            }
            None => toast(tr("Select a region at least 2 × 2 pixels.")),
        });
    }
    {
        let (model, window_ref, peer) = (model.clone(), window.clone(), peer.to_string());
        save.connect_clicked(move |_| {
            let Some(image) = model.borrow().export() else {
                toast(tr("Select a region at least 2 × 2 pixels."));
                return;
            };
            let toast = toast.clone();
            let window = window_ref.clone();
            let dialog = gtk::FileDialog::builder().title(tr("Save PNG…")).initial_name(capture::default_file_name(&peer, "png")).build();
            let folder = capture::capture_folder();
            let _ = std::fs::create_dir_all(&folder);
            dialog.set_initial_folder(Some(&gio::File::for_path(&folder)));
            dialog.save(Some(&window_ref), gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                let Some(path) = file.path() else { return };
                match image.texture().save_to_png(&path) {
                    Ok(()) => {
                        toast(trf("Screenshot saved to {}", &[&path.display().to_string()]));
                        window.close();
                    }
                    Err(error) => toast(format!("{}: {error}", tr("Capture failed"))),
                }
            });
        });
    }
    refresh();
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(width: usize, height: usize) -> Model {
        let data = vec![255u8; width * height * 4]; // opaque white
        let surface = cairo::ImageSurface::create_for_data(data.clone(), cairo::Format::ARgb32, width as i32, height as i32, (width * 4) as i32).unwrap();
        Model {
            capture: Capture { width, height, pixels: std::sync::Arc::new(data) },
            surface,
            tool: Tool::Region,
            ink: Ink::Red,
            selection: None,
            marks: Vec::new(),
            origin: None,
        }
    }

    #[test]
    fn region_selection_crops_and_annotations_are_burned_in() {
        let mut m = model(100, 80);
        assert!(m.export().is_none());
        m.begin((60.4, 50.0));
        m.drag((10.0, 20.6));
        m.origin = None;
        assert_eq!(m.region(), Some((10, 20, 51, 30)));
        m.tool = Tool::Rectangle;
        m.begin((12.0, 22.0));
        m.drag((40.0, 45.0));
        let image = m.export().unwrap();
        assert_eq!((image.width, image.height), (51, 30));
        // The rectangle's top edge at image y=22 → exported row 2 is red.
        let pixel = &image.pixels[(2 * 51 + 15) * 4..(2 * 51 + 15) * 4 + 4];
        assert!(pixel[2] > 200 && pixel[1] < 120 && pixel[0] < 120, "{pixel:?}");
        // Inside the rectangle stays white.
        let inside = &image.pixels[(12 * 51 + 15) * 4..(12 * 51 + 15) * 4 + 4];
        assert_eq!(inside, &[255, 255, 255, 255]);
        m.marks.pop();
        assert!(m.marks.is_empty());
    }
}
