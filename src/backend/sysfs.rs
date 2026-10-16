//! Small helpers for reading Linux sysfs / hwmon attributes.

use std::fs;
use std::path::{Path, PathBuf};

pub fn read(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn read_u64(p: &Path) -> Option<u64> {
    read(p)?.parse().ok()
}

/// Current and max clock (MHz) from pp_dpm_* ("1: 1500Mhz *").
pub fn dpm(p: &Path) -> (Option<u32>, Option<u32>) {
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
pub fn pcie_gen(speed: &str) -> Option<u32> {
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

pub fn find_hwmon(dev: &Path) -> Option<PathBuf> {
    fs::read_dir(dev.join("hwmon")).ok()?.flatten().map(|e| e.path()).find(|p| p.join("name").exists())
}

/// hwmon temperature by label ("edge", "junction", "mem"); falls back to temp1.
pub fn temp(hwmon: &Path, label: &str) -> Option<u32> {
    for i in 1..=8 {
        if read(&hwmon.join(format!("temp{i}_label"))).as_deref() == Some(label) {
            return read_u64(&hwmon.join(format!("temp{i}_input"))).map(|m| (m / 1000) as u32);
        }
    }
    None
}
