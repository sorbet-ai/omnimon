//! Time series kept for charts, normalized at ingest time.

use std::collections::HashMap;

use crate::model::{GpuSample, Snapshot, SystemInfo};

const HISTORY: usize = 3600;

/// Ring of samples (Vec + periodic trim so charts can borrow a slice).
#[derive(Default)]
pub struct Ring(Vec<Option<f64>>);

impl Ring {
    pub fn push(&mut self, v: Option<f64>) {
        self.0.push(v);
        if self.0.len() > HISTORY * 2 {
            self.0.drain(..self.0.len() - HISTORY);
        }
    }
    /// `n` samples ending `back` samples before the newest.
    pub fn window(&self, n: usize, back: usize) -> &[Option<f64>] {
        let end = self.0.len().saturating_sub(back);
        &self.0[end.saturating_sub(n)..end]
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
}

#[derive(Default)]
pub struct ProcHist {
    /// Bytes.
    pub mem: Ring,
    pub sm: Ring,
    seen: bool,
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
    /// Rewind offset in samples (0 = live); charts end here.
    pub back: usize,
    last_time: Option<f64>,
}

impl History {
    pub fn push(&mut self, info: &SystemInfo, s: &Snapshot) {
        self.gpus.resize_with(info.gpus.len(), GpuHist::default);
        // A gap in time (the app wasn't running) shows as a break in the charts.
        if self.last_time.is_some_and(|t| s.timestamp - t > 10.0) {
            for h in &mut self.gpus {
                for r in [&mut h.util, &mut h.mem, &mut h.power, &mut h.temp, &mut h.clock, &mut h.rx, &mut h.tx] {
                    r.push(None);
                }
            }
            for r in [&mut self.cpu, &mut self.net_rx, &mut self.net_tx, &mut self.disk] {
                r.push(None);
            }
        }
        self.last_time = Some(s.timestamp);
        for ((h, g), gi) in self.gpus.iter_mut().zip(&s.gpus).zip(&info.gpus) {
            h.util.push(g.util_gpu.map(|v| v as f64 / 100.0));
            h.mem.push(g.mem_frac());
            if let Some(p) = g.power {
                h.peak_power = h.peak_power.max(p);
            }
            h.power.push(power_frac(g, gi.power_limit_max, h.peak_power));
            let tmax = gi.temp_shutdown.unwrap_or(100).max(1) as f64;
            h.temp.push(g.temp.map(|t| t as f64 / tmax));
            h.clock.push(match (g.clock_sm.or(g.clock_graphics), gi.max_clock_sm.or(gi.max_clock_graphics)) {
                (Some(c), Some(m)) if m > 0 => Some(c as f64 / m as f64),
                _ => None,
            });
            h.rx.push(g.pcie_rx.map(|v| v as f64));
            h.tx.push(g.pcie_tx.map(|v| v as f64));
        }
        self.cpu.push(Some(s.host.cpu as f64 / 100.0));
        self.net_rx.push(s.host.net_rx.map(|v| v as f64));
        self.net_tx.push(s.host.net_tx.map(|v| v as f64));
        self.disk.push(match (s.host.disk_read, s.host.disk_write) {
            (Some(r), Some(w)) => Some((r + w) as f64),
            _ => None,
        });
        for p in &s.processes {
            let h = self.procs.entry(p.key()).or_default();
            h.mem.push(p.usage.gpu_mem.map(|v| v as f64));
            h.sm.push(p.usage.sm_util.map(|v| v as f64 / 100.0));
            h.seen = true;
        }
        self.procs.retain(|_, h| std::mem::take(&mut h.seen));
    }

    /// The last `n` samples of `r` as of the rewind position.
    pub fn tail<'a>(&self, r: &'a Ring, n: usize) -> &'a [Option<f64>] {
        r.window(n, self.back)
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

    #[test]
    fn ring_trims_and_tails() {
        let mut r = Ring::default();
        for i in 0..(HISTORY * 2 + 10) {
            r.push(Some(i as f64));
        }
        assert!(r.0.len() <= HISTORY * 2);
        assert_eq!(r.window(2, 0), &[Some((HISTORY * 2 + 8) as f64), Some((HISTORY * 2 + 9) as f64)]);
        assert_eq!(Ring::default().window(5, 0).len(), 0);
        assert_eq!(r.window(2, 1), &[Some((HISTORY * 2 + 7) as f64), Some((HISTORY * 2 + 8) as f64)]);
        assert_eq!(r.window(3, usize::MAX).len(), 0);
    }

    #[test]
    fn power_without_limit_scales_to_peak() {
        let s = GpuSample { power: Some(10.0), ..Default::default() };
        assert_eq!(power_frac(&s, None, 20.0), Some(0.5));
        assert_eq!(power_frac(&s, Some(40.0), 20.0), Some(0.25));
    }
}
