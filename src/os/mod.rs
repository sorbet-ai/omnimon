//! OS-specific plumbing behind cfg-free facades, so the rest of the app
//! never needs `#[cfg(target_os)]`.

// The mapping logic is portable (and tested everywhere); only Linux uses it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub mod pidmap;
#[cfg(windows)]
pub mod windows;

use crate::model::{GpuInfo, GpuProcess, GpuSample};

/// Adds per-process data the GPU backends can't see (Windows WDDM counters).
/// A no-op elsewhere.
pub struct ProcEnricher {
    #[cfg(windows)]
    inner: Option<windows::Enricher>,
}

impl ProcEnricher {
    pub fn new(gpus: &[GpuInfo]) -> Self {
        let _ = gpus;
        ProcEnricher {
            #[cfg(windows)]
            inner: windows::Enricher::new(gpus),
        }
    }

    pub fn enrich(&mut self, samples: &mut [GpuSample], procs: &mut Vec<(usize, GpuProcess)>) {
        let _ = (&samples, &procs);
        #[cfg(windows)]
        if let Some(e) = &mut self.inner {
            e.enrich(samples, procs);
        }
    }
}

/// Maps driver PIDs to local PIDs (Linux containers); identity elsewhere.
pub struct PidResolver {
    #[cfg(target_os = "linux")]
    inner: pidmap::PidMapper<pidmap::LinuxProc>,
}

impl PidResolver {
    pub fn new(in_container: bool) -> Self {
        let _ = in_container;
        PidResolver {
            #[cfg(target_os = "linux")]
            inner: pidmap::PidMapper::with_fs(pidmap::LinuxProc, in_container),
        }
    }

    pub fn resolve(&mut self, pids: &[u32]) -> pidmap::Resolution {
        #[cfg(target_os = "linux")]
        {
            self.inner.resolve(pids)
        }
        #[cfg(not(target_os = "linux"))]
        {
            pids.iter().map(|&p| (p, (Some(p), crate::model::PidMapping::Direct))).collect()
        }
    }
}
