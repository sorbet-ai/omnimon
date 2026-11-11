//! Recordings: snapshots written to disk compactly, read back for rewind
//! across restarts, `--replay` and `--replay --json`.
//!
//! Layout: one folder per machine (`<state dir>/recordings/<host>/`), one
//! file per writer and hour, named by its first sample's UTC time
//! (`20261111T093105Z.rec`). A file is a series of gzip members (so it is a
//! valid `.gz`); decompressed, it's JSON lines:
//! - `{"h": header}`: format version, sampling interval, the machine and GPUs;
//! - `{"k": snapshot}`: a full snapshot (a keyframe), first in every member;
//! - `{"d": delta}`: what changed since the previous line (see `delta`).
//!
//! Members hold a minute of samples and are flushed every few seconds, so a
//! crash loses at most that, and a file can be read while it's written.
//! Process run times are stored as start times, which don't change.
//!
//! One process writes a folder at a time (a lock file); others wait and
//! take over when it exits, so the TUI and a headless `--record` can run
//! together without writing everything twice.

mod delta;
pub mod summary;

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Snapshot, SystemInfo};

/// Snapshots kept in memory for rewind and loaded at startup: an hour at
/// the default rate.
pub const KEEP: usize = 3600;
/// Format version written in each header.
const VERSION: u32 = 1;
/// Snapshots per gzip member (each starts with a keyframe).
const MEMBER: usize = 60;
/// Make what's written readable (and crash-safe) at least this often.
const FLUSH: Duration = Duration::from_secs(10);
/// How often a waiting writer checks whether the folder became free.
const LOCK_RETRY: Duration = Duration::from_secs(5);
const EXT: &str = "rec";

/// How much to keep: older files go first, then the oldest until under the
/// size cap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Retention {
    pub secs: f64,
    pub bytes: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Retention { secs: 7.0 * 86400.0, bytes: 1 << 30 }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Header {
    pub v: u32,
    /// JSON schema of the snapshots (`report::SCHEMA`).
    pub schema: u32,
    /// Writer name and version.
    pub app: String,
    /// Sampling interval, seconds.
    pub interval: f64,
    pub system: SystemInfo,
}

impl Header {
    pub fn gpu_ids(&self) -> Vec<String> {
        self.system.gpus.iter().map(|g| g.stable_id()).collect()
    }
}

#[derive(Serialize, Deserialize)]
enum Line {
    #[serde(rename = "h")]
    Header(Box<Header>),
    #[serde(rename = "k")]
    Key(Value),
    #[serde(rename = "d")]
    Delta(Value),
}

/// Host (CPU-only) processes kept per recorded snapshot: the busiest few that
/// are actually doing something, with short command lines. The live view has
/// the full list; this keeps the rewind buffer and recordings small.
const HOST_PROCS_KEEP: usize = 8;
const HOST_PROC_MIN_CPU: f32 = 1.0;
const HOST_CMDLINE_KEEP: usize = 96;
/// Command lines kept for GPU processes.
const CMDLINE_KEEP: usize = 256;

/// What's kept of a snapshot once it's no longer live (rewind buffer and
/// recordings).
pub fn trimmed(s: &Snapshot) -> Snapshot {
    let mut s = s.clone();
    s.host_processes.retain(|p| p.cpu.unwrap_or(0.0) >= HOST_PROC_MIN_CPU);
    s.host_processes.sort_by(|a, b| b.cpu.unwrap_or(0.0).total_cmp(&a.cpu.unwrap_or(0.0)));
    s.host_processes.truncate(HOST_PROCS_KEEP);
    for p in &mut s.processes {
        p.cmdline.truncate(p.cmdline.floor_char_boundary(CMDLINE_KEEP));
    }
    for p in &mut s.host_processes {
        p.cmdline.truncate(p.cmdline.floor_char_boundary(HOST_CMDLINE_KEEP));
    }
    s
}

/// This machine's recordings folder.
pub fn host_dir(hostname: &str) -> Option<PathBuf> {
    let safe: String = hostname.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
    Some(crate::config::state_dir()?.join("recordings").join(if safe.is_empty() { "localhost".into() } else { safe }))
}

/// History files of earlier versions (one JSON snapshot per line, up to
/// ~100 MB); recordings replace them.
pub fn remove_legacy() {
    let Some(dir) = crate::config::state_dir() else { return };
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("history-") && name.ends_with(".jsonl") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

// ---------------------------------------------------------------- encoding

/// A snapshot as stored: no per-core CPU (bulky, only shown live), run
/// times as start times.
fn encode(s: &Snapshot) -> serde_json::Result<Value> {
    // Through text, so f32s keep their short form instead of widening.
    let mut v: Value = serde_json::from_str(&serde_json::to_string(s)?)?;
    if let Some(h) = v.get_mut("host").and_then(Value::as_object_mut) {
        h.remove("per_core");
    }
    run_times(&mut v, true);
    Ok(v)
}

fn decode(mut v: Value) -> serde_json::Result<Snapshot> {
    run_times(&mut v, false);
    serde_json::from_value(v)
}

/// `runtime` (seconds running, changes every sample) <-> `started` (whole
/// seconds of the sample's clock minus that, constant).
fn run_times(v: &mut Value, to_start: bool) {
    let Some(t) = v.get("timestamp").and_then(Value::as_f64) else { return };
    let t = t.floor() as i64;
    for list in ["processes", "host_processes"] {
        for p in v.get_mut(list).and_then(Value::as_array_mut).into_iter().flatten() {
            let Some(o) = p.as_object_mut() else { continue };
            let (from, to) = if to_start { ("runtime", "started") } else { ("started", "runtime") };
            if let Some(x) = o.get(from).and_then(Value::as_i64) {
                o.remove(from);
                o.insert(to.into(), Value::from(t - x));
            }
        }
    }
}

fn file_name(t: f64) -> String {
    let d = chrono::DateTime::from_timestamp(t as i64, 0).unwrap_or_default();
    format!("{}.{EXT}", d.format("%Y%m%dT%H%M%SZ"))
}

/// Start time of a file from its name (`20261111T093105Z[-n].rec`).
fn file_start(p: &Path) -> Option<f64> {
    if p.extension()? != EXT {
        return None;
    }
    let stem = p.file_stem()?.to_str()?;
    let s = stem.get(..16)?;
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<u32>().ok();
    if s.as_bytes()[8] != b'T' || s.as_bytes()[15] != b'Z' {
        return None;
    }
    let date = chrono::NaiveDate::from_ymd_opt(n(0..4)? as i32, n(4..6)?, n(6..8)?)?;
    Some(date.and_hms_opt(n(9..11)?, n(11..13)?, n(13..15)?)?.and_utc().timestamp() as f64)
}

// ------------------------------------------------------------------ writer

struct Out {
    /// None only if ending a member failed (the next write reports it).
    gz: Option<GzEncoder<File>>,
    path: PathBuf,
    /// Hour (Unix time / 3600) this file covers.
    hour: i64,
    in_member: usize,
    last_flush: Instant,
    prev: Option<Value>,
}

impl Out {
    fn gz(&mut self) -> std::io::Result<&mut GzEncoder<File>> {
        self.gz.as_mut().ok_or_else(|| std::io::Error::other("file closed after an error"))
    }

    fn line(&mut self, l: &Line) -> std::io::Result<()> {
        let mut text = serde_json::to_vec(l)?;
        text.push(b'\n');
        self.gz()?.write_all(&text)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.gz()?.flush()?;
        self.last_flush = Instant::now();
        Ok(())
    }

    /// End the gzip member (making everything so far a complete .gz) and
    /// start the next one.
    fn end_member(&mut self) -> std::io::Result<()> {
        let f = self.gz.take().ok_or_else(|| std::io::Error::other("file closed after an error"))?.finish()?;
        self.gz = Some(GzEncoder::new(f, Compression::default()));
        self.in_member = 0;
        self.prev = None;
        self.last_flush = Instant::now();
        Ok(())
    }
}

/// A change in a writer's state: Ok for news, Err for a problem.
pub type Note = Result<String, String>;

/// Appends snapshots to a recordings folder. Never fails the caller: errors
/// and lock state come back as messages from [`Writer::write`].
pub struct Writer {
    dir: PathBuf,
    header: Header,
    keep: Retention,
    lock: Option<File>,
    next_try: Instant,
    out: Option<Out>,
    /// Last state reported, so each change is reported once.
    said: Option<Note>,
}

impl Writer {
    pub fn new(dir: PathBuf, info: &SystemInfo, interval: f64, keep: Retention) -> Writer {
        let header = Header {
            v: VERSION,
            schema: crate::report::SCHEMA,
            app: format!("{} {}", crate::brand::APP_NAME, crate::brand::APP_VERSION),
            interval,
            system: info.clone(),
        };
        Writer { dir, header, keep, lock: None, next_try: Instant::now(), out: None, said: None }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether this process holds the folder (others wait).
    pub fn writing(&self) -> bool {
        self.lock.is_some()
    }

    fn say(&mut self, n: Note) -> Option<Note> {
        if self.said.as_ref() == Some(&n) {
            return None;
        }
        self.said = Some(n.clone());
        Some(n)
    }

    /// Record one snapshot (already [`trimmed`]). Returns a note when the
    /// writer's state changed (started, waiting for another writer, failed).
    pub fn write(&mut self, s: &Snapshot) -> Option<Note> {
        if self.lock.is_none() {
            if Instant::now() < self.next_try {
                return None;
            }
            self.next_try = Instant::now() + LOCK_RETRY;
            match self.try_lock() {
                Ok(true) => {}
                Ok(false) => {
                    let msg =
                        format!("another {} is recording to {}; taking over when it stops", crate::brand::APP_NAME, self.dir.display());
                    return self.say(Ok(msg));
                }
                Err(e) => {
                    self.next_try = Instant::now() + Duration::from_secs(60);
                    return self.say(Err(format!("not recording ({}): {e:#}", self.dir.display())));
                }
            }
        }
        match self.append(s) {
            Ok(()) => self.say(Ok(format!("recording to {}", self.dir.display()))),
            Err(e) => {
                // Drop the file (a later write starts a new one) and back off.
                self.out = None;
                self.lock = None;
                self.next_try = Instant::now() + Duration::from_secs(60);
                self.say(Err(format!("recording stopped ({}): {e:#}", self.dir.display())))
            }
        }
    }

    fn try_lock(&mut self) -> anyhow::Result<bool> {
        std::fs::create_dir_all(&self.dir).context("creating the folder")?;
        let f =
            OpenOptions::new().create(true).truncate(false).write(true).open(self.dir.join(".lock")).context("opening the lock file")?;
        match f.try_lock() {
            Ok(()) => {
                self.lock = Some(f);
                Ok(true)
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(false),
            Err(std::fs::TryLockError::Error(e)) => Err(e).context("locking"),
        }
    }

    fn append(&mut self, s: &Snapshot) -> anyhow::Result<()> {
        let hour = (s.timestamp / 3600.0).floor() as i64;
        if self.out.as_ref().is_none_or(|o| o.hour != hour) {
            self.close();
            self.out = Some(self.open_file(s.timestamp, hour)?);
            prune(&self.dir, &self.keep, s.timestamp, self.out.as_ref().map(|o| o.path.as_path()));
        }
        let v = encode(s)?;
        let o = self.out.as_mut().expect("opened above");
        let line = match &o.prev {
            None => Line::Key(v.clone()),
            Some(prev) => Line::Delta(delta::diff(prev, &v).unwrap_or_else(|| Value::Object(Default::default()))),
        };
        o.line(&line)?;
        o.prev = Some(v);
        o.in_member += 1;
        if o.in_member >= MEMBER {
            o.end_member()?;
        } else if o.last_flush.elapsed() >= FLUSH {
            o.flush()?;
        }
        Ok(())
    }

    fn open_file(&self, t: f64, hour: i64) -> anyhow::Result<Out> {
        let name = file_name(t);
        let mut path = self.dir.join(&name);
        let mut n = 1;
        let f = loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(f) => break f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => {
                    n += 1;
                    path = self.dir.join(name.replace(&format!(".{EXT}"), &format!("-{n}.{EXT}")));
                }
                Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
            }
        };
        let mut o =
            Out { gz: Some(GzEncoder::new(f, Compression::default())), path, hour, in_member: 0, last_flush: Instant::now(), prev: None };
        // The header is a member of its own: readable as soon as it's written.
        o.line(&Line::Header(Box::new(self.header.clone())))?;
        o.end_member()?;
        Ok(o)
    }

    fn close(&mut self) {
        if let Some(gz) = self.out.take().and_then(|o| o.gz) {
            let _ = gz.finish();
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.close();
    }
}

/// Delete what retention says to: files whose data ended before the cutoff,
/// then the oldest until the folder fits the size cap. Never `current`.
fn prune(dir: &Path, keep: &Retention, now: f64, current: Option<&Path>) {
    let files = list(dir);
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let mut total: u64 = files.iter().map(|f| size(&f.path)).sum();
    for (i, f) in files.iter().enumerate() {
        if Some(f.path.as_path()) == current {
            continue;
        }
        // A file ends where the next begins (or within the hour it started).
        let end = files.get(i + 1).map(|n| n.start).unwrap_or(f.start + 3600.0).min(f.start + 3600.0);
        if end < now - keep.secs || total > keep.bytes {
            let n = size(&f.path);
            if std::fs::remove_file(&f.path).is_ok() {
                total = total.saturating_sub(n);
            }
        }
    }
}

// ------------------------------------------------------------------ reader

pub struct RecFile {
    pub path: PathBuf,
    /// First sample's time, from the name.
    pub start: f64,
}

/// Recording files in a folder, oldest first (or just `path` if it's a file).
pub fn list(path: &Path) -> Vec<RecFile> {
    if path.is_file() {
        return vec![RecFile { path: path.to_path_buf(), start: file_start(path).unwrap_or(0.0) }];
    }
    let mut v: Vec<RecFile> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            Some(RecFile { start: file_start(&p)?, path: p })
        })
        .collect();
    v.sort_by(|a, b| a.start.total_cmp(&b.start).then_with(|| a.path.cmp(&b.path)));
    v
}

/// Decode a file, calling `f(header, snapshot)` for each snapshot within
/// [from, to] recorded under a header `want` accepts, until `f` returns
/// false. A cut-off end (crash, still being written) ends the file quietly.
/// Returns how many snapshots in range were skipped by `want`.
fn read_file(
    path: &Path,
    from: f64,
    to: f64,
    want: &mut dyn FnMut(&Header) -> bool,
    f: &mut dyn FnMut(&Header, Snapshot) -> bool,
) -> std::io::Result<usize> {
    let lines = BufReader::new(MultiGzDecoder::new(BufReader::new(File::open(path)?))).lines();
    let (mut header, mut ok, mut cur, mut skipped): (Option<Header>, bool, Option<Value>, usize) = (None, false, None, 0);
    for l in lines {
        let Ok(l) = l else { break };
        match serde_json::from_str::<Line>(&l) {
            Ok(Line::Header(h)) => {
                ok = want(&h);
                header = Some(*h);
                cur = None;
            }
            Ok(Line::Key(v)) => cur = Some(v),
            Ok(Line::Delta(d)) => match &mut cur {
                Some(c) => delta::apply(c, &d),
                None => continue,
            },
            Err(e) => {
                // Deltas after a bad line would apply to the wrong base.
                log::debug!("{}: {e}", path.display());
                cur = None;
                continue;
            }
        }
        let (Some(h), Some(c)) = (&header, &cur) else { continue };
        let t = c.get("timestamp").and_then(Value::as_f64).unwrap_or(0.0);
        if t < from {
            continue;
        }
        if t > to {
            break;
        }
        if !ok {
            skipped += 1;
            continue;
        }
        match decode(c.clone()) {
            Ok(s) => {
                if !f(h, s) {
                    break;
                }
            }
            Err(e) => log::debug!("{}: {e}", path.display()),
        }
    }
    Ok(skipped)
}

/// What was read: the header of the newest file used, the snapshots
/// (oldest first) and how many were left out for being from another GPU set.
#[derive(Default)]
pub struct Loaded {
    pub header: Option<Header>,
    pub snaps: Vec<Snapshot>,
    pub skipped: usize,
}

/// The newest `max` snapshots at or after `since` (going back file by file),
/// from the GPU set `gpus` (None: whatever the newest file has).
pub fn load_recent(path: &Path, gpus: Option<&[String]>, since: f64, max: usize) -> Loaded {
    let mut out = Loaded::default();
    let mut want_ids: Option<Vec<String>> = gpus.map(<[String]>::to_vec);
    for file in list(path).iter().rev() {
        let mut got = Vec::new();
        let r = read_file(
            &file.path,
            since,
            f64::INFINITY,
            &mut |h| want_ids.get_or_insert_with(|| h.gpu_ids()) == &h.gpu_ids(),
            &mut |h, s| {
                if out.header.is_none() {
                    out.header = Some(h.clone());
                }
                got.push(s);
                true
            },
        );
        match r {
            Ok(n) => out.skipped += n,
            Err(e) => log::debug!("{}: {e}", file.path.display()),
        }
        got.append(&mut out.snaps);
        out.snaps = got;
        if out.snaps.len() >= max || file.start < since {
            break;
        }
    }
    let extra = out.snaps.len().saturating_sub(max);
    out.snaps.drain(..extra);
    out
}

/// Snapshots in [from, to], oldest first, from the GPU set of the first
/// file with data in range, until `f` returns false. Returns how many were
/// left out for being from another GPU set.
pub fn each_in_range(path: &Path, from: f64, to: f64, f: &mut dyn FnMut(&Header, Snapshot) -> bool) -> usize {
    let files = list(path);
    let (mut skipped, mut more) = (0, true);
    let mut want_ids: Option<Vec<String>> = None;
    for (i, file) in files.iter().enumerate() {
        // Files can't hold data from before their name or past the next one's start.
        let next = files.get(i + 1).map(|n| n.start).unwrap_or(f64::INFINITY);
        if file.start > to || next < from {
            continue;
        }
        let r = read_file(&file.path, from, to, &mut |h| want_ids.get_or_insert_with(|| h.gpu_ids()) == &h.gpu_ids(), &mut |h, s| {
            more = f(h, s);
            more
        });
        match r {
            Ok(n) => skipped += n,
            Err(e) => log::debug!("{}: {e}", file.path.display()),
        }
        if !more {
            break;
        }
    }
    skipped
}

/// Up to `max` snapshots of [`each_in_range`].
pub fn load_range(path: &Path, from: f64, to: f64, max: usize) -> Loaded {
    let mut out = Loaded::default();
    out.skipped = each_in_range(path, from, to, &mut |h, s| {
        if out.header.is_none() {
            out.header = Some(h.clone());
        }
        out.snaps.push(s);
        out.snaps.len() < max
    });
    out
}

/// A source with nothing new: what the UI samples while replaying.
pub struct Idle;

impl crate::collector::Source for Idle {
    fn collect(&mut self) -> Option<Snapshot> {
        None
    }
    fn apply(&mut self, _gpu: usize, _cmd: &crate::backend::Tune) -> Result<String, String> {
        Err("this is a recording".into())
    }
}

/// `--record` without `--serve`: sample and record until killed.
pub fn run(
    collector: crate::collector::Collector,
    interval: Duration,
    mut w: Writer,
    keep: Retention,
    cfg: &crate::config::Config,
) -> anyhow::Result<()> {
    eprintln!(
        "{} {}: recording every {}s to {} (keeping {}, at most {}); Ctrl-C to stop",
        crate::brand::APP_NAME,
        crate::brand::APP_VERSION,
        interval.as_secs_f64(),
        w.dir().display(),
        human_span(keep.secs),
        crate::fmt::bytes(keep.bytes)
    );
    let sampler = crate::collector::Sampler::spawn(collector, interval);
    let mut alerts = crate::alerts::Alerts::new(cfg.alert_command.clone(), cfg.alert_min);
    while let Ok(s) = sampler.rx.recv() {
        alerts.handle(&s.events);
        report_note(w.write(&trimmed(&s)));
    }
    anyhow::bail!("the sampler thread stopped")
}

/// "7 days", "48 hours", "90 minutes".
fn human_span(secs: f64) -> String {
    let (n, unit) = if secs >= 86400.0 && secs % 86400.0 == 0.0 {
        (secs / 86400.0, "day")
    } else if secs >= 3600.0 && secs % 3600.0 == 0.0 {
        (secs / 3600.0, "hour")
    } else {
        (secs / 60.0, "minute")
    };
    format!("{n:.0} {unit}{}", if n == 1.0 { "" } else { "s" })
}

/// Print a writer's note for headless use, with the time.
pub fn report_note(n: Option<Note>) {
    if let Some(n) = n {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        match n {
            Ok(m) => eprintln!("{now} {m}"),
            Err(m) => eprintln!("{now} warning: {m}"),
        }
    }
}

// ------------------------------------------------------------------- times

/// What to replay: a time range, or (None) the latest hour recorded.
pub type Range = Option<(f64, f64)>;

/// Parse `--replay`'s WHEN: empty (latest hour), a point (that hour), or
/// `A..B`. Points: `now`, `90m` / `2h` / `1d` (ago), `14:00[:30]` (today, or
/// yesterday if that's still ahead), `2026-11-10`, `2026-11-10 14:00`
/// (also with `T`). Local time.
pub fn parse_when(s: &str, now: chrono::DateTime<chrono::Local>) -> anyhow::Result<Range> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let (a, b) = match s.split_once("..") {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (s, None),
    };
    let from = point(a, now)?;
    let to = match b {
        Some("") | None => from + 3600.0,
        Some(b) => point(b, now)?,
    };
    if to <= from {
        bail!("{s:?}: the end is not after the start");
    }
    Ok(Some((from, to)))
}

fn point(s: &str, now: chrono::DateTime<chrono::Local>) -> anyhow::Result<f64> {
    use chrono::{Local, NaiveDate, NaiveTime, TimeZone};
    let t_now = now.timestamp() as f64;
    if s == "now" {
        return Ok(t_now);
    }
    let rel = s.trim_start_matches('-').trim_end_matches(" ago").trim();
    if let Some(secs) = duration(rel) {
        return Ok(t_now - secs);
    }
    let time = |t: &str| NaiveTime::parse_from_str(t, "%H:%M:%S").or_else(|_| NaiveTime::parse_from_str(t, "%H:%M")).ok();
    let local = |d: chrono::NaiveDateTime| Local.from_local_datetime(&d).earliest().map(|d| d.timestamp() as f64);
    if let Some(t) = time(s) {
        let today = local(now.date_naive().and_time(t)).context("no such local time")?;
        return Ok(if today > t_now { today - 86400.0 } else { today });
    }
    let (d, t) = match s.split_once([' ', 'T']) {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    if let Ok(d) = NaiveDate::parse_from_str(d, "%Y-%m-%d") {
        let t = match t {
            Some(t) => time(t).with_context(|| format!("{t:?} is not HH:MM[:SS]"))?,
            None => NaiveTime::MIN,
        };
        return local(d.and_time(t)).context("no such local time");
    }
    bail!("{s:?}: expected now, 2h (ago), 14:00, 2026-11-10 or 2026-11-10 14:00")
}

/// `90s`, `15m`, `2h`, `7d`, `1.5h` -> seconds.
pub fn duration(s: &str) -> Option<f64> {
    let s = s.trim();
    let unit = match s.chars().last()? {
        's' => 1.0,
        'm' => 60.0,
        'h' => 3600.0,
        'd' => 86400.0,
        _ => return None,
    };
    let n: f64 = s[..s.len() - 1].trim().parse().ok()?;
    (n >= 0.0 && n.is_finite()).then_some(n * unit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GpuInfo, GpuProcess, GpuSample, NO_GPU, ProcRow};

    fn info() -> SystemInfo {
        SystemInfo { gpus: vec![GpuInfo { uuid: Some("GPU-1".into()), ..Default::default() }], ..Default::default() }
    }

    fn snap(t: f64) -> Snapshot {
        let i = t as u32;
        let row = |gpu: usize, pid: u32, cpu: f32| ProcRow {
            gpu,
            usage: GpuProcess { pid, sm_util: Some((i % 7) as f32 * 1.1), ..Default::default() },
            name: format!("p{pid}"),
            cmdline: format!("/usr/bin/p{pid} --flag"),
            cpu: Some(cpu),
            runtime: Some(t as u64 - 1000),
            ..Default::default()
        };
        Snapshot {
            timestamp: t,
            host: crate::model::HostSample { cpu: (i % 10) as f32, per_core: vec![1.0; 8], ..Default::default() },
            gpus: vec![GpuSample { util_gpu: Some((i % 100) as f32), power: Some(100.0 + t.fract()), ..Default::default() }],
            // A process comes and goes; another is always there.
            processes: (0..3).filter(|p| *p != 2 || i % 5 < 3).map(|p| row(0, 100 + p, 0.5)).collect(),
            host_processes: vec![row(NO_GPU, 7, 5.0 + (i % 3) as f32)],
            events: Vec::new(),
        }
    }

    fn as_value(s: &Snapshot) -> Value {
        let mut s = s.clone();
        s.host.per_core.clear();
        serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap()
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("{}-test-{name}-{}", crate::brand::APP_NAME, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn snapshots_round_trip_exactly() {
        let dir = tmp("roundtrip");
        let t0 = 1_794_427_000.25;
        let snaps: Vec<Snapshot> = (0..150).map(|i| snap(t0 + i as f64)).collect();
        {
            let mut w = Writer::new(dir.clone(), &info(), 1.0, Retention::default());
            for s in &snaps {
                w.write(s);
            }
            assert!(w.writing());
        }
        let got = load_range(&dir, 0.0, f64::INFINITY, usize::MAX);
        assert_eq!(got.snaps.len(), snaps.len());
        for (a, b) in snaps.iter().zip(&got.snaps) {
            assert_eq!(as_value(a), as_value(b), "at {}", a.timestamp);
        }
        assert_eq!(got.header.unwrap().gpu_ids(), ["GPU-1"]);
        // Ranges and limits.
        let r = load_range(&dir, t0 + 10.0, t0 + 19.5, usize::MAX);
        assert_eq!(r.snaps.len(), 10);
        assert_eq!(r.snaps[0].timestamp, t0 + 10.0);
        let r = load_recent(&dir, Some(&["GPU-1".to_string()]), 0.0, 20);
        assert_eq!(r.snaps.len(), 20);
        assert_eq!(r.snaps.last().unwrap().timestamp, t0 + 149.0);
        assert_eq!(load_recent(&dir, Some(&["other".to_string()]), 0.0, 20).snaps.len(), 0, "another GPU set");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unfinished_files_read_up_to_the_last_flush() {
        let dir = tmp("crash");
        let mut w = Writer::new(dir.clone(), &info(), 1.0, Retention::default());
        for i in 0..75 {
            w.write(&snap(1_794_427_000.0 + i as f64));
        }
        // Simulate a crash: flush (as the timer would) but never finish.
        w.out.as_mut().unwrap().flush().unwrap();
        std::mem::forget(w);
        let got = load_range(&dir, 0.0, f64::INFINITY, usize::MAX);
        assert_eq!(got.snaps.len(), 75);
        // A second writer waits while the first holds the lock... (the
        // forgotten one still does).
        let mut w2 = Writer::new(dir.clone(), &info(), 1.0, Retention::default());
        let msg = w2.write(&snap(1_794_427_100.0));
        assert!(!w2.writing() && msg.is_some_and(|m| m.is_ok_and(|m| m.contains("taking over"))));
    }

    #[test]
    fn small_on_disk() {
        let dir = tmp("size");
        {
            let mut w = Writer::new(dir.clone(), &info(), 1.0, Retention::default());
            for i in 0..600 {
                w.write(&snap(1_794_427_000.0 + i as f64));
            }
        }
        let bytes: u64 = list(&dir).iter().map(|f| std::fs::metadata(&f.path).unwrap().len()).sum();
        let raw: usize = (0..600).map(|i| serde_json::to_string(&snap(1_794_427_000.0 + i as f64)).unwrap().len()).sum();
        assert!(bytes * 15 < raw as u64, "{bytes} bytes on disk for {raw} bytes of JSON");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_files_each_hour_and_retention() {
        let dir = tmp("hours");
        let t0 = 1_794_427_000.0; // 200 s before a whole hour
        {
            let mut w = Writer::new(dir.clone(), &info(), 60.0, Retention { secs: 2.0 * 3600.0, bytes: u64::MAX });
            for i in 0..(5 * 60) {
                w.write(&snap(t0 + i as f64 * 60.0));
            }
        }
        let files = list(&dir);
        // Six hourly files; the first two ended more than 2 h before the last began.
        assert_eq!(files.len(), 4, "{:?}", files.iter().map(|f| f.path.display().to_string()).collect::<Vec<_>>());
        assert_eq!(files[0].start, t0 + 240.0 + 3600.0, "kept from the third hour on");
        assert_eq!(file_start(&files[0].path), Some(files[0].start));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_and_times() {
        assert_eq!(file_name(1_794_427_079.6), "20261111T195759Z.rec");
        assert_eq!(file_start(Path::new("x/20261111T195759Z.rec")), Some(1_794_427_079.0));
        assert_eq!(file_start(Path::new("20261111T195759Z-2.rec")), Some(1_794_427_079.0));
        assert_eq!(file_start(Path::new("20261111T195759Z.txt")), None);
        assert_eq!(duration("90m"), Some(5400.0));
        assert_eq!(duration("1.5h"), Some(5400.0));
        assert_eq!(duration("x"), None);
        use chrono::TimeZone;
        let now = chrono::Local.with_ymd_and_hms(2026, 11, 11, 9, 30, 0).unwrap();
        let t = now.timestamp() as f64;
        assert_eq!(parse_when("", now).unwrap(), None);
        assert_eq!(parse_when("2h", now).unwrap(), Some((t - 7200.0, t - 3600.0)));
        assert_eq!(parse_when("2h ago..now", now).unwrap(), Some((t - 7200.0, t)));
        assert_eq!(parse_when("09:00", now).unwrap(), Some((t - 1800.0, t + 1800.0)));
        assert_eq!(parse_when("10:00", now).unwrap().unwrap().0, t + 1800.0 - 86400.0, "still ahead: yesterday");
        assert_eq!(parse_when("2026-11-11 08:30..09:00", now).unwrap(), Some((t - 3600.0, t - 1800.0)));
        assert_eq!(parse_when("2026-11-11T08:30:00", now).unwrap().unwrap().0, t - 3600.0);
        assert!(parse_when("yesterday", now).is_err());
        assert!(parse_when("now..1h", now).is_err());
    }

    #[test]
    fn trimmed_keeps_only_busy_host_processes() {
        let host = |pid: u32, cpu: f32| ProcRow {
            gpu: NO_GPU,
            usage: GpuProcess { pid, ..Default::default() },
            cpu: Some(cpu),
            cmdline: "x".repeat(500),
            ..Default::default()
        };
        let mut s = snap(0.0 + 2000.0);
        s.host_processes = (0..20).map(|i| host(i, i as f32)).collect();
        let t = trimmed(&s);
        let kept: Vec<u32> = t.host_processes.iter().map(|p| p.pid()).collect();
        assert_eq!(kept, [19, 18, 17, 16, 15, 14, 13, 12], "busiest first, capped");
        assert!(t.host_processes.iter().all(|p| p.cmdline.len() == HOST_CMDLINE_KEEP));

        s.host_processes = vec![host(1, 0.2), host(2, 0.0)];
        assert!(trimmed(&s).host_processes.is_empty(), "idle processes aren't recorded");
        assert_eq!(s.host_processes.len(), 2, "the live snapshot is untouched");
    }
}
