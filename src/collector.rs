//! Collects a full [`Snapshot`] from all backends + the host, and runs that
//! on a background thread for the TUI and the exporter.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::backend::{Backend, Detected};
use crate::host::Host;
use crate::model::{GpuInfo, GpuSample, ProcRow, Snapshot, SystemInfo, Vendor};
use crate::os::ProcEnricher;
use crate::spill;

pub struct Collector {
    backends: Vec<Box<dyn Backend>>,
    /// Number of GPUs each backend reported at startup.
    counts: Vec<usize>,
    gpus: Vec<GpuInfo>,
    host: Host,
    enricher: ProcEnricher,
    pub thresholds: spill::Thresholds,
    prev: Option<Snapshot>,
}

impl Collector {
    pub fn new(d: Detected) -> (Self, SystemInfo) {
        let host = Host::new();
        let mut info = SystemInfo { host: host.info.clone(), warnings: d.warnings, ..Default::default() };
        let mut counts = Vec::new();
        for (bi, b) in d.backends.iter().enumerate() {
            info.backends.push(b.info());
            let gpus = b.gpus();
            counts.push(gpus.len());
            for mut g in gpus {
                // The collector owns global numbering.
                g.index = info.gpus.len();
                g.backend = bi;
                info.gpus.push(g);
            }
        }
        if info.gpus.is_empty() {
            info.warnings.push("no GPUs found".into());
        }
        if info.host.wsl && info.gpus.iter().any(|g| g.vendor != Vendor::Demo) {
            info.warnings.push("WSL: the driver doesn't expose per-process GPU data here".into());
        }
        let enricher = ProcEnricher::new(&info.gpus);
        let c = Collector {
            backends: d.backends,
            counts,
            gpus: info.gpus.clone(),
            host,
            enricher,
            thresholds: spill::Thresholds::default(),
            prev: None,
        };
        (c, info)
    }

    pub fn collect(&mut self) -> Snapshot {
        // Backends run in parallel so a slow or hung one (NVML on a GPU that
        // fell off the bus) doesn't hold up the others.
        let per_backend: Vec<Vec<GpuSample>> = if self.backends.len() <= 1 {
            self.backends.iter_mut().map(|b| b.sample()).collect()
        } else {
            std::thread::scope(|s| {
                let hs: Vec<_> = self.backends.iter_mut().map(|b| s.spawn(move || b.sample())).collect();
                hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
            })
        };

        let mut gpus = Vec::with_capacity(self.gpus.len());
        let mut procs = Vec::new();
        for (mut samples, &n) in per_backend.into_iter().zip(&self.counts) {
            // Keep indices aligned with `SystemInfo::gpus` whatever a backend returns.
            samples.resize_with(n, || GpuSample { error: Some("no sample".into()), ..Default::default() });
            for mut s in samples {
                let i = gpus.len();
                procs.extend(s.processes.drain(..).map(|p| (i, p)));
                gpus.push(s);
            }
        }

        self.enricher.enrich(&mut gpus, &mut procs);
        let mut rows = self.host.processes(procs);
        for (i, g) in gpus.iter_mut().enumerate() {
            let mut mine: Vec<&mut ProcRow> = rows.iter_mut().filter(|r| r.gpu == i).collect();
            g.spill = spill::analyze(&self.gpus[i], g, &mut mine, &self.thresholds);
        }

        let mut snap = Snapshot {
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
            host: self.host.sample(),
            gpus,
            processes: rows,
            events: Vec::new(),
        };
        if let Some(prev) = &self.prev {
            snap.events = crate::events::diff(prev, &snap);
        }
        self.prev = Some(Snapshot { events: Vec::new(), ..snap.clone() });
        snap
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
        // Capacity 1: if the consumer stalls (terminal suspended), drop stale
        // snapshots instead of queueing them.
        let (tx, rx): (SyncSender<Snapshot>, _) = sync_channel(1);
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
                    if !ctl2.paused.load(Ordering::Relaxed) {
                        match tx.try_send(c.collect()) {
                            Ok(()) | Err(TrySendError::Full(_)) => {}
                            Err(TrySendError::Disconnected(_)) => break,
                        }
                    }
                    let iv = Duration::from_millis(ctl2.interval_ms.load(Ordering::Relaxed));
                    std::thread::park_timeout(iv.saturating_sub(t0.elapsed()));
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
