mod alerts;
mod backend;
mod brand;
mod collector;
mod config;
mod diagnose;
mod events;
mod fmt;
mod host;
mod logging;
mod model;
mod os;
mod recording;
mod remote;
mod report;
mod serve;
mod spill;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;

use collector::Collector;

#[derive(Parser, Debug)]
#[command(name = brand::APP_NAME, version, about = "Terminal monitor for GPUs, their processes and the host. Linux, Windows and macOS; works in containers and WSL.")]
struct Cli {
    /// Refresh interval in seconds [default: 1, or the last value used].
    #[arg(short, long)]
    interval: Option<f64>,

    /// Print a one-shot summary and exit.
    #[arg(short = '1', long)]
    once: bool,

    /// Print a JSON snapshot and exit (with --watch: one JSON object per line, forever).
    #[arg(long)]
    json: bool,

    /// With --json: keep streaming JSON lines every interval.
    #[arg(short, long, requires = "json")]
    watch: bool,

    /// Simulate N GPUs instead of reading real hardware.
    #[arg(long, num_args = 0..=1, default_missing_value = "4", value_name = "N")]
    demo: Option<usize>,

    /// Color theme (see the in-app theme picker with `t`).
    #[arg(long)]
    theme: Option<String>,

    /// Disable colors in text output.
    #[arg(long)]
    no_color: bool,

    /// Don't allow killing processes from the UI.
    #[arg(long)]
    readonly: bool,

    /// Print what was detected (libraries, environment, PID mapping) and exit.
    #[arg(long)]
    diagnose: bool,

    /// Run headless and serve Prometheus metrics (/metrics) and JSON (/json).
    #[arg(long, num_args = 0..=1, default_missing_value = "127.0.0.1:9465", value_name = "ADDR")]
    serve: Option<String>,

    /// Watch another machine running `--serve` (host[:port], default port 9465).
    #[arg(long, value_name = "ADDR", conflicts_with_all = ["serve", "demo"])]
    connect: Option<String>,

    /// Record to disk without the UI (with --serve: serve and record). The UI
    /// records while it runs anyway.
    #[arg(long, conflicts_with = "connect")]
    record: bool,

    /// Look at recorded data instead of live (with --json: print it; with
    /// --once: sum it up). The latest hour, or WHEN: 2h (ago), 14:00,
    /// 2026-11-10 14:00, or A..B.
    #[arg(long, num_args = 0..=1, default_missing_value = "", value_name = "WHEN", conflicts_with_all = ["serve", "connect", "record", "diagnose", "watch"])]
    replay: Option<String>,

    /// With --replay: compare its range with this one, side by side.
    #[arg(long, value_name = "WHEN", requires = "replay")]
    diff: Option<String>,

    /// Recordings folder, or a .rec file to replay [default: one folder per
    /// machine in the state directory].
    #[arg(long, value_name = "PATH")]
    recordings: Option<PathBuf>,

    /// Keep recordings this long, e.g. 7d or 48h [default: 7d, at most 1 GB].
    #[arg(long, value_name = "DURATION")]
    keep: Option<String>,

    /// Chart colors: rainbow, mono, heat, outline or minimal (L cycles).
    #[arg(long, value_name = "LOOK")]
    look: Option<String>,

    /// Pixel graphics for charts: auto-detect, force a protocol, or off.
    #[arg(long, value_name = "auto|kitty|sixel|iterm2|off", default_value = "auto")]
    graphics: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    logging::init(if cli.diagnose { log::LevelFilter::Debug } else { log::LevelFilter::Warn });
    let (cfg, cfg_error) = match config::load() {
        Ok(c) => (c, None),
        Err(e) => (config::Config::default(), Some(format!("config ignored: {e}"))),
    };
    let secs = cli.interval.or(cfg.interval).unwrap_or(1.0);
    let interval = Duration::from_secs_f64(secs.clamp(0.1, 60.0));

    if let Some(addr) = &cli.connect {
        let (source, mut info, first) = remote::connect(addr)?;
        info.warnings.extend(cfg_error);
        if cli.once || cli.json || cli.diagnose {
            if cli.json {
                report::json(&info, &first, true);
            } else if cli.diagnose {
                report::diagnose(&info, &first);
            } else {
                report::once(&info, &first, !cli.no_color);
            }
            return Ok(());
        }
        let opts = ui::UiOptions {
            theme: cli.theme.clone(),
            readonly: true,
            graphics: cli.graphics.clone(),
            remote: Some(addr.clone()),
            look: cli.look.clone(),
            record: None,
            replay: None,
        };
        return ui::run(source, info, interval, opts, cfg);
    }

    let keep = retention(&cli, &cfg)?;
    if let Some(when) = &cli.replay {
        return replay(&cli, when, cfg);
    }

    let detected = backend::detect(cli.demo);
    let (mut collector, mut info) = Collector::new(detected);
    info.warnings.extend(cfg_error);

    if cli.once || cli.json || cli.diagnose {
        // CPU usage needs two samples spaced >= 200ms apart.
        collector.collect();
        std::thread::sleep(Duration::from_millis(250));
        let mut snap = collector.collect();
        if cli.diagnose {
            report::diagnose(&info, &snap);
        } else if cli.json && cli.watch {
            loop {
                report::json(&info, &snap, false);
                std::thread::sleep(interval);
                snap = collector.collect();
            }
        } else if cli.json {
            report::json(&info, &snap, true);
        } else {
            report::once(&info, &snap, !cli.no_color);
        }
        return Ok(());
    }

    let dir = || cli.recordings.clone().or_else(|| recording::host_dir(&info.host.hostname)).context("no state directory to record to");
    if cli.record {
        let w = recording::Writer::new(dir()?, &info, interval.as_secs_f64(), keep);
        match &cli.serve {
            Some(addr) => return serve::run(collector, info, interval, addr, &cfg, Some(w)),
            None => return recording::run(collector, interval, w, keep, &cfg),
        }
    }
    if let Some(addr) = &cli.serve {
        return serve::run(collector, info, interval, addr, &cfg, None);
    }

    // Demos aren't recorded.
    let demo = !info.gpus.is_empty() && info.gpus.iter().all(|g| g.vendor == model::Vendor::Demo);
    let record = if cfg.record_history == Some(false) || demo { None } else { dir().ok().map(|d| (d, keep)) };
    let opts = ui::UiOptions {
        theme: cli.theme.clone(),
        readonly: cli.readonly,
        graphics: cli.graphics.clone(),
        remote: None,
        look: cli.look.clone(),
        record,
        replay: None,
    };
    ui::run(collector, info, interval, opts, cfg)
}

/// A summary of the recording in `range` (None: the latest hour).
fn summarize(path: &std::path::Path, range: recording::Range) -> Option<recording::summary::Summary> {
    let mut b: Option<recording::summary::Builder> = None;
    let mut add = |h: &recording::Header, s: model::Snapshot| {
        b.get_or_insert_with(|| recording::summary::Builder::new(&h.system, h.interval)).add(&s);
        true
    };
    match range {
        None => {
            let l = recording::load_recent(path, None, f64::NEG_INFINITY, recording::KEEP);
            let h = l.header?;
            for s in l.snaps {
                add(&h, s);
            }
        }
        Some((from, to)) => _ = recording::each_in_range(path, from, to, &mut add),
    }
    Some(b?.finish())
}

fn retention(cli: &Cli, cfg: &config::Config) -> anyhow::Result<recording::Retention> {
    let mut r = recording::Retention::default();
    if let Some(k) = cli.keep.as_ref().or(cfg.record_keep.as_ref()) {
        r.secs = recording::duration(k).with_context(|| format!("keep {k:?}: expected a duration like 7d, 48h or 90m"))?;
    }
    if let Some(mb) = cfg.record_max_mb {
        r.bytes = mb << 20;
    }
    Ok(r)
}

/// `--replay [WHEN]`: recorded snapshots in the UI, or as JSON lines.
fn replay(cli: &Cli, when: &str, cfg: config::Config) -> anyhow::Result<()> {
    let host = if cli.demo.is_some() { backend::demo::HOSTNAME.to_string() } else { sysinfo::System::host_name().unwrap_or_default() };
    let path = cli.recordings.clone().or_else(|| recording::host_dir(&host)).context("no state directory")?;
    let range = recording::parse_when(when, chrono::Local::now())?;
    let what = match range {
        None => "nothing recorded".to_string(),
        Some(_) => format!("nothing recorded in {when:?}"),
    };
    if cli.once || cli.diff.is_some() {
        let a = summarize(&path, range).with_context(|| format!("{what} in {}", path.display()))?;
        let Some(other) = &cli.diff else {
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&a)?);
            } else {
                recording::summary::print(&a);
            }
            return Ok(());
        };
        let rb = recording::parse_when(other, chrono::Local::now())?;
        let b = summarize(&path, rb).with_context(|| format!("nothing recorded in {other:?} in {}", path.display()))?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "a": a, "b": b }))?);
        } else {
            recording::summary::print_diff(&a, &b);
        }
        return Ok(());
    }
    if cli.json {
        let mut n = 0;
        let mut print = |h: &recording::Header, s: model::Snapshot| {
            report::json(&h.system, &s, false);
            n += 1;
            true
        };
        match range {
            None => {
                let l = recording::load_recent(&path, None, f64::NEG_INFINITY, recording::KEEP);
                if let Some(h) = &l.header {
                    for s in l.snaps {
                        print(h, s);
                    }
                }
            }
            // Streamed: a long range doesn't have to fit in memory.
            Some((from, to)) => _ = recording::each_in_range(&path, from, to, &mut print),
        }
        anyhow::ensure!(n > 0, "{what} in {}", path.display());
        return Ok(());
    }
    let loaded = match range {
        None => recording::load_recent(&path, None, f64::NEG_INFINITY, recording::KEEP),
        Some((from, to)) => recording::load_range(&path, from, to, recording::KEEP),
    };
    let Some(h) = loaded.header else { anyhow::bail!("{what} in {}", path.display()) };
    let mut info = h.system;
    if loaded.skipped > 0 {
        info.warnings.push(format!("{} samples left out: recorded with a different set of GPUs", loaded.skipped));
    }
    if range.is_some() && loaded.snaps.len() >= recording::KEEP {
        info.warnings.push(format!("showing the first {} samples of the range", recording::KEEP));
    }
    let opts = ui::UiOptions {
        theme: cli.theme.clone(),
        readonly: true,
        graphics: cli.graphics.clone(),
        remote: None,
        look: cli.look.clone(),
        record: None,
        replay: Some(loaded.snaps),
    };
    let interval = Duration::from_secs_f64(h.interval.clamp(0.1, 3600.0));
    ui::run(recording::Idle, info, interval, opts, cfg)
}
