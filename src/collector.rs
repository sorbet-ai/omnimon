//! Collects a full [`Snapshot`] from all backends + the host, and runs that
//! on a background thread for the TUI.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::backend::{Backend, Detected};
use crate::host::Host;
use crate::model::{Snapshot, SystemInfo};

pub struct Collector {
    backends: Vec<Box<dyn Backend>>,
    /// Global index of each backend's first GPU.
    offsets: Vec<usize>,
    host: Host,
    #[cfg(windows)]
    pdh: Option<(crate::winpdh::Pdh, Vec<Option<crate::winpdh::Luid>>)>,
    #[cfg(windows)]
    mem_totals: Vec<Option<u64>>,
}

impl Collector {
    pub fn new(d: Detected) -> (Self, SystemInfo) {
        let host = Host::new();
        let mut info = SystemInfo { host: host.info.clone(), warnings: d.warnings, ..Default::default() };
        let mut offsets = Vec::new();
        for b in &d.backends {
            offsets.push(info.gpus.len());
            info.backends.push(b.info());
            for mut g in b.gpus() {
                g.index = info.gpus.len();
                info.gpus.push(g);
            }
        }
        if info.gpus.is_empty() {
            info.warnings.push("no GPUs found".into());
        }
        if info.host.wsl && info.gpus.iter().any(|g| g.vendor != crate::model::Vendor::Demo) {
            info.warnings.push("WSL: the driver doesn't expose per-process GPU data here".into());
        }
        #[cfg(windows)]
        let pdh = crate::winpdh::Pdh::new().map(|p| {
            // Match each GPU to its WDDM adapter by PCI location.
            let luids = info
                .gpus
                .iter()
                .map(|g| {
                    let loc = g.pci_bus_id.as_deref().and_then(crate::winpdh::parse_bus_id)?;
                    p.adapters.iter().find(|(_, a)| **a == loc).map(|(l, _)| *l)
                })
                .collect();
            (p, luids)
        });
        (
            Collector {
                backends: d.backends,
                offsets,
                host,
                #[cfg(windows)]
                pdh,
                #[cfg(windows)]
                mem_totals: info.gpus.iter().map(|g| g.memory_total).collect(),
            },
            info,
        )
    }

    pub fn collect(&mut self) -> Snapshot {
        let mut gpus = Vec::new();
        let mut procs = Vec::new();
        for (b, &off) in self.backends.iter_mut().zip(&self.offsets) {
            for (i, mut s) in b.sample().into_iter().enumerate() {
                for p in s.processes.drain(..) {
                    procs.push((off + i, p));
                }
                gpus.push(s);
            }
        }
        #[cfg(windows)]
        if let Some((pdh, luids)) = &mut self.pdh {
            merge_pdh(pdh.sample(), luids, &self.mem_totals, &mut procs);
        }
        // Keep processes on the sample too (for JSON consumers), but the
        // joined table is what the UI uses.
        let rows = self.host.processes(procs);
        for r in &rows {
            if let Some(g) = gpus.get_mut(r.gpu) {
                g.processes.push(crate::model::GpuProcess {
                    pid: r.pid,
                    kind: r.kind,
                    gpu_mem: r.gpu_mem,
                    sm_util: r.sm_util,
                    mem_util: r.mem_util,
                    enc_util: r.enc_util,
                    dec_util: r.dec_util,
                    gpu_instance: None,
                    name: Some(r.name.clone()),
                });
            }
        }
        Snapshot {
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
            host: self.host.sample(),
            gpus,
            processes: rows,
        }
    }
}

/// Fill in what NVML can't report under WDDM (per-process memory and
/// utilization) and add GPU processes it didn't list.
#[cfg(windows)]
fn merge_pdh(
    usage: std::collections::HashMap<(crate::winpdh::Luid, u32), crate::winpdh::ProcUsage>,
    luids: &[Option<crate::winpdh::Luid>],
    totals: &[Option<u64>],
    procs: &mut Vec<(usize, crate::model::GpuProcess)>,
) {
    use crate::model::{GpuProcess, ProcKind};
    // The counters occasionally report garbage (terabytes); drop anything
    // larger than the card.
    let sane = |gpu: usize, v: Option<u64>| v.filter(|&b| totals.get(gpu).copied().flatten().is_none_or(|t| b <= t));
    let mut seen = std::collections::HashSet::new();
    for (gpu, p) in procs.iter_mut() {
        let Some(Some(luid)) = luids.get(*gpu) else { continue };
        seen.insert((*gpu, p.pid));
        if let Some(u) = usage.get(&(*luid, p.pid)) {
            p.gpu_mem = p.gpu_mem.or(sane(*gpu, u.dedicated));
            p.sm_util = p.sm_util.or(u.util);
            p.enc_util = p.enc_util.or(u.enc);
            p.dec_util = p.dec_util.or(u.dec);
        }
    }
    for ((luid, pid), u) in usage {
        let Some(gpu) = luids.iter().position(|l| *l == Some(luid)) else { continue };
        if seen.contains(&(gpu, pid)) || (u.dedicated.unwrap_or(0) == 0 && u.util.unwrap_or(0.0) <= 0.0) {
            continue;
        }
        procs.push((
            gpu,
            GpuProcess {
                pid,
                kind: ProcKind::Graphics,
                gpu_mem: sane(gpu, u.dedicated),
                sm_util: u.util,
                mem_util: None,
                enc_util: u.enc,
                dec_util: u.dec,
                gpu_instance: None,
                name: None,
            },
        ));
    }
}

/// Shared knobs between the UI and the sampler thread.
pub struct Control {
    pub interval_ms: AtomicU64,
    pub paused: AtomicBool,
    stop: AtomicBool,
}

pub struct Sampler {
    pub rx: Receiver<Snapshot>,
    pub ctl: Arc<Control>,
    thread: Option<JoinHandle<()>>,
}

impl Sampler {
    pub fn spawn(mut c: Collector, interval: Duration) -> Self {
        let (tx, rx): (Sender<Snapshot>, _) = channel();
        let ctl = Arc::new(Control {
            interval_ms: AtomicU64::new(interval.as_millis() as u64),
            paused: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let ctl2 = ctl.clone();
        let thread = std::thread::Builder::new()
            .name("sampler".into())
            .spawn(move || {
                while !ctl2.stop.load(Ordering::Relaxed) {
                    let t0 = Instant::now();
                    if !ctl2.paused.load(Ordering::Relaxed) && tx.send(c.collect()).is_err() {
                        break;
                    }
                    let iv = Duration::from_millis(ctl2.interval_ms.load(Ordering::Relaxed));
                    let wait = iv.saturating_sub(t0.elapsed());
                    std::thread::park_timeout(wait);
                }
            })
            .expect("spawn sampler");
        Sampler { rx, ctl, thread: Some(thread) }
    }

    /// Sample now (after an interval change, unpause, ...).
    pub fn poke(&self) {
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.ctl.stop.store(true, Ordering::Relaxed);
        self.poke();
        // Don't join: NVML calls can block; the process is exiting anyway.
    }
}
