//! Event timeline: notable changes between two snapshots (a process starts
//! spilling, clocks get thermally throttled, a GPU errors, a process exits).
//! Pure, so it is unit-tested; the collector attaches new events to each
//! snapshot and the UI keeps the log.

use std::collections::{HashMap, HashSet};

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

/// What happened, for scripts (alert hooks, JSON consumers) that shouldn't
/// parse the text.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    GpuError,
    GpuRecovered,
    Throttle,
    ThrottleCleared,
    SpillStart,
    SpillEnd,
    ProcStart,
    ProcExit,
    ProcSpill,
    LinkDown,
    LinkUp,
    LinkErrors,
    MigChange,
    #[default]
    #[serde(other)]
    Other,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::GpuError => "gpu_error",
            Kind::GpuRecovered => "gpu_recovered",
            Kind::Throttle => "throttle",
            Kind::ThrottleCleared => "throttle_cleared",
            Kind::SpillStart => "spill_start",
            Kind::SpillEnd => "spill_end",
            Kind::ProcStart => "proc_start",
            Kind::ProcExit => "proc_exit",
            Kind::ProcSpill => "proc_spill",
            Kind::LinkDown => "link_down",
            Kind::LinkUp => "link_up",
            Kind::LinkErrors => "link_errors",
            Kind::MigChange => "mig_change",
            Kind::Other => "other",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// Unix time, seconds.
    pub time: f64,
    pub gpu: Option<usize>,
    pub severity: Severity,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub text: String,
}

/// Events between `prev` and `cur`.
pub fn diff(prev: &Snapshot, cur: &Snapshot) -> Vec<Event> {
    let mut out = Vec::new();
    let mut ev = |gpu: usize, kind: Kind, severity: Severity, pid: Option<u32>, text: String| {
        out.push(Event { time: cur.timestamp, gpu: Some(gpu), severity, kind, pid, text })
    };

    for (i, (a, b)) in prev.gpus.iter().zip(&cur.gpus).enumerate() {
        match (&a.error, &b.error) {
            (None, Some(e)) => ev(i, Kind::GpuError, Severity::Crit, None, format!("GPU {i}: {e}")),
            (Some(_), None) => ev(i, Kind::GpuRecovered, Severity::Info, None, format!("GPU {i} recovered")),
            _ => {}
        }
        // Clock limiters worth knowing about, edge-triggered.
        let notable = |t: &[Throttle]| t.iter().filter(|r| r.severity() > 0).cloned().collect::<Vec<_>>();
        let (was, now) = (notable(&a.throttle), notable(&b.throttle));
        for r in now.iter().filter(|r| !was.contains(r)) {
            let sev = if r.severity() >= 2 { Severity::Crit } else { Severity::Warn };
            ev(i, Kind::Throttle, sev, None, format!("GPU {i}: clocks limited by {}", r.label()));
        }
        for r in was.iter().filter(|r| !now.contains(r)) {
            ev(i, Kind::ThrottleCleared, Severity::Info, None, format!("GPU {i}: {} cleared", r.label()));
        }
        for l in &b.nvlink {
            let Some(was) = a.nvlink.iter().find(|x| x.link == l.link) else { continue };
            match (was.up, l.up) {
                (true, false) => ev(i, Kind::LinkDown, Severity::Crit, None, format!("GPU {i}: NVLink link {} down", l.link)),
                (false, true) => ev(i, Kind::LinkUp, Severity::Info, None, format!("GPU {i}: NVLink link {} back up", l.link)),
                _ => {}
            }
            // Error counters only grow; report each doubling, so a burst
            // gives a handful of events rather than one per second.
            if let (Some(e0), Some(e1)) = (was.errors, l.errors)
                && magnitude(e1) > magnitude(e0)
            {
                ev(i, Kind::LinkErrors, Severity::Warn, None, format!("GPU {i}: NVLink link {} errors at {e1} (+{})", l.link, e1 - e0));
            }
        }
        let gis = |s: &crate::model::GpuSample| s.mig.iter().map(|m| (m.gpu_instance, m.profile.clone())).collect::<Vec<_>>();
        let (was, now) = (gis(a), gis(b));
        for (gi, profile) in now.iter().filter(|m| !was.contains(m)) {
            ev(i, Kind::MigChange, Severity::Info, None, format!("GPU {i}: MIG instance {gi} ({profile}) created"));
        }
        for (gi, profile) in was.iter().filter(|m| !now.contains(m)) {
            ev(i, Kind::MigChange, Severity::Info, None, format!("GPU {i}: MIG instance {gi} ({profile}) removed"));
        }
        if a.spill.level != SpillLevel::Spilling && b.spill.level == SpillLevel::Spilling {
            ev(i, Kind::SpillStart, Severity::Crit, None, format!("GPU {i}: spill started, {}", b.spill.reason));
        } else if a.spill.level == SpillLevel::Spilling && b.spill.level != SpillLevel::Spilling {
            ev(i, Kind::SpillEnd, Severity::Info, None, format!("GPU {i}: spill ended"));
        }
    }

    let before: HashMap<_, _> = prev.processes.iter().map(|p| (p.key(), p)).collect();
    let after: HashSet<_> = cur.processes.iter().map(|p| p.key()).collect();
    for p in &cur.processes {
        let (g, pid) = (p.gpu, p.pid());
        let was = before.get(&p.key());
        if was.is_none() {
            ev(g, Kind::ProcStart, Severity::Info, Some(pid), format!("GPU {g}: {} ({pid}) started", p.name));
        }
        if p.spilling && !was.is_some_and(|q| q.spilling) {
            let bytes = p.usage.gpu_mem_shared.map(fmt::bytes).unwrap_or_default();
            ev(g, Kind::ProcSpill, Severity::Crit, Some(pid), format!("GPU {g}: {} ({pid}) spilled {bytes} into system memory", p.name));
        }
    }
    for p in prev.processes.iter().filter(|p| !after.contains(&p.key())) {
        let (g, pid) = (p.gpu, p.pid());
        ev(g, Kind::ProcExit, Severity::Info, Some(pid), format!("GPU {g}: {} ({pid}) exited", p.name));
    }
    out
}

/// Number of binary digits: 0 for 0, then 1, 2, 2, 3, 3, 3, 3, 4...
fn magnitude(n: u64) -> u32 {
    64 - n.leading_zeros()
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
            container: None,
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
    fn nvlink_and_mig_edges() {
        use crate::model::{MigInstance, NvLink};
        let link = |up, errors| NvLink { link: 3, up, errors: Some(errors), peer_switch: true, ..Default::default() };
        let gpu = |l: NvLink, mig: Vec<MigInstance>| GpuSample { nvlink: vec![l], mig, ..Default::default() };
        let part = MigInstance { gpu_instance: 2, profile: "2g.10gb".into(), ..Default::default() };
        let a = snap(1.0, gpu(link(true, 0), vec![]), vec![]);
        let b = snap(2.0, gpu(link(false, 5), vec![part]), vec![]);
        let e = diff(&a, &b);
        assert!(e.iter().any(|e| e.kind == Kind::LinkDown && e.severity == Severity::Crit));
        assert!(e.iter().any(|e| e.kind == Kind::LinkErrors && e.text.contains("at 5 (+5)")));
        assert!(e.iter().any(|e| e.kind == Kind::MigChange && e.text.contains("2 (2g.10gb) created")));
        // More errors within the same power of two: quiet.
        let c = snap(3.0, gpu(link(false, 7), b.gpus[0].mig.clone()), vec![]);
        assert!(diff(&b, &c).is_empty());
        let d = snap(4.0, gpu(link(true, 9), vec![]), vec![]);
        let e = diff(&c, &d);
        assert!(e.iter().any(|e| e.kind == Kind::LinkUp));
        assert!(e.iter().any(|e| e.kind == Kind::LinkErrors));
        assert!(e.iter().any(|e| e.text.contains("removed")));
    }

    #[test]
    fn process_lifecycle() {
        let a = snap(1.0, GpuSample::default(), vec![proc(1, false)]);
        let b = snap(2.0, GpuSample::default(), vec![proc(2, false)]);
        let e = diff(&a, &b);
        assert!(e.iter().any(|e| e.kind == Kind::ProcStart && e.pid == Some(2)));
        assert!(e.iter().any(|e| e.kind == Kind::ProcExit && e.text.contains("(1) exited")));
    }
}
