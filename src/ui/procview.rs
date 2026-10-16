//! The process list's state: filter, sort, selection and tags. Kept apart
//! from the snapshot so it survives refreshes and rewinding.

use std::cell::RefCell;
use std::collections::HashSet;

use ratatui::widgets::TableState;
use serde::{Deserialize, Serialize};

use super::filter;
use crate::model::ProcRow;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    GpuMem,
    SharedMem,
    Sm,
    Cpu,
    Rss,
    Time,
    Pid,
    User,
    Name,
    Gpu,
}

impl SortKey {
    pub const CYCLE: [SortKey; 10] = [
        SortKey::GpuMem,
        SortKey::SharedMem,
        SortKey::Sm,
        SortKey::Cpu,
        SortKey::Rss,
        SortKey::Time,
        SortKey::Pid,
        SortKey::User,
        SortKey::Name,
        SortKey::Gpu,
    ];
    pub fn name(self) -> &'static str {
        match self {
            SortKey::GpuMem => "gpu_mem",
            SortKey::SharedMem => "shared_mem",
            SortKey::Sm => "sm",
            SortKey::Cpu => "cpu",
            SortKey::Rss => "rss",
            SortKey::Time => "time",
            SortKey::Pid => "pid",
            SortKey::User => "user",
            SortKey::Name => "name",
            SortKey::Gpu => "gpu",
        }
    }

    /// A name or a common alias ("mem", "spill", "util", ...).
    pub fn parse(s: &str) -> Option<SortKey> {
        Self::CYCLE.into_iter().find(|k| k.name() == s).or(match s {
            "mem" | "vram" => Some(SortKey::GpuMem),
            "spill" | "shared" | "sys" => Some(SortKey::SharedMem),
            "util" | "gpu%" => Some(SortKey::Sm),
            "ram" => Some(SortKey::Rss),
            "command" | "cmd" => Some(SortKey::Name),
            _ => None,
        })
    }

    /// Natural direction: numbers biggest-first, text A-Z.
    pub fn default_desc(self) -> bool {
        !matches!(self, SortKey::Pid | SortKey::User | SortKey::Name | SortKey::Gpu)
    }
    pub fn step(self, d: isize) -> SortKey {
        let i = Self::CYCLE.iter().position(|&s| s == self).unwrap_or(0) as isize;
        Self::CYCLE[(i + d).rem_euclid(Self::CYCLE.len() as isize) as usize]
    }
}

/// How the process list is filtered and ordered.
#[derive(Clone, Debug)]
pub struct ViewSpec {
    pub filter: String,
    pub gpu: Option<usize>,
    pub sort: SortKey,
    pub desc: bool,
}

/// Indices into `procs`, filtered and sorted per `v`. Pure, so it is tested.
pub fn filter_sort(procs: &[ProcRow], v: &ViewSpec) -> Vec<usize> {
    let f = filter::Filter::parse(&v.filter);
    let mut idx: Vec<usize> = (0..procs.len()).filter(|&i| v.gpu.is_none_or(|g| procs[i].gpu == g) && f.matches(&procs[i])).collect();
    // Sorting by a column nobody reports (GPU memory under WDDM without
    // counters, Apple) is useless; fall back to GPU utilization.
    let key = match v.sort {
        SortKey::GpuMem if idx.iter().all(|&i| procs[i].usage.gpu_mem.is_none()) => SortKey::Sm,
        k => k,
    };
    let num = |p: &ProcRow| -> f64 {
        let u = &p.usage;
        match key {
            SortKey::GpuMem => u.gpu_mem.map(|v| v as f64),
            SortKey::SharedMem => u.gpu_mem_shared.map(|v| v as f64),
            SortKey::Sm => u.sm_util.map(f64::from),
            SortKey::Cpu => p.cpu.map(f64::from),
            SortKey::Rss => p.rss.map(|v| v as f64),
            SortKey::Time => p.runtime.map(|v| v as f64),
            _ => None,
        }
        .unwrap_or(-1.0)
    };
    idx.sort_by(|&a, &b| {
        let (pa, pb) = (&procs[a], &procs[b]);
        let o = match key {
            SortKey::Pid => pa.pid().cmp(&pb.pid()),
            SortKey::User => pa.user.cmp(&pb.user),
            SortKey::Name => pa.name.to_lowercase().cmp(&pb.name.to_lowercase()),
            SortKey::Gpu => pa.gpu.cmp(&pb.gpu),
            _ => num(pa).total_cmp(&num(pb)),
        };
        let o = if v.desc { o.reverse() } else { o };
        o.then(pa.gpu.cmp(&pb.gpu)).then(pa.pid().cmp(&pb.pid()))
    });
    idx
}

pub struct ProcView {
    /// Visible rows: indices into the snapshot's processes, in display order.
    pub rows: Vec<usize>,
    pub spec: ViewSpec,
    /// Only show the selected GPU's processes.
    pub gpu_only: bool,
    pub table: RefCell<TableState>,
    /// The selected process; the selection follows it across refreshes.
    pub sel_key: Option<(usize, u32)>,
    pub filter_edit: bool,
    pub full_cmd: bool,
    pub tagged: HashSet<(usize, u32)>,
}

impl ProcView {
    pub fn new(sort: SortKey, desc: Option<bool>, full_cmd: bool) -> Self {
        ProcView {
            rows: Vec::new(),
            spec: ViewSpec { filter: String::new(), gpu: None, sort, desc: desc.unwrap_or(sort.default_desc()) },
            gpu_only: false,
            table: RefCell::new(TableState::default().with_selected(Some(0))),
            sel_key: None,
            filter_edit: false,
            full_cmd,
            tagged: HashSet::new(),
        }
    }

    pub fn row<'a>(&self, procs: &'a [ProcRow], n: usize) -> Option<&'a ProcRow> {
        procs.get(*self.rows.get(n)?)
    }

    /// Recompute visible rows and keep the selection on the same process.
    pub fn refresh(&mut self, procs: &[ProcRow], sel_gpu: usize) {
        self.spec.gpu = self.gpu_only.then_some(sel_gpu);
        self.rows = filter_sort(procs, &self.spec);
        let n = self.rows.len();
        let idx = self
            .sel_key
            .and_then(|k| self.rows.iter().position(|&i| procs[i].key() == k))
            .unwrap_or_else(|| self.table.borrow().selected().unwrap_or(0).min(n.saturating_sub(1)));
        self.sel_key = self.row(procs, idx).map(ProcRow::key);
        self.table.borrow_mut().select(if n == 0 { None } else { Some(idx) });
    }

    pub fn move_sel(&mut self, procs: &[ProcRow], delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let cur = self.table.borrow().selected().unwrap_or(0) as isize;
        let idx = cur.saturating_add(delta).clamp(0, self.rows.len() as isize - 1) as usize;
        self.sel_key = self.row(procs, idx).map(ProcRow::key);
        self.table.borrow_mut().select(Some(idx));
    }

    /// Sort by `k`; picking the current key again flips the direction.
    pub fn set_sort(&mut self, k: SortKey) {
        if self.spec.sort == k {
            self.spec.desc = !self.spec.desc;
        } else {
            self.spec.sort = k;
            self.spec.desc = k.default_desc();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GpuProcess, PidMapping};

    fn row(gpu: usize, pid: u32, mem: Option<u64>, sm: Option<f32>, cmd: &str) -> ProcRow {
        ProcRow {
            gpu,
            usage: GpuProcess { pid, gpu_mem: mem, sm_util: sm, ..Default::default() },
            local_pid: Some(pid),
            mapping: PidMapping::Direct,
            name: cmd.split(' ').next().unwrap_or("").into(),
            cmdline: cmd.into(),
            user: Some("alice".into()),
            cpu: None,
            rss: None,
            runtime: None,
            spilling: false,
            container: None,
        }
    }

    fn spec(sort: SortKey) -> ViewSpec {
        ViewSpec { filter: String::new(), gpu: None, sort, desc: sort.default_desc() }
    }

    #[test]
    fn sorts_by_memory_desc_with_none_last() {
        let p = [row(0, 1, Some(10), None, "a"), row(0, 2, None, None, "b"), row(0, 3, Some(30), None, "c")];
        assert_eq!(filter_sort(&p, &spec(SortKey::GpuMem)), [2, 0, 1]);
    }

    #[test]
    fn memory_sort_falls_back_to_utilization() {
        let p = [row(0, 1, None, Some(5.0), "a"), row(0, 2, None, Some(50.0), "b")];
        assert_eq!(filter_sort(&p, &spec(SortKey::GpuMem)), [1, 0]);
    }

    #[test]
    fn filters_by_text_pid_and_gpu() {
        let p = [row(0, 1, None, None, "python train.py"), row(1, 22, None, None, "ollama serve")];
        let mut v = spec(SortKey::Pid);
        v.filter = "TRAIN".into();
        assert_eq!(filter_sort(&p, &v), [0]);
        v.filter = "22".into();
        assert_eq!(filter_sort(&p, &v), [1]);
        v.filter.clear();
        v.gpu = Some(1);
        assert_eq!(filter_sort(&p, &v), [1]);
    }

    #[test]
    fn ties_break_by_gpu_then_pid() {
        let p = [row(1, 5, Some(1), None, "x"), row(0, 9, Some(1), None, "x"), row(0, 3, Some(1), None, "x")];
        assert_eq!(filter_sort(&p, &spec(SortKey::GpuMem)), [2, 1, 0]);
    }
}
