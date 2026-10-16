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
mod remote;
mod report;
mod serve;
mod spill;
mod ui;

use std::time::Duration;

use clap::Parser;

use collector::Collector;

#[derive(Parser, Debug)]
#[command(name = brand::APP_NAME, version, about = "Terminal GPU monitor. Single binary; works on bare metal, in containers, WSL and Windows.")]
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

    /// Pixel graphics for charts: auto-detect, force a protocol, or off.
    #[arg(long, value_name = "auto|kitty|sixel|iterm2|off", default_value = "auto")]
    graphics: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    logging::init(if cli.diagnose { log::LevelFilter::Debug } else { log::LevelFilter::Warn });
    let cfg = config::load();
    let secs = cli.interval.or(cfg.interval).unwrap_or(1.0);
    let interval = Duration::from_secs_f64(secs.clamp(0.1, 60.0));

    if let Some(addr) = &cli.connect {
        let (source, info, first) = remote::connect(addr)?;
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
        let opts = ui::UiOptions { theme: cli.theme.clone(), readonly: true, graphics: cli.graphics.clone(), remote: Some(addr.clone()) };
        return ui::run(source, info, interval, &opts, cfg);
    }

    let detected = backend::detect(cli.demo);
    let (mut collector, info) = Collector::new(detected);

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

    if let Some(addr) = &cli.serve {
        return serve::run(collector, info, interval, addr, &cfg);
    }

    let opts = ui::UiOptions { theme: cli.theme.clone(), readonly: cli.readonly, graphics: cli.graphics.clone(), remote: None };
    ui::run(collector, info, interval, &opts, cfg)
}
