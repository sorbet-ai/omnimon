//! GPU backends. Each vendor implements [`Backend`]; [`detect`] loads every
//! backend that works on this machine and they're shown side by side.
//!
//! Adding a vendor means adding a module here and a line in `detect`.

#[cfg(target_os = "linux")]
pub mod amd;
#[cfg(target_os = "macos")]
pub mod apple;
pub mod demo;
#[cfg(target_os = "linux")]
pub mod drm_fdinfo;
#[cfg(target_os = "linux")]
pub mod intel;
#[cfg(not(target_os = "macos"))]
pub mod nvml;
#[cfg(target_os = "linux")]
mod sysfs;
#[cfg(windows)]
pub mod wddm;

use crate::model::{BackendInfo, GpuInfo, GpuSample};

pub trait Backend: Send {
    fn info(&self) -> BackendInfo;
    /// Static GPU info. `index` is local to the backend; the registry
    /// renumbers globally.
    fn gpus(&self) -> Vec<GpuInfo>;
    /// One sample per GPU, same order as `gpus()`.
    fn sample(&mut self) -> Vec<GpuSample>;
    /// Change a device setting. `gpu` is the backend-local index.
    fn apply(&mut self, gpu: usize, cmd: &Tune) -> Result<String, String> {
        let _ = (gpu, cmd);
        Err("not supported for this GPU".into())
    }
}

/// Device settings a backend may support changing (usually needs admin/root).
#[derive(Clone, Debug, PartialEq)]
pub enum Tune {
    /// Watts; `None` restores the default.
    PowerLimit(Option<f64>),
}

pub struct Detected {
    pub backends: Vec<Box<dyn Backend>>,
    pub warnings: Vec<String>,
}

pub fn detect(demo: Option<usize>) -> Detected {
    if let Some(n) = demo {
        return Detected { backends: vec![Box::new(demo::Demo::new(n))], warnings: Vec::new() };
    }

    let mut backends: Vec<Box<dyn Backend>> = Vec::new();
    let mut failures = Vec::new();
    let mut add = |backends: &mut Vec<Box<dyn Backend>>, name: &str, r: Result<Box<dyn Backend>, String>| match r {
        Ok(b) => backends.push(b),
        Err(e) => {
            log::debug!("{name} backend: {e}");
            failures.push(format!("{name}: {e}"));
        }
    };

    #[cfg(target_os = "macos")]
    add(&mut backends, "Apple", apple::Apple::new().map(|b| Box::new(b) as Box<dyn Backend>));

    #[cfg(not(target_os = "macos"))]
    add(&mut backends, "NVIDIA", nvml::Nvml::new().map(|b| Box::new(b) as Box<dyn Backend>));

    #[cfg(target_os = "linux")]
    add(&mut backends, "AMD", amd::Amd::new().map(|b| Box::new(b) as Box<dyn Backend>));
    #[cfg(target_os = "linux")]
    add(&mut backends, "Intel", intel::Intel::new().map(|b| Box::new(b) as Box<dyn Backend>));

    // Everything else on Windows (AMD, Intel, NVIDIA without NVML), skipping
    // adapters a richer backend already covers.
    #[cfg(windows)]
    {
        let covered: Vec<(u32, u32, u32)> = backends
            .iter()
            .flat_map(|b| b.gpus())
            .filter_map(|g| g.pci_bus_id.as_deref().and_then(crate::os::windows::parse_bus_id))
            .collect();
        add(&mut backends, "WDDM", wddm::Wddm::new(&covered).map(|b| Box::new(b) as Box<dyn Backend>));
    }

    // A missing vendor library is only news when nothing else was found.
    let warnings = if backends.is_empty() { failures } else { Vec::new() };
    Detected { backends, warnings }
}
