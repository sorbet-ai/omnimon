//! Everything time-related in the UI: the live snapshot, the rewind buffer,
//! chart history, the event log and the on-disk recording.

use std::collections::VecDeque;
use std::sync::Arc;

use super::history::History;
use super::recorder::{self, Recorder};
use crate::events::Event;
use crate::model::{Snapshot, SystemInfo};

const EVENTS_KEEP: usize = 2000;
/// Command lines kept in the rewind buffer (the live view has them in full).
const CMDLINE_KEEP: usize = 256;

#[derive(Default)]
pub struct Store {
    /// Newest snapshot, complete.
    live: Option<Snapshot>,
    /// Recent snapshots, oldest first, with trimmed command lines.
    past: VecDeque<Arc<Snapshot>>,
    /// How many snapshots back from the newest are shown (0 = live).
    pub rewind: usize,
    recorder: Option<Recorder>,
    pub hist: History,
    pub events: VecDeque<Event>,
    /// Bumped whenever what's on screen changes; keys the chart image cache.
    pub version: u64,
}

/// Host (CPU-only) processes kept per past snapshot: the busiest few that
/// are actually doing something, with short command lines. The live view has
/// the full list; this keeps the rewind buffer and the history file small.
const HOST_PROCS_KEEP: usize = 8;
const HOST_PROC_MIN_CPU: f32 = 1.0;
const HOST_CMDLINE_KEEP: usize = 96;

fn trimmed(s: &Snapshot) -> Snapshot {
    let mut s = s.clone();
    s.host_processes.retain(|p| p.cpu.unwrap_or(0.0) >= HOST_PROC_MIN_CPU);
    s.host_processes.sort_by(|a, b| b.cpu.unwrap_or(0.0).total_cmp(&a.cpu.unwrap_or(0.0)));
    s.host_processes.truncate(HOST_PROCS_KEEP);
    for p in &mut s.processes {
        p.cmdline.truncate(p.cmdline.floor_char_boundary(CMDLINE_KEEP));
    }
    for p in &mut s.host_processes {
        p.cmdline.truncate(p.cmdline.floor_char_boundary(HOST_CMDLINE_KEEP));
    }
    s
}

impl Store {
    /// Load the recording for this machine (unless `record` is false) and
    /// seed history, the rewind buffer and the event log from it.
    pub fn open(info: &SystemInfo, interval: f64, record: bool) -> Store {
        let mut st = Store::default();
        if !record {
            return st;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let (rec, past) = Recorder::open(info, now);
        st.recorder = Some(rec);
        for s in past {
            st.hist.push(info, &s, interval, false);
            st.push_events(&s.events);
            st.past.push_back(Arc::new(s));
        }
        st.live = st.past.back().map(|s| (**s).clone());
        st
    }

    /// The snapshot on screen.
    pub fn current(&self) -> Option<&Snapshot> {
        if self.rewind == 0 {
            return self.live.as_ref();
        }
        self.past.get(self.past.len().checked_sub(1 + self.rewind)?).map(|s| &**s)
    }

    /// The newest snapshot (for "how long ago" while rewinding).
    pub fn newest(&self) -> Option<&Snapshot> {
        self.live.as_ref()
    }

    fn push_events(&mut self, events: &[Event]) {
        self.events.extend(events.iter().cloned());
        while self.events.len() > EVENTS_KEEP {
            self.events.pop_front();
        }
    }

    pub fn ingest(&mut self, info: &SystemInfo, s: Snapshot, interval: f64) {
        self.hist.push(info, &s, interval, true);
        self.push_events(&s.events);
        let keep = Arc::new(trimmed(&s));
        self.past.push_back(keep.clone());
        if self.past.len() > recorder::KEEP {
            self.past.pop_front();
        }
        if let Some(r) = &mut self.recorder
            && r.append(&keep)
        {
            r.rewrite(self.past.iter().map(|s| &**s));
        }
        self.live = Some(s);
        self.version += 1;
        if self.rewind > 0 {
            // Stay on the same moment while new data arrives.
            self.set_rewind(self.rewind + 1);
        }
    }

    pub fn set_rewind(&mut self, n: usize) {
        self.rewind = n.min(self.past.len().saturating_sub(1));
        self.hist.at = if self.rewind == 0 { None } else { self.current().map(|s| s.timestamp) };
        self.version += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GpuInfo, GpuSample};

    fn snap(t: f64, util: f32) -> Snapshot {
        Snapshot { timestamp: t, gpus: vec![GpuSample { util_gpu: Some(util), ..Default::default() }], ..Default::default() }
    }

    #[test]
    fn rewind_keeps_snapshot_and_charts_aligned_across_gaps() {
        let info = SystemInfo { gpus: vec![GpuInfo::default()], ..Default::default() };
        let mut st = Store::default();
        for (t, u) in [(0.0, 10.0), (1.0, 20.0), (500.0, 30.0), (501.0, 40.0)] {
            st.ingest(&info, snap(t, u), 1.0);
        }
        st.set_rewind(2);
        assert_eq!(st.current().unwrap().timestamp, 1.0);
        let chart = st.hist.tail(&st.hist.gpus[0].util, 10);
        assert_eq!(chart.last(), Some(&Some(0.2)), "chart ends at the shown snapshot: {chart:?}");
        // New data keeps the view on the same moment.
        st.ingest(&info, snap(502.0, 50.0), 1.0);
        assert_eq!(st.current().unwrap().timestamp, 1.0);
        st.set_rewind(0);
        assert_eq!(st.current().unwrap().timestamp, 502.0);
    }

    #[test]
    fn past_snapshots_keep_only_busy_host_processes() {
        use crate::model::{GpuProcess, NO_GPU, ProcRow};
        let host = |pid: u32, cpu: f32| ProcRow {
            gpu: NO_GPU,
            usage: GpuProcess { pid, ..Default::default() },
            cpu: Some(cpu),
            cmdline: "x".repeat(500),
            ..Default::default()
        };
        let mut s = snap(0.0, 0.0);
        s.host_processes = (0..20).map(|i| host(i, i as f32)).collect();
        let t = trimmed(&s);
        let kept: Vec<u32> = t.host_processes.iter().map(|p| p.pid()).collect();
        assert_eq!(kept, [19, 18, 17, 16, 15, 14, 13, 12], "busiest first, capped");
        assert!(t.host_processes.iter().all(|p| p.cmdline.len() == HOST_CMDLINE_KEEP));

        s.host_processes = vec![host(1, 0.2), host(2, 0.0)];
        assert!(trimmed(&s).host_processes.is_empty(), "idle processes aren't recorded");
        assert_eq!(s.host_processes.len(), 2, "the live snapshot is untouched");
    }
}
