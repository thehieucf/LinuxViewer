//! Screenshots and screen recording of the decoded remote frames only.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::i18n::tr;
use crate::settings;

/// Pixels of the current frame, cropped to the selected monitor.
#[derive(Clone)]
pub struct Capture {
    pub width: usize,
    pub height: usize,
    /// Tightly packed opaque BGRA.
    pub pixels: Arc<Vec<u8>>,
}

impl Capture {
    pub fn from_frame(width: usize, height: usize, bgra: &Arc<Vec<u8>>, crop: Option<(i64, i64, i64, i64)>) -> Self {
        match crop {
            Some((x, y, w, h)) if w > 0 && h > 0 && (x + w) as usize <= width && (y + h) as usize <= height => {
                let (x, y, w, h) = (x as usize, y as usize, w as usize, h as usize);
                let mut pixels = Vec::with_capacity(w * h * 4);
                for row in y..y + h {
                    pixels.extend_from_slice(&bgra[(row * width + x) * 4..(row * width + x + w) * 4]);
                }
                Self { width: w, height: h, pixels: Arc::new(pixels) }
            }
            _ => Self { width, height, pixels: bgra.clone() },
        }
    }

    pub fn texture(&self) -> gdk::Texture {
        let bytes = glib::Bytes::from(&self.pixels[..]);
        gdk::MemoryTexture::new(self.width as i32, self.height as i32, gdk::MemoryFormat::B8g8r8a8Premultiplied, &bytes, self.width * 4).upcast()
    }
}

pub fn capture_folder() -> PathBuf {
    if let Some(folder) = settings::get().capture_folder {
        return PathBuf::from(folder);
    }
    let pictures = glib::user_special_dir(glib::UserDirectory::Pictures).unwrap_or_else(glib::home_dir);
    pictures.join("LinuxViewer")
}

fn sanitized(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
    cleaned.trim().chars().take(40).collect()
}

pub fn default_file_name(peer: &str, extension: &str) -> String {
    let stamp = glib::DateTime::now_local().ok().and_then(|t| t.format("%Y-%m-%d %H.%M.%S").ok()).map(|s| s.to_string()).unwrap_or_default();
    format!("LinuxViewer {} {stamp}.{extension}", sanitized(peer))
}

/// Saves a PNG into the capture folder and returns its path.
pub fn save_png(capture: &Capture, peer: &str) -> Result<PathBuf, String> {
    let folder = capture_folder();
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let path = folder.join(default_file_name(peer, "png"));
    capture.texture().save_to_png(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn save_png_as(capture: Capture, peer: String, parent: &impl IsA<gtk::Window>, done: impl Fn(Result<PathBuf, String>) + 'static) {
    let dialog = gtk::FileDialog::builder().title(tr("Save screenshot as PNG…")).initial_name(default_file_name(&peer, "png")).build();
    let folder = capture_folder();
    let _ = std::fs::create_dir_all(&folder);
    dialog.set_initial_folder(Some(&gio::File::for_path(&folder)));
    dialog.save(Some(parent), gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else { return };
        let Some(path) = file.path() else { return };
        done(capture.texture().save_to_png(&path).map(|_| path).map_err(|e| e.to_string()));
    });
}

pub fn copy_to_clipboard(capture: &Capture) {
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_texture(&capture.texture());
    }
}

pub fn open_capture_folder() {
    let folder = capture_folder();
    let _ = std::fs::create_dir_all(&folder);
    let uri = gio::File::for_path(&folder).uri();
    let _ = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE);
}

pub fn ffmpeg_available() -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join("ffmpeg").is_file()))
        .unwrap_or(false)
}

/// Silent H.264 recording through an ffmpeg child process, up to
/// 1920 × 1080 at 15 frames per second.
pub struct Recorder {
    pub path: PathBuf,
    pub width: usize,
    pub height: usize,
    sender: Option<SyncSender<Arc<Vec<u8>>>>,
    child: Option<Child>,
    writer: Option<std::thread::JoinHandle<()>>,
    pub started: std::time::Instant,
}

impl Recorder {
    pub fn start(width: usize, height: usize, peer: &str) -> Result<Self, String> {
        if !ffmpeg_available() {
            return Err(tr("Recording needs ffmpeg. Install the ffmpeg package and try again."));
        }
        let folder = capture_folder();
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let path = folder.join(default_file_name(peer, "mp4"));
        let scale = (1920.0 / width as f64).min(1080.0 / height as f64).min(1.0);
        let out_w = ((width as f64 * scale) as usize / 2 * 2).max(2);
        let out_h = ((height as f64 * scale) as usize / 2 * 2).max(2);
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgra", "-s"])
            .arg(format!("{width}x{height}"))
            .args(["-r", "15", "-i", "-", "-an", "-vf"])
            .arg(format!("scale={out_w}:{out_h}"))
            .args(["-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p", "-movflags", "+faststart"])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take().ok_or("ffmpeg stdin unavailable")?;
        let (sender, receiver) = sync_channel::<Arc<Vec<u8>>>(30);
        let writer = std::thread::spawn(move || {
            while let Ok(frame) = receiver.recv() {
                if stdin.write_all(&frame).is_err() {
                    break;
                }
            }
        });
        Ok(Self { path, width, height, sender: Some(sender), child: Some(child), writer: Some(writer), started: std::time::Instant::now() })
    }

    /// Adds one frame; frames of a different size end the recording.
    pub fn push(&self, width: usize, height: usize, pixels: Arc<Vec<u8>>) -> bool {
        if width != self.width || height != self.height {
            return false;
        }
        if let Some(sender) = &self.sender {
            match sender.try_send(pixels) {
                Ok(()) | Err(TrySendError::Full(_)) => return true,
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        false
    }

    /// Finishes the file in the background and reports the result.
    pub fn finish(mut self, done: impl FnOnce(Result<PathBuf, String>) + 'static) {
        self.sender = None;
        let writer = self.writer.take();
        let child = self.child.take();
        let path = self.path.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            let status = child.map(|mut c| c.wait());
            let _ = tx.send(match status {
                Some(Ok(status)) if status.success() => Ok(path),
                Some(Ok(status)) => Err(format!("ffmpeg exited with {status}")),
                Some(Err(e)) => Err(e.to_string()),
                None => Err("ffmpeg did not start".into()),
            });
        });
        let mut done = Some(done);
        glib::timeout_add_local(std::time::Duration::from_millis(200), move || match rx.try_recv() {
            Ok(result) => {
                if let Some(done) = done.take() {
                    done(result);
                }
                glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(_) => glib::ControlFlow::Break,
        });
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.sender = None;
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}
