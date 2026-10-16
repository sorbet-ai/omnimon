//! Alert hook: run a user command for notable events, e.g.
//! `"alert_command": "notify-send nvtui \"$NVTUI_EVENT\""` or a `curl` to a
//! webhook. The event goes in environment variables, never into the command
//! string, so event text can't inject shell syntax.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::brand::env_var;
use crate::events::{Event, Severity};

pub struct Alerts {
    command: Option<String>,
    min: Severity,
    /// Last time each event text fired (rate limit).
    recent: HashMap<String, Instant>,
}

impl Alerts {
    pub fn new(command: Option<String>, min: Option<Severity>) -> Self {
        Alerts { command: command.filter(|c| !c.trim().is_empty()), min: min.unwrap_or(Severity::Crit), recent: HashMap::new() }
    }

    pub fn handle(&mut self, events: &[Event]) {
        let Some(cmd) = &self.command else { return };
        for e in events.iter().filter(|e| e.severity >= self.min) {
            // The same event at most once a minute.
            if self.recent.get(&e.text).is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
                continue;
            }
            self.recent.insert(e.text.clone(), Instant::now());
            let mut c = if cfg!(windows) {
                let mut c = std::process::Command::new("cmd");
                c.args(["/C", cmd]);
                c
            } else {
                let mut c = std::process::Command::new("sh");
                c.args(["-c", cmd]);
                c
            };
            c.env(env_var("EVENT"), &e.text)
                .env(env_var("SEVERITY"), format!("{:?}", e.severity).to_lowercase())
                .env(env_var("GPU"), e.gpu.map(|g| g.to_string()).unwrap_or_default())
                .env(env_var("TIME"), format!("{:.0}", e.time))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            if let Err(err) = c.spawn() {
                log::warn!("alert command failed: {err}");
            }
        }
        self.recent.retain(|_, t| t.elapsed() < Duration::from_secs(300));
    }
}
