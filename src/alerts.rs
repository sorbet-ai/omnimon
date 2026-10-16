//! Alert hook: run a user command for notable events, e.g.
//! `"alert_command": "notify-send nvtui \"$NVTUI_EVENT\""` or a `curl` to a
//! webhook. The event goes in environment variables, never into the command
//! string, so event text can't inject shell syntax.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::brand::env_var;
use crate::events::{Event, Kind, Severity};

pub struct Alerts {
    command: Option<String>,
    min: Severity,
    /// Last time each (kind, GPU, process) fired, for rate limiting.
    recent: HashMap<(Kind, Option<usize>, Option<u32>), Instant>,
}

/// The shell that runs the hook. On Windows the command goes to `cmd /C`
/// verbatim (Rust's argument quoting would mangle it); variables are then
/// `%NVTUI_EVENT%` rather than `$NVTUI_EVENT`.
fn shell(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        c.arg("/C").raw_arg(cmd);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", cmd]);
        c
    }
}

impl Alerts {
    pub fn new(command: Option<String>, min: Option<Severity>) -> Self {
        Alerts { command: command.filter(|c| !c.trim().is_empty()), min: min.unwrap_or(Severity::Crit), recent: HashMap::new() }
    }

    pub fn handle(&mut self, events: &[Event]) {
        let Some(cmd) = &self.command else { return };
        for e in events.iter().filter(|e| e.severity >= self.min) {
            // The same thing at most once a minute.
            let key = (e.kind, e.gpu, e.pid);
            if self.recent.get(&key).is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
                continue;
            }
            self.recent.insert(key, Instant::now());
            let mut c = shell(cmd);
            c.env(env_var("EVENT"), &e.text)
                .env(env_var("KIND"), e.kind.name())
                .env(env_var("SEVERITY"), format!("{:?}", e.severity).to_lowercase())
                .env(env_var("GPU"), e.gpu.map(|g| g.to_string()).unwrap_or_default())
                .env(env_var("PID"), e.pid.map(|p| p.to_string()).unwrap_or_default())
                .env(env_var("TIME"), format!("{:.0}", e.time))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            match c.spawn() {
                // Reap it so finished hooks don't linger as zombies.
                Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
                Err(err) => log::warn!("alert command failed: {err}"),
            }
        }
        self.recent.retain(|_, t| t.elapsed() < Duration::from_secs(300));
    }
}
