//! Plays received computer audio only (48 kHz signed16LE interleaved stereo)
//! through PulseAudio/PipeWire. Never records a microphone.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::JoinHandle;

#[repr(C)]
struct SampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

#[repr(C)]
struct BufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

enum Simple {}

const PA_STREAM_PLAYBACK: c_int = 1;
const PA_SAMPLE_S16LE: c_int = 3;

#[link(name = "pulse-simple")]
#[link(name = "pulse")]
extern "C" {
    fn pa_simple_new(server: *const c_char, name: *const c_char, direction: c_int, device: *const c_char,
                     stream_name: *const c_char, spec: *const SampleSpec, map: *const c_void, attr: *const BufferAttr,
                     error: *mut c_int) -> *mut Simple;
    fn pa_simple_write(simple: *mut Simple, data: *const c_void, bytes: usize, error: *mut c_int) -> c_int;
    fn pa_simple_flush(simple: *mut Simple, error: *mut c_int) -> c_int;
    fn pa_simple_free(simple: *mut Simple);
    fn pa_strerror(error: c_int) -> *const c_char;
}

/// 250 ms at the source rate; older audio is dropped instead of delaying.
const MAXIMUM_QUEUED_PACKETS: usize = 24;

enum Message {
    Play(Vec<u8>),
    Stop,
}

pub struct AudioPlayer {
    sender: Option<SyncSender<Message>>,
    worker: Option<JoinHandle<()>>,
    errors: Option<Receiver<String>>,
}

impl AudioPlayer {
    pub fn new() -> Self {
        Self { sender: None, worker: None, errors: None }
    }

    /// Opens the output stream. Errors are user-facing text.
    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), String> {
        if !enabled {
            self.stop();
            return Ok(());
        }
        if self.sender.is_some() {
            return Ok(());
        }
        let (sender, receiver) = mpsc::sync_channel::<Message>(MAXIMUM_QUEUED_PACKETS);
        let (error_sender, errors) = mpsc::channel::<String>();
        let (ready_sender, ready) = mpsc::channel::<Result<(), String>>();
        let worker = std::thread::Builder::new()
            .name("linuxviewer-audio".into())
            .spawn(move || playback(receiver, ready_sender, error_sender))
            .map_err(|e| e.to_string())?;
        match ready.recv() {
            Ok(Ok(())) => {
                self.sender = Some(sender);
                self.worker = Some(worker);
                self.errors = Some(errors);
                Ok(())
            }
            Ok(Err(message)) => {
                let _ = worker.join();
                Err(message)
            }
            Err(_) => Err("audio worker failed".into()),
        }
    }

    /// Queues PCM; a full queue drops the stale packet to stay live.
    pub fn play(&mut self, pcm: Vec<u8>) -> Result<(), String> {
        if let Some(errors) = &self.errors {
            if let Ok(message) = errors.try_recv() {
                self.stop();
                return Err(message);
            }
        }
        let Some(sender) = &self.sender else { return Ok(()) };
        if pcm.is_empty() || pcm.len() % 4 != 0 {
            return Ok(());
        }
        match sender.try_send(Message::Play(pcm)) {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => {
                self.stop();
                Err("audio output closed".into())
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.try_send(Message::Stop);
        }
        self.errors = None;
        if let Some(worker) = self.worker.take() {
            // The worker exits once the channel is dropped; do not block the UI.
            std::thread::spawn(move || {
                let _ = worker.join();
            });
        }
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn pulse_error(code: c_int) -> String {
    // SAFETY: pa_strerror returns a static string.
    unsafe {
        let text = pa_strerror(code);
        if text.is_null() {
            format!("error {code}")
        } else {
            std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned()
        }
    }
}

fn playback(receiver: Receiver<Message>, ready: mpsc::Sender<Result<(), String>>, errors: mpsc::Sender<String>) {
    let name = CString::new("LinuxViewer").unwrap();
    let stream = CString::new("Remote computer audio").unwrap();
    let spec = SampleSpec { format: PA_SAMPLE_S16LE, rate: 48_000, channels: 2 };
    // ~100 ms target latency.
    let attr = BufferAttr { maxlength: u32::MAX, tlength: 48_000 * 4 / 10, prebuf: u32::MAX, minreq: u32::MAX, fragsize: u32::MAX };
    let mut error = 0;
    // SAFETY: all pointers are valid for the call; the handle is freed below.
    let simple = unsafe {
        pa_simple_new(std::ptr::null(), name.as_ptr(), PA_STREAM_PLAYBACK, std::ptr::null(), stream.as_ptr(), &spec,
                      std::ptr::null(), &attr, &mut error)
    };
    if simple.is_null() {
        let _ = ready.send(Err(pulse_error(error)));
        return;
    }
    let _ = ready.send(Ok(()));
    while let Ok(message) = receiver.recv() {
        match message {
            Message::Play(pcm) => {
                // SAFETY: buffer valid for its length.
                if unsafe { pa_simple_write(simple, pcm.as_ptr().cast(), pcm.len(), &mut error) } < 0 {
                    let _ = errors.send(pulse_error(error));
                    break;
                }
            }
            Message::Stop => {
                // SAFETY: valid handle.
                unsafe { pa_simple_flush(simple, &mut error) };
                break;
            }
        }
    }
    // SAFETY: created above and freed once.
    unsafe { pa_simple_free(simple) };
}
