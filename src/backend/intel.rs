//! Intel GPUs on Linux (i915 and xe drivers). EXPERIMENTAL: written against
//! the documented sysfs/hwmon/fdinfo interfaces, tested on a fake tree only.
//!
//! - frequency: i915 `cardN/gt_act_freq_mhz` or `cardN/gt/gt0/rps_act_freq_mhz`;
//!   xe `device/tile0/gt0/freq0/act_freq`
//! - power: hwmon `energy1_input` (µJ) as a rate; limit `power1_max`
//! - temperature: hwmon `temp*_input` (discrete cards)
//! - utilization, VRAM and per-process data: DRM fdinfo across visible
//!   clients (i915 `drm-engine-*` ns, xe `drm-cycles-*`). Without root only
//!   your own processes count, so device utilization is a lower bound.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::Backend;
use super::drm_fdinfo::Scanner;
use super::sysfs::{find_hwmon, read, read_u64};
use crate::model::{BackendInfo, GpuInfo, GpuProcess, GpuSample, PidScope, ProcKind, Vendor};

struct Card {
    card: PathBuf,
    hwmon: Option<PathBuf>,
    pdev: String,
    info: GpuInfo,
    /// (energy µJ, when) for power as a rate.
    last_energy: Option<(u64, Instant)>,
}

pub struct Intel {
    cards: Vec<Card>,
    scanner: Scanner,
}

/// First readable attribute among `paths` relative to `base`, as a number.
fn first_u64(base: &Path, paths: &[&str]) -> Option<u64> {
    paths.iter().find_map(|p| read_u64(&base.join(p)))
}

fn name_for(device_id: &str) -> String {
    let id = u32::from_str_radix(device_id.trim_start_matches("0x"), 16).unwrap_or(0);
    let family = match id >> 8 {
        0x56 => "Intel Arc A-series",
        0xe2 => "Intel Arc B-series",
        0x7d | 0x64 => "Intel Arc Graphics",
        _ => "Intel Graphics",
    };
    format!("{family} ({device_id})")
}

const ACT_FREQ: &[&str] = &["gt_act_freq_mhz", "gt/gt0/rps_act_freq_mhz", "device/tile0/gt0/freq0/act_freq"];
const MAX_FREQ: &[&str] = &["gt_RP0_freq_mhz", "gt/gt0/rps_RP0_freq_mhz", "device/tile0/gt0/freq0/rp0_freq", "gt_max_freq_mhz"];

impl Intel {
    pub fn new() -> Result<Self, String> {
        Self::with_root(Path::new("/sys/class/drm"))
    }

    pub fn with_root(root: &Path) -> Result<Self, String> {
        let mut entries: Vec<PathBuf> = fs::read_dir(root).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
        entries.sort();
        let mut cards = Vec::new();
        for card in entries {
            let name = card.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }
            let dev = card.join("device");
            if read(&dev.join("vendor")).as_deref() != Some("0x8086") {
                continue;
            }
            let driver = fs::read_link(dev.join("driver")).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            if driver.as_deref().is_some_and(|d| d != "i915" && d != "xe") {
                continue;
            }
            let pdev =
                fs::canonicalize(&dev).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
            let mut info = GpuInfo::new(Vendor::Intel, name_for(&read(&dev.join("device")).unwrap_or_default()));
            info.pci_bus_id = Some(pdev.clone()).filter(|s| !s.is_empty());
            info.max_clock_graphics = first_u64(&card, MAX_FREQ).map(|v| v as u32);
            let hwmon = find_hwmon(&dev);
            // Discrete cards expose hwmon; integrated ones share system memory.
            info.integrated = hwmon.is_none();
            if let Some(h) = &hwmon {
                info.power_limit_default = read_u64(&h.join("power1_max")).map(|u| u as f64 / 1e6);
            }
            cards.push(Card { card, hwmon, pdev, info, last_energy: None });
        }
        if cards.is_empty() {
            return Err("no i915/xe devices".into());
        }
        Ok(Intel { cards, scanner: Scanner::default() })
    }
}

impl Backend for Intel {
    fn info(&self) -> BackendInfo {
        BackendInfo { name: "i915/xe sysfs".into(), ..Default::default() }
    }

    fn gpus(&self) -> Vec<GpuInfo> {
        self.cards.iter().map(|c| c.info.clone()).collect()
    }

    fn sample(&mut self) -> Vec<GpuSample> {
        let scan = self.scanner.scan(&["i915", "xe"]);
        let single = self.cards.len() == 1;
        self.cards
            .iter_mut()
            .map(|c| {
                let mine = |pdev: &str| pdev == c.pdev || (pdev.is_empty() && single);
                let dev = scan.devices.iter().find(|(p, _)| mine(p)).map(|(_, u)| u.clone()).unwrap_or_default();
                let mut s = GpuSample {
                    util_gpu: dev.util,
                    enc_util: dev.enc,
                    dec_util: dev.dec,
                    mem_used: if c.info.integrated { None } else { dev.vram },
                    clock_graphics: first_u64(&c.card, ACT_FREQ).map(|v| v as u32),
                    ..Default::default()
                };
                if let Some(h) = &c.hwmon {
                    s.temp = read_u64(&h.join("temp1_input")).map(|m| (m / 1000) as u32);
                    s.power_limit = read_u64(&h.join("power1_max")).map(|u| u as f64 / 1e6);
                    if let Some(e) = read_u64(&h.join("energy1_input")) {
                        let now = Instant::now();
                        if let Some((prev, t)) = c.last_energy {
                            let dt = now.duration_since(t).as_secs_f64();
                            if dt > 0.0 && e >= prev {
                                s.power = Some((e - prev) as f64 / 1e6 / dt);
                            }
                        }
                        c.last_energy = Some((e, now));
                    }
                }
                s.processes = scan
                    .procs
                    .iter()
                    .filter(|((pdev, _), _)| mine(pdev))
                    .map(|((_, pid), u)| GpuProcess {
                        pid: *pid,
                        scope: PidScope::Local,
                        kind: ProcKind::Graphics,
                        gpu_mem: u.vram,
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
        let root = std::env::temp_dir().join(format!("intel-sysfs-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // An xe discrete card and an i915 iGPU.
        let a = root.join("card0");
        w(&a.join("device/vendor"), "0x8086\n");
        w(&a.join("device/device"), "0xe20b\n");
        w(&a.join("device/tile0/gt0/freq0/act_freq"), "2450\n");
        w(&a.join("device/tile0/gt0/freq0/rp0_freq"), "2850\n");
        w(&a.join("device/hwmon/hwmon4/name"), "xe\n");
        w(&a.join("device/hwmon/hwmon4/temp1_input"), "51000\n");
        w(&a.join("device/hwmon/hwmon4/energy1_input"), "1000000\n");
        let b = root.join("card1");
        w(&b.join("device/vendor"), "0x8086\n");
        w(&b.join("device/device"), "0x7d55\n");
        w(&b.join("gt_act_freq_mhz"), "1100\n");
        w(&b.join("gt_RP0_freq_mhz"), "2250\n");
        // Not Intel: ignored.
        w(&root.join("card2/device/vendor"), "0x10de\n");

        let mut intel = Intel::with_root(&root).unwrap();
        let g = intel.gpus();
        assert_eq!(g.len(), 2);
        assert!(g[0].name.starts_with("Intel Arc B-series"));
        assert_eq!(g[0].max_clock_graphics, Some(2850));
        assert!(!g[0].integrated && g[1].integrated);
        let s = intel.sample();
        assert_eq!(s[0].clock_graphics, Some(2450));
        assert_eq!(s[0].temp, Some(51));
        assert_eq!(s[1].clock_graphics, Some(1100));
        assert_eq!(s[1].temp, None);
        let _ = fs::remove_dir_all(&root);
    }
}
