//! Time series kept for charts, normalized at ingest time. Every sample
//! carries its timestamp, so charts line up with the snapshot being shown
//! while rewinding, and gaps (the app wasn't running) show as breaks.

use std::collections::HashMap;

use crate::model::{GpuSample, Snapshot, SystemInfo};

const HISTORY: usize = 3600;
/// Exited processes keep their history this long (so rewind can show it).
const PROC_KEEP_SECS: f64 = 3600.0;

/// Timestamped samples (Vec + periodic trim so charts can borrow a slice).
#[derive(Default)]
pub struct Ring {
    t: Vec<f64>,
    v: Vec<Option<f64>>,
}

impl Ring {
    fn push(&mut self, t: f64, v: Option<f64>) {
        self.t.push(t);
        self.v.push(v);
        if self.v.len() > HISTORY * 2 {
            let cut = self.v.len() - HISTORY;
            self.t.drain(..cut);
            self.v.drain(..cut);
        }
    }

    /// End index for samples at or before `at` (None = newest).
    fn end(&self, at: Option<f64>) -> usize {
        match at {
            None => self.v.len(),
            Some(at) => self.t.partition_point(|&t| t <= at),
        }
    }

    /// Up to `n` samples ending at time `at` (None = now).
    pub fn window(&self, n: usize, at: Option<f64>) -> &[Option<f64>] {
        let end = self.end(at);
        &self.v[end.saturating_sub(n)..end]
    }

    /// Seconds covered by that window.
    pub fn span(&self, n: usize, at: Option<f64>) -> Option<f64> {
        let end = self.end(at);
        let first = self.t.get(end.saturating_sub(n))?;
        Some(self.t.get(end.checked_sub(1)?)? - first)
    }

    fn last_time(&self) -> Option<f64> {
        self.t.last().copied()
    }
}

/// Per-GPU series, each normalized to [0,1] except PCIe (bytes/s, auto-scaled).
#[derive(Default)]
pub struct GpuHist {
    pub util: Ring,
    pub mem: Ring,
    pub power: Ring,
    pub temp: Ring,
    pub clock: Ring,
    pub rx: Ring,
    pub tx: Ring,
    /// Highest power seen; the bar scale for GPUs without a power limit (Apple).
    pub peak_power: f64,
    /// Joules since this session started (integrated power).
    pub energy: f64,
}

#[derive(Default)]
pub struct ProcHist {
    /// Bytes.
    pub mem: Ring,
    pub sm: Ring,
    /// Estimated joules this session: GPU power x this process's share of SM activity.
    pub energy: f64,
}

#[derive(Default)]
pub struct History {
    pub gpus: Vec<GpuHist>,
    pub cpu: Ring,
    /// Bytes/s.
    pub net_rx: Ring,
    pub net_tx: Ring,
    pub disk: Ring,
    pub procs: HashMap<(usize, u32), ProcHist>,
    /// Rewind position: charts end at this time (None = live).
    pub at: Option<f64>,
    last_time: Option<f64>,
}

impl History {
    /// Add one snapshot. `interval` (seconds) sets what counts as a gap;
    /// `live` is false while replaying a recording (no energy accounting).
    pub fn push(&mut self, info: &SystemInfo, s: &Snapshot, interval: f64, live: bool) {
        self.gpus.resize_with(info.gpus.len(), GpuHist::default);
        let t = s.timestamp;
        let max_step = (3.0 * interval).max(10.0);
        let dt = self.last_time.map(|l| t - l).filter(|d| *d > 0.0);

        // A gap shows as a break: an empty sample just after the previous one.
        if let (Some(last), Some(d)) = (self.last_time, dt)
            && d > max_step
        {
            let gap = last + d / 2.0;
            for h in &mut self.gpus {
                for r in [&mut h.util, &mut h.mem, &mut h.power, &mut h.temp, &mut h.clock, &mut h.rx, &mut h.tx] {
                    r.push(gap, None);
                }
            }
            for r in [&mut self.cpu, &mut self.net_rx, &mut self.net_tx, &mut self.disk] {
                r.push(gap, None);
            }
        }
        self.last_time = Some(t);

        if let Some(dt) = dt.filter(|d| live && *d <= max_step) {
            self.add_energy(s, dt);
        }
        for ((h, g), gi) in self.gpus.iter_mut().zip(&s.gpus).zip(&info.gpus) {
            h.util.push(t, g.util_gpu.map(|v| v as f64 / 100.0));
            h.mem.push(t, g.mem_frac());
            if let Some(p) = g.power {
                h.peak_power = h.peak_power.max(p);
            }
            h.power.push(t, power_frac(g, gi.power_limit_max, h.peak_power));
            let tmax = gi.temp_shutdown.unwrap_or(100).max(1) as f64;
            h.temp.push(t, g.temp.map(|v| v as f64 / tmax));
            h.clock.push(
                t,
                match (g.clock_sm.or(g.clock_graphics), gi.max_clock_sm.or(gi.max_clock_graphics)) {
                    (Some(c), Some(m)) if m > 0 => Some(c as f64 / m as f64),
                    _ => None,
                },
            );
            h.rx.push(t, g.pcie_rx.map(|v| v as f64));
            h.tx.push(t, g.pcie_tx.map(|v| v as f64));
        }
        self.cpu.push(t, Some(s.host.cpu as f64 / 100.0));
        self.net_rx.push(t, s.host.net_rx.map(|v| v as f64));
        self.net_tx.push(t, s.host.net_tx.map(|v| v as f64));
        self.disk.push(
            t,
            match (s.host.disk_read, s.host.disk_write) {
                (Some(r), Some(w)) => Some((r + w) as f64),
                _ => None,
            },
        );
        for p in &s.processes {
            let h = self.procs.entry(p.key()).or_default();
            h.mem.push(t, p.usage.gpu_mem.map(|v| v as f64));
            h.sm.push(t, p.usage.sm_util.map(|v| v as f64 / 100.0));
        }
        self.procs.retain(|_, h| h.mem.last_time().is_some_and(|l| t - l < PROC_KEEP_SECS));
    }

    fn add_energy(&mut self, s: &Snapshot, dt: f64) {
        for (h, g) in self.gpus.iter_mut().zip(&s.gpus) {
            h.energy += g.power.unwrap_or(0.0) * dt;
        }
        for (i, g) in s.gpus.iter().enumerate() {
            let Some(p) = g.power else { continue };
            let total: f64 = s.processes.iter().filter(|q| q.gpu == i).filter_map(|q| q.usage.sm_util).map(f64::from).sum();
            if total <= 0.0 {
                continue;
            }
            for q in s.processes.iter().filter(|q| q.gpu == i) {
                let share = q.usage.sm_util.unwrap_or(0.0) as f64 / total;
                self.procs.entry(q.key()).or_default().energy += p * dt * share;
            }
        }
    }

    /// The last `n` samples of `r` as of the rewind position.
    pub fn tail<'a>(&self, r: &'a Ring, n: usize) -> &'a [Option<f64>] {
        r.window(n, self.at)
    }

    /// Seconds those samples cover.
    pub fn span(&self, r: &Ring, n: usize) -> Option<f64> {
        r.span(n, self.at)
    }

    /// Power as a fraction of the limit, or of the peak seen if there's no limit.
    pub fn power_frac(&self, i: usize, s: &GpuSample, limit_max: Option<f64>) -> Option<f64> {
        power_frac(s, limit_max, self.gpus.get(i)?.peak_power)
    }
}

fn power_frac(s: &GpuSample, limit_max: Option<f64>, peak: f64) -> Option<f64> {
    let scale = s.power_limit.or(limit_max).unwrap_or(peak.max(5.0));
    s.power.map(|p| (p / scale).min(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GpuInfo, HostSample};

    #[test]
    fn ring_trims_and_windows_by_time() {
        let mut r = Ring::default();
        for i in 0..(HISTORY * 2 + 10) {
            r.push(i as f64, Some(i as f64));
        }
        assert!(r.v.len() <= HISTORY * 2);
        let last = (HISTORY * 2 + 9) as f64;
        assert_eq!(r.window(2, None), &[Some(last - 1.0), Some(last)]);
        assert_eq!(r.window(2, Some(last - 1.0)), &[Some(last - 2.0), Some(last - 1.0)]);
        assert_eq!(r.window(3, Some(-1.0)).len(), 0);
        assert_eq!(r.span(10, None), Some(9.0));
        assert_eq!(Ring::default().window(5, None).len(), 0);
    }

    fn snap(t: f64, util: f32, power: f64) -> Snapshot {
        Snapshot {
            timestamp: t,
            gpus: vec![GpuSample { util_gpu: Some(util), power: Some(power), ..Default::default() }],
            host: HostSample::default(),
            ..Default::default()
        }
    }

    #[test]
    fn gaps_break_charts_but_stay_aligned_with_rewind() {
        let info = SystemInfo { gpus: vec![GpuInfo::default()], ..Default::default() };
        let mut h = History::default();
        for (t, u) in [(0.0, 10.0), (1.0, 20.0), (500.0, 30.0), (501.0, 40.0)] {
            h.push(&info, &snap(t, u, 100.0), 1.0, true);
        }
        let util = |h: &History| h.tail(&h.gpus[0].util, 10).to_vec();
        assert_eq!(util(&h), [Some(0.1), Some(0.2), None, Some(0.3), Some(0.4)]);
        // Rewound to t=1: the chart ends exactly at that snapshot.
        h.at = Some(1.0);
        assert_eq!(util(&h), [Some(0.1), Some(0.2)]);
        // Energy counts steady ticks only, not the gap: 100 W for 1 s twice.
        assert_eq!(h.gpus[0].energy, 200.0);
    }

    #[test]
    fn slow_intervals_are_not_gaps() {
        let info = SystemInfo { gpus: vec![GpuInfo::default()], ..Default::default() };
        let mut h = History::default();
        for t in [0.0, 30.0, 60.0] {
            h.push(&info, &snap(t, 50.0, 10.0), 30.0, true);
        }
        assert_eq!(h.tail(&h.gpus[0].util, 10).len(), 3);
        assert_eq!(h.gpus[0].energy, 600.0);
        // Replayed history doesn't count as this session's energy.
        let mut r = History::default();
        r.push(&info, &snap(0.0, 1.0, 10.0), 1.0, false);
        r.push(&info, &snap(1.0, 1.0, 10.0), 1.0, false);
        assert_eq!(r.gpus[0].energy, 0.0);
    }

    #[test]
    fn power_without_limit_scales_to_peak() {
        let s = GpuSample { power: Some(10.0), ..Default::default() };
        assert_eq!(power_frac(&s, None, 20.0), Some(0.5));
        assert_eq!(power_frac(&s, Some(40.0), 20.0), Some(0.25));
    }
}
