//! Persisted history: every snapshot is appended to a JSON-lines file in the
//! platform state directory and the most recent hour is reloaded at startup,
//! so rewinding works across restarts. Best effort: an unwritable home
//! (containers) just means no persistence.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{Snapshot, SystemInfo};

/// Snapshots kept (in memory and on disk): an hour at the default rate.
pub const KEEP: usize = 3600;
/// Older recordings are ignored.
const MAX_AGE_SECS: f64 = 24.0 * 3600.0;

#[derive(Serialize, Deserialize, PartialEq)]
struct Header {
    schema: u32,
    host: String,
    gpus: Vec<String>,
}

pub struct Recorder {
    out: Option<BufWriter<File>>,
    header: Header,
    /// Lines appended since the last rewrite.
    lines: usize,
}

fn path(host: &str) -> Option<PathBuf> {
    let safe: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
    Some(crate::config::state_dir()?.join(format!("history-{safe}.jsonl")))
}

fn header(info: &SystemInfo) -> Header {
    Header { schema: crate::report::SCHEMA, host: info.host.hostname.clone(), gpus: info.gpus.iter().map(|g| g.stable_id()).collect() }
}

impl Recorder {
    /// Load the recent history recorded for this machine and GPU set, compact
    /// the file, and keep it open for appending.
    pub fn open(info: &SystemInfo, now: f64) -> (Recorder, Vec<Snapshot>) {
        let want = header(info);
        let Some(path) = path(&want.host) else { return (Recorder { out: None, header: want, lines: 0 }, Vec::new()) };
        let mut past = Vec::new();
        if let Ok(f) = File::open(&path) {
            let mut lines = BufReader::new(f).lines().map_while(Result::ok);
            let same = lines.next().and_then(|h| serde_json::from_str::<Header>(&h).ok()).is_some_and(|h| h == want);
            if same {
                let mut all: Vec<Snapshot> =
                    lines.filter_map(|l| serde_json::from_str::<Snapshot>(&l).ok()).filter(|s| now - s.timestamp < MAX_AGE_SECS).collect();
                let skip = all.len().saturating_sub(KEEP);
                past = all.split_off(skip);
            }
        }
        let mut r = Recorder { out: None, header: want, lines: 0 };
        r.rewrite(past.iter());
        (r, past)
    }

    /// Replace the file with the header and `snaps` (keeps it bounded).
    pub fn rewrite<'a>(&mut self, snaps: impl Iterator<Item = &'a Snapshot>) {
        let Some(path) = path(&self.header.host) else { return };
        let out = (|| -> std::io::Result<BufWriter<File>> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut w = BufWriter::new(OpenOptions::new().create(true).write(true).truncate(true).open(&path)?);
            writeln!(w, "{}", serde_json::to_string(&self.header)?)?;
            for s in snaps {
                writeln!(w, "{}", serde_json::to_string(s)?)?;
            }
            w.flush()?;
            Ok(w)
        })();
        if let Err(e) = &out {
            log::debug!("history not persisted ({}): {e}", path.display());
        }
        self.out = out.ok();
        self.lines = 0;
    }

    /// Append one snapshot. Returns true when the file has grown enough that
    /// the caller should `rewrite` it from memory.
    pub fn append(&mut self, s: &Snapshot) -> bool {
        let Some(w) = &mut self.out else { return false };
        // Command lines can be long; the table only needs the start.
        let mut s = s.clone();
        for p in &mut s.processes {
            p.cmdline.truncate(p.cmdline.floor_char_boundary(240));
        }
        let ok = serde_json::to_string(&s).ok().is_some_and(|line| writeln!(w, "{line}").and_then(|_| w.flush()).is_ok());
        if !ok {
            self.out = None;
        }
        self.lines += 1;
        self.lines > KEEP
    }
}
