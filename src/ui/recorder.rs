//! Persisted history: snapshots are appended to a JSON-lines file in the
//! platform state directory and the most recent hour is reloaded at startup,
//! so rewinding works across restarts. Best effort: an unwritable home
//! (containers) just means no persistence.

use std::fs::{File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{Snapshot, SystemInfo};

/// Snapshots kept (in memory and on disk): an hour at the default rate.
pub const KEEP: usize = 3600;
/// Older recordings are ignored.
const MAX_AGE_SECS: f64 = 24.0 * 3600.0;
/// Rewrite (compact) the file once this much has been appended.
const MAX_APPEND_BYTES: usize = 64 << 20;

#[derive(Serialize, Deserialize, PartialEq, Hash)]
struct Header {
    schema: u32,
    host: String,
    gpus: Vec<String>,
}

pub struct Recorder {
    path: PathBuf,
    out: Option<BufWriter<File>>,
    header: Header,
    /// Lines and bytes appended since the last rewrite.
    lines: usize,
    bytes: usize,
}

/// One file per machine *and* GPU set, so a demo run or a changed GPU list
/// never overwrites another recording.
fn path(h: &Header) -> Option<PathBuf> {
    let safe: String = h.host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    h.hash(&mut hs);
    Some(crate::config::state_dir()?.join(format!("history-{safe}-{:08x}.jsonl", hs.finish() as u32)))
}

/// What gets written: no per-core CPU array (bulky, only shown live).
fn line(s: &Snapshot) -> serde_json::Result<String> {
    if s.host.per_core.is_empty() {
        return serde_json::to_string(s);
    }
    let mut s = s.clone();
    s.host.per_core.clear();
    serde_json::to_string(&s)
}

impl Recorder {
    /// Load this machine's recent history, compact the file, and keep it
    /// open for appending.
    pub fn open(info: &SystemInfo, now: f64) -> (Recorder, Vec<Snapshot>) {
        let header = Header {
            schema: crate::report::SCHEMA,
            host: info.host.hostname.clone(),
            gpus: info.gpus.iter().map(|g| g.stable_id()).collect(),
        };
        let path = path(&header).unwrap_or_default();
        let mut past = Vec::new();
        if let Ok(f) = File::open(&path) {
            let mut lines = BufReader::new(f).lines().map_while(Result::ok);
            if lines.next().and_then(|h| serde_json::from_str::<Header>(&h).ok()).is_some_and(|h| h == header) {
                let mut all: Vec<Snapshot> =
                    lines.filter_map(|l| serde_json::from_str::<Snapshot>(&l).ok()).filter(|s| now - s.timestamp < MAX_AGE_SECS).collect();
                past = all.split_off(all.len().saturating_sub(KEEP));
            }
        }
        let mut r = Recorder { path, out: None, header, lines: 0, bytes: 0 };
        r.rewrite(past.iter());
        (r, past)
    }

    /// Replace the file with the header and `snaps` (keeps it bounded), then
    /// reopen it for appending.
    pub fn rewrite<'a>(&mut self, snaps: impl Iterator<Item = &'a Snapshot>) {
        self.out = None;
        if self.path.as_os_str().is_empty() {
            return;
        }
        let result = (|| -> std::io::Result<BufWriter<File>> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut w = BufWriter::new(File::create(&self.path)?);
            writeln!(w, "{}", serde_json::to_string(&self.header)?)?;
            for s in snaps {
                writeln!(w, "{}", line(s)?)?;
            }
            w.flush()?;
            drop(w);
            // Append mode: each line lands at the end even if another
            // instance writes the same file.
            Ok(BufWriter::new(OpenOptions::new().append(true).open(&self.path)?))
        })();
        match result {
            Ok(w) => self.out = Some(w),
            Err(e) => log::debug!("history not persisted ({}): {e}", self.path.display()),
        }
        self.lines = 0;
        self.bytes = 0;
    }

    /// Append one snapshot. Returns true when the file has grown enough that
    /// the caller should `rewrite` it from memory.
    pub fn append(&mut self, s: &Snapshot) -> bool {
        let Some(w) = &mut self.out else { return false };
        match line(s).ok().map(|l| (l.len(), writeln!(w, "{l}").and_then(|_| w.flush()))) {
            Some((n, Ok(()))) => {
                self.lines += 1;
                self.bytes += n;
            }
            _ => self.out = None,
        }
        self.lines > KEEP || self.bytes > MAX_APPEND_BYTES
    }
}
