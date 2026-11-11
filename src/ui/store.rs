//! Everything time-related in the UI: the live snapshot, the rewind buffer,
//! chart history, the event log and the on-disk recording.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use super::history::History;
use crate::events::Event;
use crate::model::{Snapshot, SystemInfo};
use crate::recording::{self, KEEP, Retention, Writer, trimmed};

const EVENTS_KEEP: usize = 2000;
/// Recordings older than this aren't loaded at startup.
const LOAD_MAX_AGE: f64 = 24.0 * 3600.0;

#[derive(Default)]
pub struct Store {
    /// Newest snapshot, complete.
    live: Option<Snapshot>,
    /// Recent snapshots, oldest first, with trimmed command lines.
    past: VecDeque<Arc<Snapshot>>,
    /// How many snapshots back from the newest are shown (0 = live).
    pub rewind: usize,
    recorder: Option<Writer>,
    /// Replaying a recording (no live data).
    pub replay: bool,
    pub hist: History,
    pub events: VecDeque<Event>,
    /// Bumped whenever what's on screen changes; keys the chart image cache.
    pub version: u64,
}

impl Store {
    /// Load this machine's recent recordings from `dir` (seeding history,
    /// the rewind buffer and the event log) and keep recording there.
    pub fn open(info: &SystemInfo, interval: f64, dir: PathBuf, keep: Retention) -> Store {
        recording::remove_legacy();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let ids: Vec<String> = info.gpus.iter().map(|g| g.stable_id()).collect();
        let past = recording::load_recent(&dir, Some(&ids), now - LOAD_MAX_AGE, KEEP).snaps;
        let mut st = Store::seeded(info, past, interval);
        st.recorder = Some(Writer::new(dir, info, interval, keep));
        st
    }

    /// A recording to look at: no live data, starting at the first snapshot.
    pub fn replay(info: &SystemInfo, snaps: Vec<Snapshot>, interval: f64) -> Store {
        let mut st = Store::seeded(info, snaps, interval);
        st.replay = true;
        st.set_rewind(usize::MAX);
        st
    }

    fn seeded(info: &SystemInfo, past: Vec<Snapshot>, interval: f64) -> Store {
        let mut st = Store::default();
        for s in past {
            st.hist.push(info, &s, interval, false);
            st.push_events(&s.events);
            st.past.push_back(Arc::new(s));
        }
        st.live = st.past.back().map(|s| (**s).clone());
        st
    }

    /// Where this session records, while it does (another process may hold
    /// the folder).
    pub fn recording_to(&self) -> Option<&std::path::Path> {
        self.recorder.as_ref().filter(|r| r.writing()).map(|r| r.dir())
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
        if self.past.len() > KEEP {
            self.past.pop_front();
        }
        match self.recorder.as_mut().and_then(|r| r.write(&keep)) {
            Some(Ok(m)) => log::info!("{m}"),
            Some(Err(m)) => log::warn!("{m}"),
            None => {}
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
}
