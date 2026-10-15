//! Vendor-neutral data model. Every backend (NVIDIA, AMD, Apple, ...) fills
//! these structs; anything a vendor can't provide is `None`/empty and the UI
//! simply hides it.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Demo,
}

impl Vendor {
    pub fn label(self) -> &'static str {
        match self {
            Vendor::Nvidia => "NVIDIA",
            Vendor::Amd => "AMD",
            Vendor::Intel => "Intel",
            Vendor::Apple => "Apple",
            Vendor::Demo => "Demo",
        }
    }
}

/// Information about a backend (one per vendor library that loaded).
#[derive(Clone, Debug, Default, Serialize)]
pub struct BackendInfo {
    pub name: String,
    pub library: Option<String>,
    pub driver_version: Option<String>,
    pub api_version: Option<String>,
    /// e.g. CUDA driver version "13.0".
    pub runtime_version: Option<String>,
}

/// Mostly-static properties of a GPU, queried once.
#[derive(Clone, Debug, Serialize)]
pub struct GpuInfo {
    pub index: usize,
    pub vendor: Vendor,
    pub name: String,
    pub uuid: Option<String>,
    pub pci_bus_id: Option<String>,
    pub serial: Option<String>,
    pub vbios: Option<String>,
    pub architecture: Option<String>,
    pub compute_capability: Option<String>,
    pub cores: Option<u32>,
    pub memory_total: Option<u64>,
    /// Unified memory (APUs / Apple Silicon): VRAM is system RAM.
    pub integrated: bool,
    pub power_limit_default: Option<f64>,
    pub power_limit_min: Option<f64>,
    pub power_limit_max: Option<f64>,
    pub max_clock_graphics: Option<u32>,
    pub max_clock_sm: Option<u32>,
    pub max_clock_mem: Option<u32>,
    pub max_clock_video: Option<u32>,
    pub pcie_max_gen: Option<u32>,
    pub pcie_max_width: Option<u32>,
    pub temp_slowdown: Option<u32>,
    pub temp_shutdown: Option<u32>,
    pub temp_max_operating: Option<u32>,
    pub num_fans: u32,
    /// /dev/nvidiaN minor number (Linux), used to match processes to GPUs.
    pub minor_number: Option<u32>,
    pub persistence_mode: Option<bool>,
    pub display_active: Option<bool>,
    pub compute_mode: Option<String>,
    pub mig_enabled: Option<bool>,
}

impl GpuInfo {
    pub fn new(index: usize, vendor: Vendor, name: String) -> Self {
        GpuInfo {
            index,
            vendor,
            name,
            uuid: None,
            pci_bus_id: None,
            serial: None,
            vbios: None,
            architecture: None,
            compute_capability: None,
            cores: None,
            memory_total: None,
            integrated: false,
            power_limit_default: None,
            power_limit_min: None,
            power_limit_max: None,
            max_clock_graphics: None,
            max_clock_sm: None,
            max_clock_mem: None,
            max_clock_video: None,
            pcie_max_gen: None,
            pcie_max_width: None,
            temp_slowdown: None,
            temp_shutdown: None,
            temp_max_operating: None,
            num_fans: 0,
            minor_number: None,
            persistence_mode: None,
            display_active: None,
            compute_mode: None,
            mig_enabled: None,
        }
    }

    /// Name with the vendor boilerplate stripped: "NVIDIA GeForce RTX 4090" -> "GeForce RTX 4090".
    pub fn short_name(&self) -> &str {
        let n = self.name.trim();
        for p in ["NVIDIA ", "AMD ", "Intel(R) ", "Intel ", "Apple "] {
            if let Some(rest) = n.strip_prefix(p) {
                return rest;
            }
        }
        n
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ProcKind {
    Compute,
    Graphics,
    Both,
}

impl ProcKind {
    pub fn short(self) -> &'static str {
        match self {
            ProcKind::Compute => "C",
            ProcKind::Graphics => "G",
            ProcKind::Both => "C+G",
        }
    }
}

/// A process as reported by a GPU backend (PID is in the *driver's* PID
/// namespace, which may not be ours inside a container).
#[derive(Clone, Debug, Serialize)]
pub struct GpuProcess {
    pub pid: u32,
    /// True when `pid` is already in our PID namespace (fdinfo, OS counters,
    /// macOS). False for NVML on Linux, whose PIDs are host PIDs.
    pub pid_is_local: bool,
    pub kind: ProcKind,
    pub gpu_mem: Option<u64>,
    pub sm_util: Option<f32>,
    pub mem_util: Option<f32>,
    pub enc_util: Option<f32>,
    pub dec_util: Option<f32>,
    /// MIG GPU instance id, if any.
    pub gpu_instance: Option<u32>,
    /// Process name, for backends that know it (demo, OS-level counters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One sampling of a GPU's dynamic state.
#[derive(Clone, Debug, Default, Serialize)]
pub struct GpuSample {
    pub util_gpu: Option<f32>,
    pub util_mem: Option<f32>,
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
    pub mem_reserved: Option<u64>,
    pub temp: Option<u32>,
    pub temp_mem: Option<u32>,
    pub fans: Vec<u32>,
    pub power: Option<f64>,
    pub power_limit: Option<f64>,
    pub energy_joules: Option<f64>,
    pub clock_graphics: Option<u32>,
    pub clock_sm: Option<u32>,
    pub clock_mem: Option<u32>,
    pub clock_video: Option<u32>,
    pub pstate: Option<u32>,
    pub pcie_gen: Option<u32>,
    pub pcie_width: Option<u32>,
    /// Bytes/s.
    pub pcie_tx: Option<u64>,
    pub pcie_rx: Option<u64>,
    pub enc_util: Option<f32>,
    pub dec_util: Option<f32>,
    pub jpg_util: Option<f32>,
    pub ofa_util: Option<f32>,
    /// Active clock-limiting reasons, human readable ("power cap", "thermal").
    pub throttle: Vec<String>,
    pub ecc_corrected: Option<u64>,
    pub ecc_uncorrected: Option<u64>,
    pub bar1_used: Option<u64>,
    pub bar1_total: Option<u64>,
    pub processes: Vec<GpuProcess>,
    /// Set when the GPU fell off the bus / errored this round.
    pub error: Option<String>,
}

impl GpuSample {
    pub fn mem_frac(&self) -> Option<f64> {
        match (self.mem_used, self.mem_total) {
            (Some(u), Some(t)) if t > 0 => Some(u as f64 / t as f64),
            _ => None,
        }
    }
    pub fn fan(&self) -> Option<u32> {
        if self.fans.is_empty() {
            None
        } else {
            Some(self.fans.iter().sum::<u32>() / self.fans.len() as u32)
        }
    }
}

/// How a driver-reported PID was mapped to a process we can see.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PidMapping {
    /// Same PID namespace; PID is ours.
    Direct,
    /// Driver PID belongs to another namespace (container); we found the
    /// matching local process.
    Translated,
    /// Like Translated, but matched heuristically (several candidates).
    Guessed,
    /// Process lives outside our PID namespace (another container / the host).
    External,
}

/// A GPU process joined with host-side information. This is what the
/// process table shows.
#[derive(Clone, Debug, Serialize)]
pub struct ProcRow {
    pub gpu: usize,
    /// PID as reported by the GPU driver.
    pub pid: u32,
    /// PID in our namespace (what you'd pass to `kill`).
    pub local_pid: Option<u32>,
    pub mapping: PidMapping,
    pub kind: ProcKind,
    pub gpu_mem: Option<u64>,
    pub sm_util: Option<f32>,
    pub mem_util: Option<f32>,
    pub enc_util: Option<f32>,
    pub dec_util: Option<f32>,
    pub name: String,
    pub cmdline: String,
    pub user: Option<String>,
    pub cpu: Option<f32>,
    pub rss: Option<u64>,
    /// Seconds since start.
    pub runtime: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
    pub kernel: Option<String>,
    pub cpu_brand: String,
    pub cpu_cores: usize,
    /// "docker", "podman", "kubernetes", "lxc", ... if we're in a container.
    pub container: Option<String>,
    pub wsl: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct HostSample {
    pub cpu: f32,
    pub per_core: Vec<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub load: Option<[f64; 3]>,
    pub uptime: u64,
}

/// Everything known at startup.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SystemInfo {
    pub host: HostInfo,
    pub backends: Vec<BackendInfo>,
    pub gpus: Vec<GpuInfo>,
    /// Non-fatal problems worth telling the user about (e.g. "NVML not found").
    pub warnings: Vec<String>,
}

/// One full tick of data.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Snapshot {
    /// Unix time, seconds.
    pub timestamp: f64,
    pub host: HostSample,
    pub gpus: Vec<GpuSample>,
    pub processes: Vec<ProcRow>,
}
