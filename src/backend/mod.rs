//! GPU backends. Each vendor implements [`Backend`]; [`detect`] loads every
//! backend that works on this machine and they're shown side by side.
//!
//! Adding a vendor (AMD sysfs/fdinfo, Apple IOReport, Intel, Windows PDH...)
//! means adding a module here and a line in `detect`.

#[cfg(target_os = "linux")]
pub mod amd;
#[cfg(target_os = "macos")]
pub mod apple;
pub mod demo;
#[cfg(target_os = "linux")]
pub mod drm_fdinfo;
pub mod nvml;

use crate::model::{BackendInfo, GpuInfo, GpuSample};

pub trait Backend: Send {
    fn info(&self) -> BackendInfo;
    /// Static GPU info. `index` is local to the backend; the registry
    /// renumbers globally.
    fn gpus(&self) -> Vec<GpuInfo>;
    /// One sample per GPU, same order as `gpus()`.
    fn sample(&mut self) -> Vec<GpuSample>;
}

pub struct Detected {
    pub backends: Vec<Box<dyn Backend>>,
    pub warnings: Vec<String>,
}

pub fn detect(demo: Option<usize>) -> Detected {
    let mut backends: Vec<Box<dyn Backend>> = Vec::new();
    let mut warnings = Vec::new();

    if let Some(n) = demo {
        backends.push(Box::new(demo::Demo::new(n)));
        return Detected { backends, warnings };
    }

    #[cfg(target_os = "macos")]
    {
        match apple::Apple::new() {
            Ok(b) => backends.push(Box::new(b)),
            Err(e) => warnings.push(format!("Apple GPU: {e}")),
        }
        return Detected { backends, warnings };
    }

    #[allow(unreachable_code)]
    match nvml::Nvml::new() {
        Ok(b) => backends.push(Box::new(b)),
        Err(e) => warnings.push(e),
    }

    #[cfg(target_os = "linux")]
    if let Ok(b) = amd::Amd::new() {
        // NVIDIA missing is only worth a warning if nothing else was found.
        if backends.is_empty() {
            warnings.clear();
        }
        backends.push(Box::new(b));
    }

    Detected { backends, warnings }
}
