//! AMD backend for Linux (amdgpu). EXPERIMENTAL: built against the documented
//! sysfs/hwmon interface, tested against a fake sysfs tree only.
//!
//! - /sys/class/drm/cardN/device/{gpu_busy_percent, mem_busy_percent,
//!   mem_info_vram_used, mem_info_vram_total, pp_dpm_sclk, pp_dpm_mclk,
//!   current_link_speed, current_link_width, product_name}
//! - device/hwmon/hwmonM/{temp*_input + temp*_label, power1_average|power1_input,
//!   power1_cap, fan1_input, pwm1, freq1_input, freq2_input}
//! - per-process usage from DRM fdinfo (see drm_fdinfo.rs)
//!
//! No root, no libdrm, works in containers given /sys and /dev/dri.

use std::fs;
use std::path::{Path, PathBuf};

use super::Backend;
use super::drm_fdinfo::Scanner;
use crate::model::{BackendInfo, GpuInfo, GpuProcess, GpuSample, PidScope, ProcKind, Vendor};

struct Card {
    dev: PathBuf,
    hwmon: Option<PathBuf>,
    pdev: String,
    info: GpuInfo,
}

pub struct Amd {
    cards: Vec<Card>,
    scanner: Scanner,
    driver_version: Option<String>,
}

fn read(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn read_u64(p: &Path) -> Option<u64> {
    read(p)?.parse().ok()
}

/// Current and max clock (MHz) from pp_dpm_* ("1: 1500Mhz *").
fn dpm(p: &Path) -> (Option<u32>, Option<u32>) {
    let Some(s) = read(p) else { return (None, None) };
    let mhz = |l: &str| -> Option<u32> {
        let v = l.split_whitespace().nth(1)?;
        v.trim_end_matches(|c: char| c.is_alphabetic()).parse().ok()
    };
    let cur = s.lines().find(|l| l.trim_end().ends_with('*')).and_then(mhz);
    let max = s.lines().filter_map(mhz).max();
    (cur, max)
}

/// "16.0 GT/s PCIe" -> generation.
fn pcie_gen(speed: &str) -> Option<u32> {
    let gts: f64 = speed.split_whitespace().next()?.parse().ok()?;
    Some(match gts {
        x if x >= 64.0 => 6,
        x if x >= 32.0 => 5,
        x if x >= 16.0 => 4,
        x if x >= 8.0 => 3,
        x if x >= 5.0 => 2,
        _ => 1,
    })
}

fn find_hwmon(dev: &Path) -> Option<PathBuf> {
    fs::read_dir(dev.join("hwmon")).ok()?.flatten().map(|e| e.path()).find(|p| p.join("name").exists())
}

/// hwmon temperature by label ("edge", "junction", "mem"); falls back to temp1.
fn temp(hwmon: &Path, label: &str) -> Option<u32> {
    for i in 1..=8 {
        if read(&hwmon.join(format!("temp{i}_label"))).as_deref() == Some(label) {
            return read_u64(&hwmon.join(format!("temp{i}_input"))).map(|m| (m / 1000) as u32);
        }
    }
    None
}

impl Amd {
    pub fn new() -> Result<Self, String> {
        Self::with_root(Path::new("/sys/class/drm"))
    }

    pub fn with_root(root: &Path) -> Result<Self, String> {
        let mut cards = Vec::new();
        let mut entries: Vec<PathBuf> = fs::read_dir(root).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            let name = e.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            // cardN only (skip connectors like card0-DP-1 and renderD nodes).
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }
            let dev = e.join("device");
            if read(&dev.join("vendor")).as_deref() != Some("0x1002") {
                continue;
            }
            let driver = fs::read_link(dev.join("driver")).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            if driver.as_deref().is_some_and(|d| d != "amdgpu") {
                continue;
            }
            let pdev =
                fs::canonicalize(&dev).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
            let hwmon = find_hwmon(&dev);
            let devid = read(&dev.join("device")).unwrap_or_default();
            let name = read(&dev.join("product_name")).unwrap_or_else(|| format!("AMD Radeon GPU ({devid})"));
            let mut info = GpuInfo::new(Vendor::Amd, name);
            info.pci_bus_id = Some(pdev.clone()).filter(|s| !s.is_empty());
            info.uuid = read(&dev.join("unique_id"));
            info.vbios = read(&dev.join("vbios_version"));
            info.memory_total = read_u64(&dev.join("mem_info_vram_total"));
            // APUs have a small carve-out and no memory controller busy counter.
            info.integrated = info.memory_total.is_some_and(|m| m <= 4 << 30) && !dev.join("mem_busy_percent").exists();
            info.max_clock_graphics = dpm(&dev.join("pp_dpm_sclk")).1;
            info.max_clock_mem = dpm(&dev.join("pp_dpm_mclk")).1;
            info.pcie_max_gen = read(&dev.join("max_link_speed")).and_then(|s| pcie_gen(&s));
            info.pcie_max_width = read(&dev.join("max_link_width")).and_then(|s| s.parse().ok());
            if let Some(h) = &hwmon {
                info.num_fans = u32::from(h.join("fan1_input").exists() || h.join("pwm1").exists());
                info.power_limit_max = read_u64(&h.join("power1_cap_max")).map(|u| u as f64 / 1e6);
                info.power_limit_default = read_u64(&h.join("power1_cap_default")).map(|u| u as f64 / 1e6);
                info.temp_shutdown = read_u64(&h.join("temp2_emergency")).map(|m| (m / 1000) as u32);
                info.temp_slowdown = read_u64(&h.join("temp2_crit")).map(|m| (m / 1000) as u32);
            }
            cards.push(Card { dev, hwmon, pdev, info });
        }
        if cards.is_empty() {
            return Err("no amdgpu devices".into());
        }
        Ok(Amd { cards, scanner: Scanner::default(), driver_version: read(Path::new("/sys/module/amdgpu/version")) })
    }
}

impl Backend for Amd {
    fn info(&self) -> BackendInfo {
        BackendInfo { name: "amdgpu sysfs".into(), driver_version: self.driver_version.clone(), ..Default::default() }
    }

    fn gpus(&self) -> Vec<GpuInfo> {
        self.cards.iter().map(|c| c.info.clone()).collect()
    }

    fn sample(&mut self) -> Vec<GpuSample> {
        let usage = self.scanner.scan("amdgpu");
        self.cards
            .iter()
            .map(|c| {
                let d = &c.dev;
                let mut s = GpuSample {
                    util_gpu: read_u64(&d.join("gpu_busy_percent")).map(|v| v as f32),
                    util_mem: read_u64(&d.join("mem_busy_percent")).map(|v| v as f32),
                    mem_used: read_u64(&d.join("mem_info_vram_used")),
                    mem_total: read_u64(&d.join("mem_info_vram_total")),
                    // GTT = GPU-mapped system memory; on a full dGPU it is where spill lands.
                    mem_shared: read_u64(&d.join("mem_info_gtt_used")).filter(|_| !c.info.integrated),
                    pcie_gen: read(&d.join("current_link_speed")).and_then(|s| pcie_gen(&s)),
                    pcie_width: read(&d.join("current_link_width")).and_then(|s| s.parse().ok()),
                    ..Default::default()
                };
                let (sclk, _) = dpm(&d.join("pp_dpm_sclk"));
                let (mclk, _) = dpm(&d.join("pp_dpm_mclk"));
                s.clock_graphics = sclk;
                s.clock_mem = mclk;
                if let Some(h) = &c.hwmon {
                    s.temp = temp(h, "junction")
                        .or_else(|| temp(h, "edge"))
                        .or_else(|| read_u64(&h.join("temp1_input")).map(|m| (m / 1000) as u32));
                    s.temp_mem = temp(h, "mem");
                    s.power = read_u64(&h.join("power1_average")).or_else(|| read_u64(&h.join("power1_input"))).map(|u| u as f64 / 1e6);
                    s.power_limit = read_u64(&h.join("power1_cap")).map(|u| u as f64 / 1e6);
                    if let (Some(pwm), max) = (read_u64(&h.join("pwm1")), read_u64(&h.join("pwm1_max")).unwrap_or(255)) {
                        s.fans = vec![(pwm * 100 / max.max(1)) as u32];
                    }
                    // hwmon freq1 (sclk) is instantaneous; pp_dpm is only the level.
                    if let Some(hz) = read_u64(&h.join("freq1_input")) {
                        s.clock_graphics = Some((hz / 1_000_000) as u32);
                    }
                    if let Some(hz) = read_u64(&h.join("freq2_input")) {
                        s.clock_mem = Some((hz / 1_000_000) as u32);
                    }
                }
                s.processes = usage
                    .iter()
                    .filter(|((pdev, _), _)| pdev == &c.pdev || (pdev.is_empty() && self.cards.len() == 1))
                    .map(|((_, pid), u)| GpuProcess {
                        pid: *pid,
                        scope: PidScope::Local,
                        kind: ProcKind::Graphics,
                        gpu_mem: u.vram,
                        gpu_mem_shared: u.gtt,
                        sm_util: u.util,
                        enc_util: u.enc,
                        dec_util: u.dec,
                        ..Default::default()
                    })
                    .collect();
                s.processes.sort_by_key(|p| p.pid);
                s
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    #[test]
    fn reads_fake_sysfs() {
        let root = std::env::temp_dir().join(format!("amd-sysfs-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dev = root.join("card0/device");
        w(&dev.join("vendor"), "0x1002\n");
        w(&dev.join("device"), "0x744c\n");
        w(&dev.join("gpu_busy_percent"), "37\n");
        w(&dev.join("mem_busy_percent"), "12\n");
        w(&dev.join("mem_info_vram_total"), "25753026560\n");
        w(&dev.join("mem_info_vram_used"), "1073741824\n");
        w(&dev.join("pp_dpm_sclk"), "0: 500Mhz\n1: 1800Mhz *\n2: 2500Mhz\n");
        w(&dev.join("current_link_speed"), "16.0 GT/s PCIe\n");
        w(&dev.join("current_link_width"), "16\n");
        let h = dev.join("hwmon/hwmon3");
        w(&h.join("name"), "amdgpu\n");
        w(&h.join("temp1_label"), "edge\n");
        w(&h.join("temp1_input"), "45000\n");
        w(&h.join("temp2_label"), "junction\n");
        w(&h.join("temp2_input"), "61000\n");
        w(&h.join("power1_average"), "123000000\n");
        w(&h.join("power1_cap"), "300000000\n");
        w(&h.join("pwm1"), "128\n");
        w(&h.join("freq1_input"), "2100000000\n");
        // A connector entry must be ignored.
        w(&root.join("card0-DP-1/status"), "connected\n");

        let mut amd = Amd::with_root(&root).unwrap();
        let g = amd.gpus();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].vendor, Vendor::Amd);
        assert_eq!(g[0].max_clock_graphics, Some(2500));
        assert!(!g[0].integrated);
        let s = &amd.sample()[0];
        assert_eq!(s.util_gpu, Some(37.0));
        assert_eq!(s.util_mem, Some(12.0));
        assert_eq!(s.mem_used, Some(1 << 30));
        assert_eq!(s.temp, Some(61));
        assert_eq!(s.power, Some(123.0));
        assert_eq!(s.power_limit, Some(300.0));
        assert_eq!(s.fans, vec![50]);
        assert_eq!(s.clock_graphics, Some(2100));
        assert_eq!(s.pcie_gen, Some(4));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dpm_parsing() {
        assert_eq!(pcie_gen("8.0 GT/s PCIe"), Some(3));
        assert_eq!(pcie_gen("2.5 GT/s"), Some(1));
    }
}
