//! Vendor-neutral data model. Every backend (NVIDIA, AMD, Apple, ...) fills
//! these structs; anything a vendor can't provide is `None`/empty and the UI
//! hides it or shows N/A.
//!
//! These types are also the `--json` / `--serve` wire format (see
//! `report::JsonOut`): changing a field's name or meaning is a schema change.

use serde::{Deserialize, Serialize};

// Some variants are only constructed on some platforms.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    #[default]
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
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendInfo {
    /// Data source, e.g. "NVML", "IOKit", "amdgpu sysfs".
    pub name: String,
    pub library: Option<String>,
    pub driver_version: Option<String>,
    pub api_version: Option<String>,
    /// Compute runtime version, e.g. CUDA driver version "13.0".
    pub runtime_version: Option<String>,
    /// What `runtime_version` is ("CUDA", "ROCm", "Metal").
    pub runtime_name: Option<String>,
}

/// Mostly-static properties of a GPU, queried once.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuInfo {
    /// Position in `SystemInfo::gpus`; assigned by the collector.
    pub index: usize,
    /// Index into `SystemInfo::backends`; assigned by the collector.
    pub backend: usize,
    pub vendor: Vendor,
    pub name: String,
    pub uuid: Option<String>,
    pub pci_bus_id: Option<String>,
    pub serial: Option<String>,
    pub vbios: Option<String>,
    pub architecture: Option<String>,
    /// SoC / chip name for integrated GPUs (e.g. "Apple M4 Pro").
    pub chip: Option<String>,
    pub compute_capability: Option<String>,
    pub cores: Option<u32>,
    pub memory_total: Option<u64>,
    /// Peak memory bandwidth, bytes/s (for spill slowdown estimates).
    pub memory_bandwidth: Option<f64>,
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
    pub fn new(vendor: Vendor, name: String) -> Self {
        GpuInfo { vendor, name, ..Default::default() }
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

    /// Stable identifier across runs: UUID, else PCI address, else vendor+index.
    pub fn stable_id(&self) -> String {
        self.uuid
            .clone()
            .or_else(|| self.pci_bus_id.clone())
            .unwrap_or_else(|| format!("{}-{}", self.vendor.label().to_lowercase(), self.index))
    }

    /// Theoretical host link bandwidth in bytes/s (one direction), from the
    /// maximum PCIe generation and width.
    pub fn pcie_bandwidth(&self) -> Option<f64> {
        // Usable GB/s per lane after encoding overhead.
        let per_lane = match self.pcie_max_gen? {
            1 => 0.25,
            2 => 0.5,
            3 => 0.985,
            4 => 1.969,
            5 => 3.938,
            _ => 7.877,
        };
        Some(per_lane * self.pcie_max_width? as f64 * 1e9)
    }
}

/// Why clocks are being held down. Vendor backends map their own reason
/// codes onto these; the UI decides severity from the variant.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Throttle {
    Idle,
    AppClocks,
    PowerCap,
    HwSlowdown,
    SyncBoost,
    SwThermal,
    HwThermal,
    PowerBrake,
    DisplayClocks,
    Other(String),
}

impl Throttle {
    pub fn label(&self) -> &str {
        match self {
            Throttle::Idle => "idle",
            Throttle::AppClocks => "app clocks",
            Throttle::PowerCap => "power cap",
            Throttle::HwSlowdown => "hw slowdown",
            Throttle::SyncBoost => "sync boost",
            Throttle::SwThermal => "sw thermal",
            Throttle::HwThermal => "hw thermal",
            Throttle::PowerBrake => "power brake",
            Throttle::DisplayClocks => "display clocks",
            Throttle::Other(s) => s,
        }
    }

    /// 0 = informational, 1 = worth showing, 2 = something is wrong.
    pub fn severity(&self) -> u8 {
        match self {
            Throttle::Idle | Throttle::AppClocks | Throttle::DisplayClocks | Throttle::SyncBoost => 0,
            Throttle::PowerCap | Throttle::SwThermal | Throttle::Other(_) => 1,
            Throttle::HwSlowdown | Throttle::HwThermal | Throttle::PowerBrake => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ProcKind {
    #[default]
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

/// Which PID namespace a backend's `pid` belongs to.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PidScope {
    /// The GPU driver's namespace (NVML on Linux reports host PIDs): must be
    /// mapped before it can be joined with /proc or signalled.
    #[default]
    Driver,
    /// Already ours (DRM fdinfo, Windows counters, macOS IOKit).
    Local,
    /// Not a real process (demo backend); never joined or signalled.
    Synthetic,
}

/// A process as reported by a GPU backend.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuProcess {
    pub pid: u32,
    pub scope: PidScope,
    pub kind: ProcKind,
    /// Device-local memory (VRAM).
    pub gpu_mem: Option<u64>,
    /// GPU allocations living in system memory (Windows "shared usage", AMD
    /// GTT). Large values on a discrete GPU mean the process has spilled.
    pub gpu_mem_shared: Option<u64>,
    pub sm_util: Option<f32>,
    pub mem_util: Option<f32>,
    pub enc_util: Option<f32>,
    pub dec_util: Option<f32>,
    /// MIG GPU instance id, if any.
    pub gpu_instance: Option<u32>,
    /// Process name when the backend knows it (demo, OS counters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_hint: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum SpillLevel {
    #[default]
    None,
    /// VRAM nearly full; the next allocation may spill.
    Pressure,
    /// Allocations are measurably living in system memory.
    Spilling,
}

/// Result of spill analysis for one GPU (see `spill.rs`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Spill {
    pub level: SpillLevel,
    /// Bytes of GPU allocations living in system memory.
    pub bytes: Option<u64>,
    /// Estimated slowdown factor for bandwidth-bound work (e.g. 5.0 = 5x).
    pub slowdown: Option<f64>,
    /// Short human-readable evidence.
    pub reason: String,
}

/// One sampling of a GPU's dynamic state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuSample {
    pub util_gpu: Option<f32>,
    pub util_mem: Option<f32>,
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
    pub mem_reserved: Option<u64>,
    /// GPU allocations in system memory across all processes (Windows
    /// adapter shared usage, AMD GTT).
    pub mem_shared: Option<u64>,
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
    /// Active clock-limiting reasons.
    pub throttle: Vec<Throttle>,
    pub ecc_corrected: Option<u64>,
    pub ecc_uncorrected: Option<u64>,
    pub bar1_used: Option<u64>,
    pub bar1_total: Option<u64>,
    pub spill: Spill,
    /// Backend -> collector only; the joined list is `Snapshot::processes`.
    #[serde(skip)]
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
        if self.fans.is_empty() { None } else { Some(self.fans.iter().sum::<u32>() / self.fans.len() as u32) }
    }
    pub fn throttle_severity(&self) -> u8 {
        self.throttle.iter().map(Throttle::severity).max().unwrap_or(0)
    }
}

/// How a driver-reported PID was mapped to a process we can see.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PidMapping {
    /// Same PID namespace; PID is ours.
    #[default]
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
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcRow {
    pub gpu: usize,
    /// What the GPU backend reported (pid is in `usage.scope`'s namespace).
    #[serde(flatten)]
    pub usage: GpuProcess,
    /// PID in our namespace (what you'd pass to `kill`).
    pub local_pid: Option<u32>,
    pub mapping: PidMapping,
    pub name: String,
    pub cmdline: String,
    pub user: Option<String>,
    pub cpu: Option<f32>,
    pub rss: Option<u64>,
    /// Seconds since start.
    pub runtime: Option<u64>,
    /// This process's allocations are spilling into system memory.
    pub spilling: bool,
    /// Container runtime and short id ("docker:3f2a1b9c0d12"), when known.
    #[serde(default)]
    pub container: Option<String>,
}

impl ProcRow {
    /// Driver-reported PID (identity within one GPU).
    pub fn pid(&self) -> u32 {
        self.usage.pid
    }
    pub fn key(&self) -> (usize, u32) {
        (self.gpu, self.usage.pid)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostSample {
    pub cpu: f32,
    pub per_core: Vec<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub load: Option<[f64; 3]>,
    pub uptime: u64,
    /// Bytes/s across non-loopback interfaces.
    pub net_rx: Option<u64>,
    pub net_tx: Option<u64>,
    /// Bytes/s across physical disks.
    pub disk_read: Option<u64>,
    pub disk_write: Option<u64>,
    /// Hottest CPU package/core sensor.
    pub cpu_temp: Option<f32>,
}

/// Everything known at startup.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemInfo {
    pub host: HostInfo,
    pub backends: Vec<BackendInfo>,
    pub gpus: Vec<GpuInfo>,
    /// Non-fatal problems worth telling the user about (e.g. "NVML not found").
    pub warnings: Vec<String>,
}

impl SystemInfo {
    pub fn backend_of(&self, gpu: usize) -> Option<&BackendInfo> {
        self.backends.get(self.gpus.get(gpu)?.backend)
    }
}

/// One full tick of data.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// Unix time, seconds.
    pub timestamp: f64,
    pub host: HostSample,
    pub gpus: Vec<GpuSample>,
    pub processes: Vec<ProcRow>,
    /// What changed since the previous snapshot.
    #[serde(default)]
    pub events: Vec<crate::events::Event>,
}
