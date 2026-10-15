//! Event timeline: notable changes between two snapshots (a process starts
//! spilling, clocks get thermally throttled, a GPU errors, a process exits).
//! Pure, so it is unit-tested; the collector attaches new events to each
//! snapshot and the UI keeps the log.

use serde::{Deserialize, Serialize};

use crate::fmt;
use crate::model::{Snapshot, SpillLevel, Throttle};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Crit,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// Unix time, seconds.
    pub time: f64,
    pub gpu: Option<usize>,
    pub severity: Severity,
    pub text: String,
}

/// Events between `prev` and `cur`.
pub fn diff(prev: &Snapshot, cur: &Snapshot) -> Vec<Event> {
    let mut out = Vec::new();
    let mut ev = |gpu: Option<usize>, severity: Severity, text: String| out.push(Event { time: cur.timestamp, gpu, severity, text });

    for (i, (a, b)) in prev.gpus.iter().zip(&cur.gpus).enumerate() {
        let g = Some(i);
        match (&a.error, &b.error) {
            (None, Some(e)) => ev(g, Severity::Crit, format!("GPU {i}: {e}")),
            (Some(_), None) => ev(g, Severity::Info, format!("GPU {i} recovered")),
            _ => {}
        }
        // Clock limiters worth knowing about, edge-triggered.
        let notable = |t: &[Throttle]| t.iter().filter(|r| r.severity() > 0).cloned().collect::<Vec<_>>();
        let (was, now) = (notable(&a.throttle), notable(&b.throttle));
        for r in now.iter().filter(|r| !was.contains(r)) {
            let sev = if r.severity() >= 2 { Severity::Crit } else { Severity::Warn };
            ev(g, sev, format!("GPU {i}: clocks limited by {}", r.label()));
        }
        for r in was.iter().filter(|r| !now.contains(r)) {
            ev(g, Severity::Info, format!("GPU {i}: {} cleared", r.label()));
        }
        if a.spill.level != SpillLevel::Spilling && b.spill.level == SpillLevel::Spilling {
            let slow = b.spill.slowdown.filter(|x| *x >= 1.5).map(|x| format!(", ≈{x:.0}× slower")).unwrap_or_default();
            ev(g, Severity::Crit, format!("GPU {i}: spill started, {}{slow}", b.spill.reason));
        } else if a.spill.level == SpillLevel::Spilling && b.spill.level != SpillLevel::Spilling {
            ev(g, Severity::Info, format!("GPU {i}: spill ended"));
        }
    }

    let key = |p: &crate::model::ProcRow| p.key();
    for p in cur.processes.iter().filter(|p| !prev.processes.iter().any(|q| key(q) == key(p))) {
        ev(Some(p.gpu), Severity::Info, format!("GPU {}: {} ({}) started", p.gpu, p.name, p.pid()));
    }
    for p in prev.processes.iter().filter(|p| !cur.processes.iter().any(|q| key(q) == key(p))) {
        ev(Some(p.gpu), Severity::Info, format!("GPU {}: {} ({}) exited", p.gpu, p.name, p.pid()));
    }
    for p in &cur.processes {
        let before = prev.processes.iter().find(|q| key(q) == key(p));
        if p.spilling && !before.is_some_and(|q| q.spilling) {
            let bytes = p.usage.gpu_mem_shared.map(fmt::bytes).unwrap_or_default();
            ev(Some(p.gpu), Severity::Crit, format!("GPU {}: {} ({}) spilled {bytes} into system memory", p.gpu, p.name, p.pid()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GpuProcess, GpuSample, PidMapping, ProcRow, Spill};

    fn proc(pid: u32, spilling: bool) -> ProcRow {
        ProcRow {
            gpu: 0,
            usage: GpuProcess { pid, gpu_mem_shared: Some(2 << 30), ..Default::default() },
            local_pid: Some(pid),
            mapping: PidMapping::Direct,
            name: "train".into(),
            cmdline: String::new(),
            user: None,
            cpu: None,
            rss: None,
            runtime: None,
            spilling,
        }
    }

    fn snap(t: f64, gpu: GpuSample, procs: Vec<ProcRow>) -> Snapshot {
        Snapshot { timestamp: t, gpus: vec![gpu], processes: procs, ..Default::default() }
    }

    #[test]
    fn spill_and_throttle_edges() {
        let a = snap(1.0, GpuSample::default(), vec![proc(1, false)]);
        let spilling = GpuSample {
            spill: Spill { level: SpillLevel::Spilling, reason: "2.00GiB in system memory".into(), ..Default::default() },
            throttle: vec![Throttle::HwThermal, Throttle::Idle],
            ..Default::default()
        };
        let b = snap(2.0, spilling, vec![proc(1, true)]);
        let e = diff(&a, &b);
        assert!(e.iter().any(|e| e.text.contains("spill started") && e.severity == Severity::Crit));
        assert!(e.iter().any(|e| e.text.contains("hw thermal")));
        assert!(!e.iter().any(|e| e.text.contains("idle")), "idle is not an event");
        assert!(e.iter().any(|e| e.text.contains("spilled 2.00GiB")));
        assert!(e.iter().all(|e| e.time == 2.0));
        // Steady state: nothing new.
        assert!(diff(&b, &b).is_empty());
        // And back.
        let e = diff(&b, &a);
        assert!(e.iter().any(|e| e.text.contains("spill ended")));
        assert!(e.iter().any(|e| e.text.contains("hw thermal cleared")));
    }

    #[test]
    fn process_lifecycle() {
        let a = snap(1.0, GpuSample::default(), vec![proc(1, false)]);
        let b = snap(2.0, GpuSample::default(), vec![proc(2, false)]);
        let e = diff(&a, &b);
        assert!(e.iter().any(|e| e.text.contains("(2) started")));
        assert!(e.iter().any(|e| e.text.contains("(1) exited")));
    }
}
