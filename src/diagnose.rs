//! "Why is it slow?": plain-language findings from hardware, OS and process
//! level signals only. Pure, so it is unit-tested.

use serde::{Deserialize, Serialize};

use crate::events::Severity;
use crate::fmt;
use crate::model::{GpuInfo, GpuSample, ProcRow, SpillLevel, Throttle};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Finding {
    pub severity: Severity,
    pub title: String,
    pub detail: String,
}

/// Context that needs history, computed by the caller.
#[derive(Clone, Copy, Debug, Default)]
pub struct Recent {
    /// Mean GPU utilization over the last minute or so, percent.
    pub util_avg: Option<f64>,
    /// Number of samples that average covers.
    pub samples: usize,
}

pub fn diagnose(info: &GpuInfo, s: &GpuSample, procs: &[&ProcRow], recent: Recent) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut add = |severity, title: &str, detail: String| out.push(Finding { severity, title: title.into(), detail });
    // Rules about how busy the GPU is need a utilization reading.
    let util = s.util_gpu.map(f64::from);
    let busy = util.is_some_and(|u| u >= 40.0);

    if let Some(e) = &s.error {
        add(Severity::Crit, "GPU error", e.clone());
    }
    if s.ecc_uncorrected.is_some_and(|e| e > 0) {
        add(
            Severity::Crit,
            "Uncorrected ECC errors",
            format!("{} since driver load; the GPU may need a reset", s.ecc_uncorrected.unwrap_or(0)),
        );
    }
    if s.spill.level == SpillLevel::Spilling {
        let slow = s.spill.slowdown.map(|x| format!(", about {x:.0}× slower for bandwidth-bound work")).unwrap_or_default();
        add(
            Severity::Crit,
            "Spilling into system memory",
            format!("{}{slow}. Free VRAM: smaller model or batch, fewer GPU apps.", s.spill.reason),
        );
    } else if s.spill.level == SpillLevel::Pressure {
        add(Severity::Warn, "VRAM almost full", format!("{}; the next allocation may spill or fail", s.spill.reason));
    }

    let has = |t: Throttle| s.throttle.contains(&t);
    if has(Throttle::HwThermal) || has(Throttle::SwThermal) {
        add(Severity::Crit, "Thermal throttling", format!("clocks reduced at {}; check airflow and fan curve", fmt::opt(s.temp, "°C")));
    } else if let (Some(t), Some(slow)) = (s.temp, info.temp_slowdown.or(info.temp_max_operating))
        && t + 5 >= slow
    {
        add(Severity::Warn, "Running hot", format!("{t}°C, slowdown at {slow}°C"));
    }
    if has(Throttle::PowerBrake) || has(Throttle::HwSlowdown) {
        add(
            Severity::Crit,
            "Hardware slowdown",
            "the board is forcing low clocks (power brake or hardware slowdown); check power cables and PSU".into(),
        );
    }
    if has(Throttle::PowerCap) && busy {
        let lim = s.power_limit.map(|l| format!(" at {l:.0} W")).unwrap_or_default();
        let room = match (s.power_limit, info.power_limit_max) {
            (Some(l), Some(m)) if m > l + 1.0 => format!("; the board allows up to {m:.0} W"),
            _ => String::new(),
        };
        add(Severity::Info, "Power-limited", format!("clocks are capped by the power limit{lim}{room}"));
    }

    // PCIe link below what the slot and card support, while working.
    if busy {
        if let (Some(g), Some(w), Some(mg), Some(mw)) = (s.pcie_gen, s.pcie_width, info.pcie_max_gen, info.pcie_max_width)
            && (g < mg || w < mw)
        {
            add(
                Severity::Warn,
                "PCIe link degraded",
                format!("running Gen{g} ×{w} under load; card supports Gen{mg} ×{mw} (riser, slot or BIOS setting)"),
            );
        }
        if let (Some(c), Some(m)) = (s.clock_sm.or(s.clock_graphics), info.max_clock_sm.or(info.max_clock_graphics)) {
            let quiet = s.throttle.iter().all(|r| r.severity() == 0);
            if util.is_some_and(|u| u >= 80.0) && (c as f64) < 0.5 * m as f64 && quiet {
                add(Severity::Warn, "Clocks low under load", format!("{c} MHz of {m} MHz with no reported limiter"));
            }
        }
    }

    // A process pinning a CPU core while the GPU waits.
    if let Some(util) = util.filter(|u| *u < 50.0)
        && let Some(p) = procs
            .iter()
            .filter(|p| p.cpu.is_some_and(|c| c >= 95.0) && p.usage.sm_util.is_none_or(|u| u < 50.0))
            .max_by(|a, b| a.cpu.unwrap_or(0.0).total_cmp(&b.cpu.unwrap_or(0.0)))
    {
        add(
            Severity::Info,
            "CPU-bound feeder",
            format!(
                "{} ({}) is at {:.0}% CPU while the GPU is {:.0}% busy; data loading or preprocessing may be the bottleneck",
                p.name,
                p.pid(),
                p.cpu.unwrap_or(0.0),
                util
            ),
        );
    }

    // Holding memory without using the GPU.
    if let (Some(avg), Some(m)) = (recent.util_avg, s.mem_frac())
        && recent.samples >= 30
        && avg < 3.0
        && m >= 0.25
    {
        add(Severity::Info, "Allocated but idle", format!("{:.0}% of VRAM held, {avg:.1}% average utilization recently", m * 100.0));
    }
    // NVLink: a dead link sends peer traffic over fewer links (or PCIe).
    let down: Vec<String> = s.nvlink.iter().filter(|l| !l.up).map(|l| l.link.to_string()).collect();
    if !down.is_empty() && s.nvlink.iter().any(|l| l.up) {
        add(
            Severity::Crit,
            "NVLink link down",
            format!(
                "link {} of {} is down; GPU-to-GPU traffic has less bandwidth (check the board, cable or NVSwitch)",
                down.join(", "),
                s.nvlink.len()
            ),
        );
    }
    let errs: Vec<String> = s
        .nvlink
        .iter()
        .filter(|l| l.errors.is_some_and(|e| e > 0))
        .map(|l| format!("link {}: {}", l.link, l.errors.unwrap_or(0)))
        .collect();
    if !errs.is_empty() {
        add(
            Severity::Info,
            "NVLink errors recorded",
            format!("{} (replays, recoveries, CRC since the driver loaded); a rising count points at hardware", errs.join(", ")),
        );
    }

    out.sort_by_key(|f| std::cmp::Reverse(f.severity));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Spill;

    fn gpu() -> GpuInfo {
        GpuInfo {
            pcie_max_gen: Some(4),
            pcie_max_width: Some(16),
            max_clock_sm: Some(3000),
            temp_slowdown: Some(90),
            power_limit_max: Some(600.0),
            ..Default::default()
        }
    }

    #[test]
    fn healthy_gpu_has_no_findings() {
        let s = GpuSample {
            util_gpu: Some(95.0),
            clock_sm: Some(2800),
            pcie_gen: Some(4),
            pcie_width: Some(16),
            temp: Some(70),
            ..Default::default()
        };
        assert!(diagnose(&gpu(), &s, &[], Recent::default()).is_empty());
    }

    #[test]
    fn finds_the_usual_suspects() {
        let s = GpuSample {
            util_gpu: Some(90.0),
            clock_sm: Some(1200),
            pcie_gen: Some(1),
            pcie_width: Some(4),
            temp: Some(88),
            power_limit: Some(450.0),
            throttle: vec![Throttle::PowerCap],
            spill: Spill { level: SpillLevel::Spilling, reason: "4.0GiB in system memory".into(), slowdown: Some(6.0), bytes: None },
            ..Default::default()
        };
        let f = diagnose(&gpu(), &s, &[], Recent::default());
        let titles: Vec<&str> = f.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(f[0].severity, Severity::Crit);
        assert!(titles.contains(&"Spilling into system memory"));
        assert!(titles.contains(&"PCIe link degraded"));
        assert!(titles.contains(&"Running hot"));
        assert!(f.iter().any(|f| f.title == "Power-limited" && f.detail.contains("600 W")));
        // Power cap explains the low clocks, so no separate "clocks low".
        assert!(!titles.contains(&"Clocks low under load"));
    }

    #[test]
    fn idle_allocation_needs_history() {
        let s = GpuSample { util_gpu: Some(0.0), mem_used: Some(20), mem_total: Some(24), ..Default::default() };
        assert!(diagnose(&gpu(), &s, &[], Recent { util_avg: Some(1.0), samples: 5 }).is_empty());
        let f = diagnose(&gpu(), &s, &[], Recent { util_avg: Some(1.0), samples: 60 });
        assert_eq!(f[0].title, "Allocated but idle");
    }
}
