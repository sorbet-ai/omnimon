//! Persisted UI preferences (theme, refresh rate, sort...). Stored as JSON
//! in the platform config dir under the app's name. Command-line flags win.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::brand::APP_NAME;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: Option<String>,
    pub interval: Option<f64>,
    pub sort: Option<crate::ui::SortKey>,
    pub sort_desc: Option<bool>,
    pub full_cmd: Option<bool>,
    pub chart_style: Option<crate::ui::ChartStyle>,
    /// Overview panels to hide: "gpus", "host", "procs".
    pub hidden: Vec<String>,
    /// Key overrides per action, e.g. {"kill": ["K", "f9"]}. Hand-edited;
    /// preserved across saves.
    pub keys: std::collections::HashMap<crate::ui::Action, Vec<String>>,
}

pub fn path() -> Option<PathBuf> {
    if let Some(p) = crate::brand::env("CONFIG") {
        return Some(PathBuf::from(p));
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join(APP_NAME).join("config.json"))
}

/// Where recorded history lives: the platform's local state directory.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(p) = crate::brand::env("STATE_DIR") {
        return Some(PathBuf::from(p));
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
    }?;
    Some(base.join(APP_NAME))
}

pub fn load() -> Config {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Best effort; a read-only home (containers) is fine. Hand-edited key
/// bindings already on disk are kept.
pub fn save(c: &Config) {
    let mut c = c.clone();
    if c.keys.is_empty() {
        c.keys = load().keys;
    }
    let c = &c;
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(c) {
        let _ = std::fs::write(p, s);
    }
}
