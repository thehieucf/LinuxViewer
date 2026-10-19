//! Small JSON preferences file in the XDG config directory. Passwords are
//! never stored here.

use std::cell::RefCell;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// "system", "en" or "vi".
    pub language: String,
    pub remember_partners: bool,
    pub remember_password: bool,
    pub recent_partners: Vec<String>,
    pub share_clipboard: bool,
    pub smooth_scaling: bool,
    pub scroll_sensitivity: f64,
    /// Send Super/Alt+Tab style shortcuts to the remote computer while
    /// controlling it (uses the compositor's shortcut-inhibit protocol).
    pub forward_system_shortcuts: bool,
    pub show_remote_cursor: bool,
    pub enable_lan_direct: bool,
    pub chat_sidebar: bool,
    pub capture_folder: Option<String>,
    /// Lets the screen-cast portal reuse the last approved screen.
    pub portal_restore_token: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "system".into(),
            remember_partners: true,
            remember_password: true,
            recent_partners: Vec::new(),
            share_clipboard: true,
            smooth_scaling: true,
            scroll_sensitivity: 1.0,
            forward_system_shortcuts: true,
            show_remote_cursor: true,
            enable_lan_direct: true,
            chat_sidebar: false,
            capture_folder: None,
            portal_restore_token: None,
        }
    }
}

pub fn config_directory() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("linuxviewer")
}

pub fn data_directory() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("linuxviewer")
}

/// Logs live here (`$XDG_STATE_HOME/linuxviewer`).
pub fn state_directory() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("linuxviewer")
}

fn settings_path() -> PathBuf {
    config_directory().join("settings.json")
}

thread_local! {
    static SETTINGS: RefCell<Settings> = RefCell::new(load());
}

fn load() -> Settings {
    std::fs::read(settings_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .map(|mut settings| {
            settings.recent_partners.truncate(10);
            settings.scroll_sensitivity = settings.scroll_sensitivity.clamp(0.25, 2.0);
            settings
        })
        .unwrap_or_default()
}

/// Reads the current settings (main thread).
pub fn get() -> Settings {
    SETTINGS.with(|s| s.borrow().clone())
}

/// Updates and persists the settings (main thread).
pub fn update(change: impl FnOnce(&mut Settings)) {
    SETTINGS.with(|s| {
        let mut settings = s.borrow_mut();
        change(&mut settings);
        let directory = config_directory();
        if std::fs::create_dir_all(&directory).is_ok() {
            if let Ok(bytes) = serde_json::to_vec_pretty(&*settings) {
                let temporary = directory.join("settings.json.tmp");
                if std::fs::write(&temporary, bytes).is_ok() {
                    let _ = std::fs::rename(&temporary, settings_path());
                }
            }
        }
    });
}
