//! LinuxViewer: an UltraViewer-compatible remote desktop client for Linux,
//! ported from MacViewer.

mod credentials;
mod host;
mod i18n;
mod profile;
mod runtime;
mod settings;
mod tools;
mod ui;
mod viewer;

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

pub const APP_ID: &str = "io.github.linuxviewer";

static STYLE: &str = include_str!("../resources/style.css");
static ICON: &str = include_str!("../resources/io.github.linuxviewer.svg");

thread_local! {
    static MAIN: RefCell<Option<Rc<ui::window::MainWindow>>> = const { RefCell::new(None) };
}

/// Copies log records to stderr and to a file users can attach to a report.
struct LogFile(std::fs::File);

impl Write for LogFile {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buffer);
        self.0.write_all(buffer)?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Logs to stderr and `$XDG_STATE_HOME/linuxviewer/linuxviewer.log`; the
/// previous run is kept as `linuxviewer.log.old`.
fn init_logging() {
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,ultraviewer_protocol=info,linuxviewer=info"),
    );
    let directory = settings::state_directory();
    let path = directory.join("linuxviewer.log");
    let file = std::fs::create_dir_all(&directory).ok().and_then(|_| {
        let _ = std::fs::rename(&path, directory.join("linuxviewer.log.old"));
        std::os::unix::fs::OpenOptionsExt::mode(std::fs::OpenOptions::new().write(true).create(true).truncate(true), 0o600)
            .open(&path)
            .ok()
    });
    if let Some(file) = file {
        builder.target(env_logger::Target::Pipe(Box::new(LogFile(file))));
    }
    builder.init();
}

fn install_icon() {
    let base = settings::data_directory().join("icons");
    let directory = base.join("hicolor/scalable/apps");
    let path = directory.join(format!("{APP_ID}.svg"));
    if std::fs::read_to_string(&path).ok().as_deref() != Some(ICON) {
        let _ = std::fs::create_dir_all(&directory);
        let _ = std::fs::write(&path, ICON);
    }
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(&base);
    }
}

fn load_style() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(STYLE);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

/// Tells connected computers before exiting, bounded by a timeout.
fn quit_gracefully(app: &adw::Application) {
    thread_local! {
        static QUITTING: Cell<bool> = const { Cell::new(false) };
    }
    if QUITTING.with(|q| q.replace(true)) {
        return;
    }
    let app = app.clone();
    let main = MAIN.with(|m| m.borrow().clone());
    glib::spawn_future_local(async move {
        if let Some(main) = main {
            let (viewer, hosting) = main.models();
            let shutdown = async {
                viewer.shutdown().await;
                hosting.shutdown().await;
            };
            let timeout = glib::timeout_future(Duration::from_secs(3));
            futures_select(shutdown, timeout).await;
        }
        app.quit();
    });
}

/// Completes when either future does.
async fn futures_select(a: impl std::future::Future<Output = ()>, b: impl std::future::Future<Output = ()>) {
    let mut a = std::pin::pin!(a);
    let mut b = std::pin::pin!(b);
    std::future::poll_fn(|cx| {
        if a.as_mut().poll(cx).is_ready() || b.as_mut().poll(cx).is_ready() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}

fn main() -> glib::ExitCode {
    init_logging();
    i18n::init(&settings::get().language);
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|app| {
        load_style();
        install_icon();
        gtk::Window::set_default_icon_name(APP_ID);
        let preferences = gio::SimpleAction::new("preferences", None);
        preferences.connect_activate(|_, _| {
            if let Some(main) = MAIN.with(|m| m.borrow().clone()) {
                ui::window::present_preferences(&main);
            }
        });
        app.add_action(&preferences);
        let about = gio::SimpleAction::new("about", None);
        about.connect_activate(|_, _| {
            if let Some(main) = MAIN.with(|m| m.borrow().clone()) {
                ui::window::present_about(&main.window);
            }
        });
        app.add_action(&about);
        let quit = gio::SimpleAction::new("quit", None);
        let a = app.clone();
        quit.connect_activate(move |_, _| quit_gracefully(&a));
        app.add_action(&quit);
    });
    app.connect_activate(|app| {
        if let Some(main) = MAIN.with(|m| m.borrow().clone()) {
            main.window.present();
            return;
        }
        let main = ui::window::MainWindow::new(app);
        let a = app.clone();
        main.window.connect_close_request(move |_| {
            quit_gracefully(&a);
            glib::Propagation::Stop
        });
        main.window.present();
        MAIN.with(|m| *m.borrow_mut() = Some(main));
    });
    app.run()
}
