//! Minimal `log` backend. Fallbacks and silently-skipped data sources log at
//! debug level so `--diagnose` (or `<APP>_LOG=debug`) can explain them.
//! While the TUI owns the terminal, records go to a small in-memory ring
//! instead of stderr.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use log::{LevelFilter, Log, Metadata, Record};

static LOGGER: Logger = Logger;
static TO_MEMORY: AtomicBool = AtomicBool::new(false);
static RING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
const RING_CAP: usize = 200;

struct Logger;

impl Log for Logger {
    fn enabled(&self, m: &Metadata) -> bool {
        m.level() <= log::max_level()
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let line = format!("{:<5} {}: {}", r.level(), r.target(), r.args());
        if TO_MEMORY.load(Ordering::Relaxed) {
            if let Ok(mut ring) = RING.lock() {
                if ring.len() == RING_CAP {
                    ring.pop_front();
                }
                ring.push_back(line);
            }
        } else {
            eprintln!("{line}");
        }
    }

    fn flush(&self) {}
}

/// Level from `<APP>_LOG` (error|warn|info|debug|trace), else `default`.
pub fn init(default: LevelFilter) {
    let level = crate::brand::env("LOG").and_then(|v| v.parse().ok()).unwrap_or(default);
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);
}

/// Route records to memory (while the TUI is up) or back to stderr.
pub fn capture(on: bool) {
    TO_MEMORY.store(on, Ordering::Relaxed);
    if !on {
        // Show what was logged while the screen was busy.
        if let Ok(mut ring) = RING.lock() {
            for l in ring.drain(..) {
                eprintln!("{l}");
            }
        }
    }
}
