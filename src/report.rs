//! Non-interactive output: `--once` (pretty, gpustat-style), `--json`, `--diagnose`.

use std::fmt::Write as _;
use std::io::{IsTerminal, Write};

use serde::Serialize;

use crate::brand::{APP_NAME, APP_VERSION};
use crate::fmt;
use crate::model::{PidMapping, ProcRow, Snapshot, SystemInfo};

/// Bump whenever a JSON field changes name or meaning.
pub const SCHEMA: u32 = 2;

/// The JSON document emitted by `--json` and served at `/json`.
#[derive(Serialize)]
pub struct JsonOut<'a> {
    pub schema: u32,
    pub tool: String,
    pub system: &'a SystemInfo,
    pub snapshot: &'a Snapshot,
}

impl<'a> JsonOut<'a> {
    pub fn new(system: &'a SystemInfo, snapshot: &'a Snapshot) -> Self {
        JsonOut { schema: SCHEMA, tool: format!("{APP_NAME} {APP_VERSION}"), system, snapshot }
    }
}

struct Ansi(bool);
impl Ansi {
    fn c(&self, code: &str, s: impl std::fmt::Display) -> String {
        if self.0 { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
    }
    fn heat(&self, frac: f64, s: impl std::fmt::Display) -> String {
        let code = if frac >= 0.85 {
            "1;31"
        } else if frac >= 0.6 {
            "1;33"
        } else {
            "1;32"
        };
        self.c(code, s)
    }
}

fn bar(frac: f64, width: usize) -> String {
    const PART: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let f = frac.clamp(0.0, 1.0) * width as f64;
    let full = f as usize;
    let rem = ((f - full as f64) * 8.0) as usize;
    let mut s = "█".repeat(full);
    if full < width {
        s.push_str(PART[rem]);
        s.push_str(&" ".repeat(width - full - usize::from(rem > 0)));
    }
    s
}

pub fn once(info: &SystemInfo, snap: &Snapshot, color: bool) {
    let a = Ansi(color && std::io::stdout().is_terminal());
    let mut out = String::new();
    let b = info.backends.first();
    let _ = writeln!(
        out,
        "{}  {}  {}{}",
        a.c("1;36", &info.host.hostname),
        a.c("2", chrono::Local::now().format("%H:%M:%S")),
        b.and_then(|b| b.driver_version.as_ref()).map(|d| format!("driver {d}")).unwrap_or_default(),
        b.and_then(|b| Some(format!("  {} {}", b.runtime_name.as_deref()?, b.runtime_version.as_deref()?))).unwrap_or_default(),
    );
    for w in &info.warnings {
        let _ = writeln!(out, "{} {w}", a.c("33", "!"));
    }
    for (g, s) in info.gpus.iter().zip(&snap.gpus) {
        let util = s.util_gpu.unwrap_or(0.0) as f64 / 100.0;
        let mem = s.mem_frac().unwrap_or(0.0);
        let temp = s.temp.map(|t| a.heat(t as f64 / 90.0, format!("{t:>3}°C"))).unwrap_or_else(|| "  N/A".into());
        let power = match (s.power, s.power_limit) {
            (Some(p), Some(l)) => a.heat(p / l, format!("{p:>4.0}/{l:.0}W")),
            (Some(p), None) => format!("{p:>4.0}W"),
            _ => "N/A".into(),
        };
        let mem_text = format!(
            "{:>9} / {:<9}",
            s.mem_used.map(fmt::bytes).unwrap_or_else(|| "N/A".into()),
            s.mem_total.map(fmt::bytes).unwrap_or_else(|| "N/A".into())
        );
        let _ = writeln!(
            out,
            "{} {:<24} {}  {} {}  {} {} {}  {}",
            a.c("1;34", format!("[{}]", g.index)),
            fmt::truncate(g.short_name(), 24),
            temp,
            a.heat(util, format!("{:>3.0}%", util * 100.0)),
            a.heat(util, bar(util, 10)),
            a.c("1;35", mem_text),
            a.c("35", bar(mem, 10)),
            power,
            procs_inline(snap, g.index, &a),
        );
        let procs: Vec<&ProcRow> = snap.processes.iter().filter(|p| p.gpu == g.index).collect();
        for f in crate::diagnose::diagnose(g, s, &procs, crate::diagnose::Recent::default()) {
            let code = match f.severity {
                crate::events::Severity::Crit => "1;31",
                crate::events::Severity::Warn => "1;33",
                crate::events::Severity::Info => "1;36",
            };
            let _ = writeln!(out, "    {} {}", a.c(code, format!("{}:", f.title)), f.detail);
        }
        if let Some(e) = &s.error {
            let _ = writeln!(out, "    {}", a.c("1;31", e));
        }
    }
    let _ = std::io::stdout().write_all(out.as_bytes());
}

fn procs_inline(snap: &Snapshot, gpu: usize, a: &Ansi) -> String {
    const MAX: usize = 5;
    let mut ps: Vec<&ProcRow> = snap.processes.iter().filter(|p| p.gpu == gpu).collect();
    ps.sort_by_key(|p| std::cmp::Reverse((p.usage.gpu_mem, p.usage.sm_util.map(|v| (v * 100.0) as u64))));
    let more = ps.len().saturating_sub(MAX);
    let mut s = ps
        .into_iter()
        .take(MAX)
        .map(|p| {
            let name = p.name.rsplit(['/', '\\']).next().unwrap_or(&p.name);
            let who = p.user.as_deref().map(|u| format!("{u}:")).unwrap_or_default();
            let usage = match (p.usage.gpu_mem, p.usage.sm_util) {
                (Some(m), _) => fmt::bytes_short(m),
                (None, Some(u)) => format!("{u:.0}%"),
                _ => "?".into(),
            };
            let spill = if p.spilling { a.c("1;31", "!") } else { String::new() };
            format!("{}{}/{}({usage}){spill}", a.c("2", who), a.c("36", name), p.pid())
        })
        .collect::<Vec<_>>()
        .join(" ");
    if more > 0 {
        s += &a.c("2", format!(" +{more} more"));
    }
    s
}

pub fn json(info: &SystemInfo, snap: &Snapshot, pretty: bool) {
    let o = JsonOut::new(info, snap);
    let s = if pretty { serde_json::to_string_pretty(&o) } else { serde_json::to_string(&o) };
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{}", s.unwrap_or_default());
    let _ = stdout.flush();
}

pub fn diagnose(info: &SystemInfo, snap: &Snapshot) {
    println!("{APP_NAME} {APP_VERSION} diagnostics");
    println!("  os:         {} ({})", info.host.os, info.host.kernel.as_deref().unwrap_or("?"));
    println!("  container:  {}", info.host.container.as_deref().unwrap_or("no"));
    println!("  wsl:        {}", info.host.wsl);
    println!("  env:        {}={}", crate::brand::env_var("NVML_PATH"), crate::brand::env("NVML_PATH").unwrap_or_default());
    for k in ["NVIDIA_VISIBLE_DEVICES", "NVIDIA_DRIVER_CAPABILITIES", "CUDA_VISIBLE_DEVICES", "LD_LIBRARY_PATH"] {
        if let Ok(v) = std::env::var(k) {
            println!("              {k}={v}");
        }
    }
    for b in &info.backends {
        println!(
            "  backend:    {} lib={} driver={} api={} runtime={} {}",
            b.name,
            b.library.as_deref().unwrap_or("-"),
            b.driver_version.as_deref().unwrap_or("-"),
            b.api_version.as_deref().unwrap_or("-"),
            b.runtime_name.as_deref().unwrap_or(""),
            b.runtime_version.as_deref().unwrap_or("-")
        );
    }
    for w in &info.warnings {
        println!("  warning:    {w}");
    }
    for g in &info.gpus {
        println!(
            "  gpu {}:      {} [{}] bus={} uuid={}",
            g.index,
            g.name,
            g.architecture.as_deref().or(g.chip.as_deref()).unwrap_or("?"),
            g.pci_bus_id.as_deref().unwrap_or("?"),
            g.uuid.as_deref().unwrap_or("?")
        );
        if let Some(s) = snap.gpus.get(g.index) {
            let checks = [
                ("util", s.util_gpu.is_some()),
                ("memory", s.mem_used.is_some()),
                ("shared-mem", s.mem_shared.is_some()),
                ("temp", s.temp.is_some()),
                ("fan", !s.fans.is_empty()),
                ("power", s.power.is_some()),
                ("clocks", s.clock_graphics.is_some()),
                ("pcie", s.pcie_rx.is_some()),
                ("encdec", s.enc_util.is_some()),
                ("ecc", s.ecc_corrected.is_some()),
                ("bandwidth", g.memory_bandwidth.is_some()),
            ];
            let missing: Vec<&str> = checks.iter().filter(|c| !c.1).map(|c| c.0).collect();
            println!("              unavailable: {}", if missing.is_empty() { "-".into() } else { missing.join(", ") });
            println!("              spill: {:?} {}", s.spill.level, s.spill.reason);
        }
    }
    println!("  processes:  {}", snap.processes.len());
    for p in &snap.processes {
        let m = match p.mapping {
            PidMapping::Direct => "direct",
            PidMapping::Translated => "translated",
            PidMapping::Guessed => "guessed",
            PidMapping::External => "external",
        };
        println!(
            "    gpu{} pid={} local={} [{m}] mem={} shared={}{} sm={} {}",
            p.gpu,
            p.pid(),
            p.local_pid.map(|l| l.to_string()).unwrap_or_else(|| "-".into()),
            p.usage.gpu_mem.map(fmt::bytes).unwrap_or_else(|| "N/A".into()),
            p.usage.gpu_mem_shared.map(fmt::bytes).unwrap_or_else(|| "N/A".into()),
            if p.spilling { " SPILL" } else { "" },
            fmt::pct(p.usage.sm_util),
            fmt::truncate(&p.cmdline, 100)
        );
    }
}
