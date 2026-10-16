//! Windows, any vendor (AMD, Intel, and NVIDIA when NVML is missing): WDDM
//! adapters from D3DKMT, utilization and memory from the OS's GPU
//! performance counters. No vendor SDK needed. Clocks, power, temperature and
//! fans aren't exposed this way (ADLX / IGCL would add them later).

use super::Backend;
use crate::model::{BackendInfo, GpuInfo, GpuProcess, GpuSample, PidScope, ProcKind, Vendor};
use crate::os::windows::{AdapterDesc, Luid, adapter_descs, pdh_sample};

struct Adapter {
    luid: Luid,
    info: GpuInfo,
}

pub struct Wddm {
    adapters: Vec<Adapter>,
}

fn vendor_of(d: &AdapterDesc) -> Vendor {
    let n = d.name.to_lowercase();
    match d.vendor_id {
        Some(0x1002) | Some(0x1022) => Vendor::Amd,
        Some(0x8086) => Vendor::Intel,
        Some(0x10de) => Vendor::Nvidia,
        _ if n.contains("radeon") || n.contains("amd") => Vendor::Amd,
        _ if n.contains("intel") => Vendor::Intel,
        _ => Vendor::Nvidia,
    }
}

impl Wddm {
    /// `skip`: PCI locations already handled by a richer backend (NVML).
    pub fn new(skip: &[(u32, u32, u32)]) -> Result<Self, String> {
        let all = crate::brand::env("WDDM_ALL").is_some();
        let adapters: Vec<Adapter> = adapter_descs()
            .into_iter()
            .filter(AdapterDesc::is_gpu)
            .filter(|d| all || d.pci.is_none_or(|p| !skip.contains(&p)))
            .map(|d| {
                let mut info = GpuInfo::new(vendor_of(&d), d.name.clone());
                info.pci_bus_id = d.pci.map(|(b, dev, f)| format!("00000000:{b:02X}:{dev:02X}.{f:X}"));
                info.integrated = d.is_integrated();
                info.memory_total = Some(if info.integrated { d.shared_system } else { d.dedicated }).filter(|&m| m > 0);
                Adapter { luid: d.luid, info }
            })
            .collect();
        if adapters.is_empty() {
            return Err("no additional WDDM adapters".into());
        }
        Ok(Wddm { adapters })
    }
}

impl Backend for Wddm {
    fn info(&self) -> BackendInfo {
        BackendInfo { name: "WDDM counters".into(), library: Some("pdh.dll + gdi32.dll".into()), ..Default::default() }
    }

    fn gpus(&self) -> Vec<GpuInfo> {
        self.adapters.iter().map(|a| a.info.clone()).collect()
    }

    fn sample(&mut self) -> Vec<GpuSample> {
        let Some(pdh) = pdh_sample() else {
            return self
                .adapters
                .iter()
                .map(|_| GpuSample { error: Some("performance counters unavailable".into()), ..Default::default() })
                .collect();
        };
        self.adapters
            .iter()
            .map(|a| {
                let u = pdh.adapters.get(&a.luid).cloned().unwrap_or_default();
                // On integrated GPUs the "shared" pool is their memory.
                let used = if a.info.integrated { u.shared.map(|s| s + u.dedicated.unwrap_or(0)) } else { u.dedicated };
                let mut processes: Vec<GpuProcess> = pdh
                    .procs
                    .iter()
                    .filter(|((luid, _), p)| *luid == a.luid && p.is_active())
                    .map(|(&(_, pid), p)| GpuProcess {
                        pid,
                        scope: PidScope::Local,
                        kind: ProcKind::Graphics,
                        // An integrated GPU's memory is mostly the shared pool.
                        // The counters occasionally report garbage (terabytes).
                        gpu_mem: if a.info.integrated {
                            p.shared.map(|s| s + p.dedicated.unwrap_or(0))
                        } else {
                            p.dedicated.filter(|&b| a.info.memory_total.is_none_or(|t| b <= t))
                        },
                        // Shared usage only means spill on a discrete GPU.
                        gpu_mem_shared: if a.info.integrated { None } else { p.shared },
                        sm_util: p.util,
                        enc_util: p.enc,
                        dec_util: p.dec,
                        ..Default::default()
                    })
                    .collect();
                processes.sort_by_key(|p| p.pid);
                GpuSample {
                    util_gpu: u.util,
                    mem_used: used,
                    mem_total: a.info.memory_total,
                    mem_shared: if a.info.integrated { None } else { u.shared },
                    enc_util: u.enc,
                    dec_util: u.dec,
                    processes,
                    ..Default::default()
                }
            })
            .collect()
    }
}
