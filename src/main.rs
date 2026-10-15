mod backend;
mod brand;
mod collector;
mod fmt;
mod host;
mod model;
mod pidmap;
mod report;
mod ui;

use std::time::Duration;

use clap::Parser;

use collector::Collector;

#[derive(Parser, Debug)]
#[command(name = brand::APP_NAME, version, about = "Terminal GPU monitor. Single binary; works on bare metal, in containers, WSL and Windows.")]
pub struct Cli {
    /// Refresh interval in seconds.
    #[arg(short, long, default_value_t = 1.0)]
    interval: f64,

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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let interval = Duration::from_secs_f64(cli.interval.clamp(0.1, 60.0));

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

    ui::run(collector, info, interval, &cli)
}
