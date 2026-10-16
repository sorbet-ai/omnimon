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
    pub hidden: Vec<crate::ui::Panel>,
    /// Key overrides per action, e.g. {"kill": ["K", "f9"]}. Hand-edited;
    /// preserved across saves.
    pub keys: std::collections::HashMap<crate::ui::Action, Vec<String>>,
    /// Shell command run on notable events; the event is in <APP>_EVENT,
    /// <APP>_SEVERITY, <APP>_GPU, <APP>_TIME. Hand-edited.
    pub alert_command: Option<String>,
    /// Lowest severity that triggers it: "info", "warn" or "crit" (default).
    pub alert_min: Option<crate::events::Severity>,
    /// Electricity price for cost estimates (any currency per kWh).
    pub price_per_kwh: Option<f64>,
    /// Keys this version doesn't know (from a newer one, or notes): kept on save.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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

/// The config on disk: defaults if there is none, an error if it doesn't parse.
pub fn load() -> Result<Config, String> {
    let Some(p) = path() else { return Ok(Config::default()) };
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display())),
        Err(_) => Ok(Config::default()),
    }
}

/// Change the settings the UI manages, re-reading the file first so hand
/// edits made meanwhile (and unknown keys) survive. A file that doesn't
/// parse is never overwritten. Best effort: a read-only home is fine.
pub fn update(f: impl FnOnce(&mut Config)) {
    let mut c = match load() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("settings not saved, fix the config first ({e})");
            return;
        }
    };
    f(&mut c);
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(&c) {
        let _ = std::fs::write(p, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_and_bindings_survive_a_round_trip() {
        let text = r#"{"theme":"nord","hidden":["host"],"keys":{"kill":["K"]},"from_the_future":{"a":1}}"#;
        let c: Config = serde_json::from_str(text).unwrap();
        assert_eq!(c.hidden, [crate::ui::Panel::Host]);
        assert_eq!(c.keys[&crate::ui::Action::Kill], ["K"]);
        let back = serde_json::to_value(&c).unwrap();
        assert_eq!(back["from_the_future"]["a"], 1);
        assert_eq!(back["keys"]["kill"][0], "K");
        assert!(serde_json::from_str::<Config>(r#"{"sort":"bogus"}"#).is_err(), "bad values are reported, not silently reset");
    }
}
