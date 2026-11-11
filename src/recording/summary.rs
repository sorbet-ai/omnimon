//! `--replay WHEN --once`: what a stretch of recording adds up to, and
//! `--replay A --diff B`: two stretches side by side. Computed in one pass,
//! so a long range doesn't have to fit in memory.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::model::{Snapshot, SystemInfo, Throttle};

/// Gaps longer than this many intervals (nothing was recording) don't count.
const GAP: f64 = 3.0;
/// Utilization at or above this counts as busy.
const BUSY: f64 = 5.0;
/// Processes listed.
const TOP: usize = 8;

#[derive(Serialize, Default, Clone, Copy)]
pub struct Stat {
    pub mean: Option<f64>,
    pub p95: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Default)]
struct Acc {
    /// (value, weight in seconds)
    v: Vec<(f64, f64)>,
}

impl Acc {
    fn add(&mut self, v: Option<f64>, dt: f64) {
        if let Some(v) = v.filter(|v| v.is_finite()) {
            self.v.push((v, dt));
        }
    }
    fn stat(&mut self) -> Stat {
        let w: f64 = self.v.iter().map(|x| x.1).sum();
        if self.v.is_empty() || w <= 0.0 {
            return Stat::default();
        }
        let mean = self.v.iter().map(|(v, d)| v * d).sum::<f64>() / w;
        self.v.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut seen, mut p95) = (0.0, self.v.last().map(|x| x.0));
        for (v, d) in &self.v {
            seen += d;
            if seen >= 0.95 * w {
                p95 = Some(*v);
                break;
            }
        }
        Stat { mean: Some(mean), p95, max: self.v.last().map(|x| x.0) }
    }
}

#[derive(Serialize, Default)]
pub struct GpuSum {
    pub index: usize,
    pub name: String,
    /// Utilization, %.
    pub util: Stat,
    /// Seconds at or above 5% utilization.
    pub busy_secs: f64,
    /// Watts.
    pub power: Stat,
    pub energy_wh: Option<f64>,
    /// Bytes.
    pub mem_used_max: Option<u64>,
    pub mem_total: Option<u64>,
    /// °C.
    pub temp: Stat,
    /// Graphics clock, MHz.
    pub clock: Stat,
    /// Seconds each clock limiter was active (not counting "idle").
    pub limited_secs: BTreeMap<String, f64>,
    /// Seconds with GPU memory spilled into system RAM.
    pub spill_secs: f64,
}

#[derive(Serialize, Default)]
pub struct HostSum {
    /// CPU, %.
    pub cpu: Stat,
    pub mem_used_max: Option<u64>,
    pub mem_total: Option<u64>,
}

#[derive(Serialize, Default, Clone)]
pub struct ProcSum {
    pub gpu: usize,
    pub pid: u32,
    pub name: String,
    /// Seconds of whole-GPU utilization (50% for 10 s = 5 s).
    pub gpu_secs: f64,
    /// Bytes.
    pub vram_max: Option<u64>,
    /// Seconds seen.
    pub seen_secs: f64,
}

#[derive(Serialize, Default, Clone)]
pub struct CpuProcSum {
    pub pid: u32,
    pub name: String,
    /// Also uses a GPU.
    pub gpu: bool,
    /// Core-seconds of CPU (two cores busy for 10 s = 20 s).
    pub cpu_secs: f64,
    /// Highest CPU, % of one core.
    pub cpu_max: f32,
    /// Bytes.
    pub rss_max: Option<u64>,
    /// Seconds seen.
    pub seen_secs: f64,
}

#[derive(Serialize, Default)]
pub struct Summary {
    pub from: f64,
    pub to: f64,
    pub samples: usize,
    /// Seconds actually recorded (gaps left out).
    pub recorded_secs: f64,
    pub gpus: Vec<GpuSum>,
    pub host: HostSum,
    /// Busiest GPU processes, by GPU time.
    pub processes: Vec<ProcSum>,
    /// Busiest processes by CPU time, GPU or not. Recordings keep only the
    /// busiest few CPU-only processes per sample, so quieter ones are missing.
    pub cpu_processes: Vec<CpuProcSum>,
    /// Event counts by kind.
    pub events: BTreeMap<String, usize>,
}

#[derive(Default)]
struct GpuAcc {
    util: Acc,
    power: Acc,
    temp: Acc,
    clock: Acc,
    /// (first, last) reading of the energy counter.
    energy: Option<(f64, f64)>,
    energy_sum: f64,
    integrated_j: f64,
}

/// Feed snapshots in time order, then [`Builder::finish`].
pub struct Builder {
    interval: f64,
    s: Summary,
    last_t: Option<f64>,
    gpus: Vec<GpuAcc>,
    cpu: Acc,
    procs: HashMap<(usize, u32, String), ProcSum>,
    cpu_procs: HashMap<(u32, String), CpuProcSum>,
}

impl Builder {
    pub fn new(info: &SystemInfo, interval: f64) -> Builder {
        let s = Summary {
            gpus: info.gpus.iter().map(|g| GpuSum { index: g.index, name: g.name.clone(), ..Default::default() }).collect(),
            ..Default::default()
        };
        let gpus = info.gpus.iter().map(|_| GpuAcc::default()).collect();
        Builder { interval, s, last_t: None, gpus, cpu: Acc::default(), procs: HashMap::new(), cpu_procs: HashMap::new() }
    }

    pub fn add(&mut self, snap: &Snapshot) {
        let t = snap.timestamp;
        // Each sample stands for the time since the previous one.
        let (dt, contiguous) = match self.last_t {
            Some(p) if t > p && t - p <= GAP * self.interval => (t - p, true),
            Some(p) if t <= p => return,
            _ => (self.interval, false),
        };
        if self.s.samples == 0 {
            self.s.from = t - dt;
        }
        self.last_t = Some(t);
        self.s.to = t;
        self.s.samples += 1;
        self.s.recorded_secs += dt;

        for (i, g) in snap.gpus.iter().enumerate() {
            let (Some(sum), Some(acc)) = (self.s.gpus.get_mut(i), self.gpus.get_mut(i)) else { continue };
            let util = g.util_gpu.map(f64::from);
            acc.util.add(util, dt);
            if util.is_some_and(|u| u >= BUSY) {
                sum.busy_secs += dt;
            }
            acc.power.add(g.power, dt);
            acc.integrated_j += g.power.unwrap_or(0.0) * dt;
            acc.temp.add(g.temp.map(f64::from), dt);
            acc.clock.add(g.clock_graphics.map(f64::from), dt);
            if let Some(e) = g.energy_joules {
                match &mut acc.energy {
                    // Counters reset (driver reload): count only increases,
                    // and not across gaps.
                    Some((_, last)) => {
                        if e > *last && contiguous {
                            acc.energy_sum += e - *last;
                        }
                        *last = e;
                    }
                    None => acc.energy = Some((e, e)),
                }
            }
            sum.mem_used_max = sum.mem_used_max.max(g.mem_used);
            sum.mem_total = sum.mem_total.or(g.mem_total);
            for th in g.throttle.iter().filter(|th| !matches!(th, Throttle::Idle)) {
                *sum.limited_secs.entry(th.label().to_string()).or_default() += dt;
            }
            if g.spill.level == crate::model::SpillLevel::Spilling {
                sum.spill_secs += dt;
            }
        }
        self.cpu.add(Some(snap.host.cpu as f64), dt);
        self.s.host.mem_used_max = self.s.host.mem_used_max.max(Some(snap.host.mem_used));
        if snap.host.mem_total > 0 {
            self.s.host.mem_total = Some(snap.host.mem_total);
        }
        for p in &snap.processes {
            let e = self.procs.entry((p.gpu, p.pid(), p.name.clone())).or_insert_with(|| ProcSum {
                gpu: p.gpu,
                pid: p.pid(),
                name: p.name.clone(),
                ..Default::default()
            });
            e.gpu_secs += p.usage.sm_util.map(f64::from).unwrap_or(0.0) / 100.0 * dt;
            e.vram_max = e.vram_max.max(p.usage.gpu_mem);
            e.seen_secs += dt;
        }
        // A process on two GPUs is listed twice; count its CPU once.
        let mut counted = std::collections::HashSet::new();
        for p in snap.processes.iter().chain(&snap.host_processes) {
            let Some(cpu) = p.cpu else { continue };
            if !counted.insert(p.pid()) {
                continue;
            }
            let e = self.cpu_procs.entry((p.pid(), p.name.clone())).or_insert_with(|| CpuProcSum {
                pid: p.pid(),
                name: p.name.clone(),
                ..Default::default()
            });
            e.gpu |= p.on_gpu();
            e.cpu_secs += cpu as f64 / 100.0 * dt;
            e.cpu_max = e.cpu_max.max(cpu);
            e.rss_max = e.rss_max.max(p.rss);
            e.seen_secs += dt;
        }
        for ev in &snap.events {
            *self.s.events.entry(ev.kind.name().to_string()).or_default() += 1;
        }
    }

    pub fn finish(mut self) -> Summary {
        for (sum, acc) in self.s.gpus.iter_mut().zip(&mut self.gpus) {
            sum.util = acc.util.stat();
            sum.power = acc.power.stat();
            sum.temp = acc.temp.stat();
            sum.clock = acc.clock.stat();
            sum.energy_wh = if acc.energy.is_some() && acc.energy_sum > 0.0 {
                Some(acc.energy_sum / 3600.0)
            } else {
                sum.power.mean.map(|_| acc.integrated_j / 3600.0)
            };
        }
        self.s.host.cpu = self.cpu.stat();
        let mut procs: Vec<ProcSum> = self.procs.into_values().collect();
        procs.sort_by(|a, b| b.gpu_secs.total_cmp(&a.gpu_secs).then(b.vram_max.cmp(&a.vram_max)));
        procs.truncate(TOP);
        self.s.processes = procs;
        let mut cpu: Vec<CpuProcSum> = self.cpu_procs.into_values().filter(|p| p.cpu_secs > 0.0).collect();
        cpu.sort_by(|a, b| b.cpu_secs.total_cmp(&a.cpu_secs));
        cpu.truncate(TOP);
        self.s.cpu_processes = cpu;
        self.s
    }
}

// ------------------------------------------------------------------ output

#[derive(Clone, Copy)]
enum Unit {
    Pct,
    Watts,
    Wh,
    Bytes,
    Celsius,
    Mhz,
    Secs,
}

fn show(v: Option<f64>, u: Unit) -> String {
    let Some(v) = v else { return "-".into() };
    match u {
        Unit::Pct => format!("{v:.0}%"),
        Unit::Watts => format!("{v:.0} W"),
        Unit::Wh => {
            if v < 10.0 {
                format!("{v:.2} Wh")
            } else {
                format!("{v:.0} Wh")
            }
        }
        Unit::Bytes => crate::fmt::bytes(v as u64),
        Unit::Celsius => format!("{v:.0}°C"),
        Unit::Mhz => format!("{v:.0} MHz"),
        Unit::Secs => crate::fmt::duration(v.round() as u64),
    }
}

/// Change from a to b, in the unit's terms.
fn change(a: Option<f64>, b: Option<f64>, u: Unit) -> String {
    let (Some(a), Some(b)) = (a, b) else { return String::new() };
    let d = b - a;
    // Whole units: no "-0".
    let whole = |d: f64| if d.round() == 0.0 { "0".to_string() } else { format!("{:+.0}", d.round()) };
    match u {
        Unit::Pct => format!("{} pts", whole(d)),
        Unit::Celsius => format!("{}°C", whole(d)),
        Unit::Secs => format!("{}{}", if d < 0.0 { "-" } else { "+" }, crate::fmt::duration(d.abs().round() as u64)),
        _ if a.abs() > 1e-9 => format!("{}%", whole(d / a * 100.0)),
        _ => String::new(),
    }
}

/// The comparable numbers of a summary, as labelled rows.
fn rows(s: &Summary) -> Vec<(String, Option<f64>, Unit)> {
    let mut r = Vec::new();
    let stat = |r: &mut Vec<_>, label: &str, st: &Stat, u: Unit| {
        r.push((format!("{label} mean"), st.mean, u));
        r.push((format!("{label} p95"), st.p95, u));
        r.push((format!("{label} max"), st.max, u));
    };
    for g in &s.gpus {
        let p = format!("gpu{} ", g.index);
        stat(&mut r, &format!("{p}util"), &g.util, Unit::Pct);
        r.push((format!("{p}busy"), Some(g.busy_secs), Unit::Secs));
        stat(&mut r, &format!("{p}power"), &g.power, Unit::Watts);
        r.push((format!("{p}energy"), g.energy_wh, Unit::Wh));
        r.push((format!("{p}memory peak"), g.mem_used_max.map(|v| v as f64), Unit::Bytes));
        stat(&mut r, &format!("{p}temp"), &g.temp, Unit::Celsius);
        r.push((format!("{p}clock mean"), g.clock.mean, Unit::Mhz));
        for (k, v) in &g.limited_secs {
            r.push((format!("{p}limited: {k}"), Some(*v), Unit::Secs));
        }
        if g.spill_secs > 0.0 {
            r.push((format!("{p}spilling"), Some(g.spill_secs), Unit::Secs));
        }
    }
    stat(&mut r, "host cpu", &s.host.cpu, Unit::Pct);
    r.push(("host memory peak".into(), s.host.mem_used_max.map(|v| v as f64), Unit::Bytes));
    r
}

fn span(s: &Summary) -> String {
    let t = |x: f64| {
        chrono::DateTime::from_timestamp(x as i64, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default()
    };
    format!("{} .. {} ({} recorded, {} samples)", t(s.from), t(s.to), show(Some(s.recorded_secs), Unit::Secs), s.samples)
}

fn procs(s: &Summary) -> Vec<String> {
    s.processes
        .iter()
        .map(|p| {
            format!(
                "  gpu{} {:>7}  {:<28} {:>8} GPU time  {:>9} VRAM peak  seen {}",
                p.gpu,
                p.pid,
                crate::fmt::truncate(&p.name, 28),
                show(Some(p.gpu_secs), Unit::Secs),
                show(p.vram_max.map(|v| v as f64), Unit::Bytes),
                show(Some(p.seen_secs), Unit::Secs)
            )
        })
        .collect()
}

fn cpu_procs(s: &Summary) -> Vec<String> {
    s.cpu_processes
        .iter()
        .map(|p| {
            format!(
                "  {:<4} {:>7}  {:<28} {:>8} CPU time  {:>5.0}% max  {:>9} RAM peak  seen {}",
                if p.gpu { "gpu" } else { "cpu" },
                p.pid,
                crate::fmt::truncate(&p.name, 28),
                show(Some(p.cpu_secs), Unit::Secs),
                p.cpu_max,
                show(p.rss_max.map(|v| v as f64), Unit::Bytes),
                show(Some(p.seen_secs), Unit::Secs)
            )
        })
        .collect()
}

pub fn print(s: &Summary) {
    println!("{}", span(s));
    for g in &s.gpus {
        println!("gpu{}  {}", g.index, g.name);
    }
    for (label, v, u) in rows(s) {
        println!("  {label:<26} {}", show(v, u));
    }
    if !s.processes.is_empty() {
        println!("busiest GPU processes");
        procs(s).iter().for_each(|l| println!("{l}"));
    }
    if !s.cpu_processes.is_empty() {
        println!("busiest CPU processes (CPU time in core-seconds)");
        cpu_procs(s).iter().for_each(|l| println!("{l}"));
    }
    if !s.events.is_empty() {
        println!("events  {}", s.events.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(" · "));
    }
}

pub fn print_diff(a: &Summary, b: &Summary) {
    println!("A  {}", span(a));
    println!("B  {}", span(b));
    let rb: HashMap<String, (Option<f64>, Unit)> = rows(b).into_iter().map(|(l, v, u)| (l, (v, u))).collect();
    let mut seen = std::collections::HashSet::new();
    println!("  {:<26} {:>12} {:>12}  change", "", "A", "B");
    for (label, va, u) in rows(a) {
        let vb = rb.get(&label).and_then(|x| x.0);
        seen.insert(label.clone());
        println!("  {label:<26} {:>12} {:>12}  {}", show(va, u), show(vb, u), change(va, vb, u));
    }
    for (label, vb, u) in rows(b).into_iter().filter(|r| !seen.contains(&r.0)) {
        println!("  {label:<26} {:>12} {:>12}", "-", show(vb, u));
    }
    for (name, s) in [("A", a), ("B", b)] {
        if !s.processes.is_empty() {
            println!("busiest GPU processes in {name}");
            procs(s).iter().for_each(|l| println!("{l}"));
        }
        if !s.cpu_processes.is_empty() {
            println!("busiest CPU processes in {name}");
            cpu_procs(s).iter().for_each(|l| println!("{l}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GpuInfo, GpuProcess, GpuSample, ProcRow};

    fn snap(t: f64, util: f32, power: f64, energy: f64) -> Snapshot {
        Snapshot {
            timestamp: t,
            gpus: vec![GpuSample {
                util_gpu: Some(util),
                power: Some(power),
                energy_joules: Some(energy),
                throttle: if util > 50.0 { vec![Throttle::PowerCap] } else { vec![Throttle::Idle] },
                ..Default::default()
            }],
            processes: vec![ProcRow {
                gpu: 0,
                name: "train".into(),
                usage: GpuProcess { pid: 7, sm_util: Some(util), gpu_mem: Some(1 << 30), ..Default::default() },
                cpu: Some(100.0),
                ..Default::default()
            }],
            host_processes: vec![ProcRow {
                gpu: crate::model::NO_GPU,
                name: "compile".into(),
                usage: GpuProcess { pid: 9, ..Default::default() },
                cpu: Some(400.0),
                rss: Some(1 << 20),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn adds_up_time_weighted_and_skips_gaps() {
        let info = SystemInfo { gpus: vec![GpuInfo { name: "G".into(), ..Default::default() }], ..Default::default() };
        let mut b = Builder::new(&info, 1.0);
        // 10 s idle, then a gap (not recorded), then 10 s at 100%.
        for i in 0..10 {
            b.add(&snap(i as f64, 0.0, 50.0, 1000.0 + 50.0 * i as f64));
        }
        for i in 0..10 {
            b.add(&snap(1000.0 + i as f64, 100.0, 300.0, 5000.0 + 300.0 * i as f64));
        }
        let s = b.finish();
        assert_eq!(s.samples, 20);
        assert_eq!(s.recorded_secs, 20.0, "the gap isn't time");
        let g = &s.gpus[0];
        assert_eq!(g.util.mean, Some(50.0));
        assert_eq!(g.util.max, Some(100.0));
        assert_eq!(g.busy_secs, 10.0);
        assert_eq!(g.limited_secs.get("power cap"), Some(&10.0));
        assert!(!g.limited_secs.contains_key("idle"));
        // Energy from the counter, without the gap: 9*50 + 9*300 J.
        assert_eq!(g.energy_wh, Some((450.0 + 2700.0) / 3600.0));
        assert_eq!(s.processes[0].gpu_secs, 10.0);
        assert_eq!(s.processes[0].vram_max, Some(1 << 30));
        // CPU time in core-seconds, GPU and CPU-only processes together.
        let names: Vec<(&str, f64, bool)> = s.cpu_processes.iter().map(|p| (p.name.as_str(), p.cpu_secs, p.gpu)).collect();
        assert_eq!(names, [("compile", 80.0, false), ("train", 20.0, true)]);
        assert_eq!(s.cpu_processes[0].rss_max, Some(1 << 20));
    }
}
