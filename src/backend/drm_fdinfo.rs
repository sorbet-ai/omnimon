//! Per-process GPU usage from DRM fdinfo (Linux), shared by every DRM driver
//! that implements the spec: amdgpu, i915, xe, msm, panfrost, v3d...
//! See Documentation/gpu/drm-usage-stats.rst.
//!
//! For each process we find fds pointing at /dev/dri/*, read
//! /proc/<pid>/fdinfo/<fd>, dedupe DRM clients by (pdev, client-id), and turn
//! cumulative `drm-engine-*` busy-ns into utilization between samples. Only
//! processes we're allowed to inspect (our own, or all with root) show up.

use std::collections::HashMap;
use std::time::Instant;

/// One DRM client's stats as parsed from an fdinfo file.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ClientInfo {
    pub driver: String,
    pub pdev: Option<String>,
    pub client_id: Option<u64>,
    /// engine name -> busy ns (cumulative).
    pub engines: HashMap<String, u64>,
    /// engine name -> capacity (number of identical engines).
    pub capacity: HashMap<String, u64>,
    /// VRAM bytes (resident if reported, else total / legacy memory key).
    pub vram: Option<u64>,
}

fn parse_size(v: &str) -> Option<u64> {
    let mut it = v.split_whitespace();
    let n: u64 = it.next()?.parse().ok()?;
    Some(match it.next() {
        Some("KiB") => n * 1024,
        Some("MiB") => n * 1024 * 1024,
        Some("GiB") => n * 1024 * 1024 * 1024,
        _ => n,
    })
}

pub fn parse(text: &str) -> Option<ClientInfo> {
    let mut c = ClientInfo::default();
    let (mut resident, mut total, mut legacy) = (None, None, None);
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        match k.trim() {
            "drm-driver" => c.driver = v.to_string(),
            "drm-pdev" => c.pdev = Some(v.to_string()),
            "drm-client-id" => c.client_id = v.parse().ok(),
            "drm-resident-vram" | "drm-resident-local0" | "drm-resident-vram0" => resident = parse_size(v),
            "drm-total-vram" | "drm-total-local0" | "drm-total-vram0" => total = parse_size(v),
            "drm-memory-vram" => legacy = parse_size(v),
            k => {
                if let Some(e) = k.strip_prefix("drm-engine-capacity-") {
                    if let Ok(n) = v.parse() {
                        c.capacity.insert(e.to_string(), n);
                    }
                } else if let Some(e) = k.strip_prefix("drm-engine-") {
                    if let Some(ns) = v.split_whitespace().next().and_then(|n| n.parse().ok()) {
                        c.engines.insert(e.to_string(), ns);
                    }
                }
            }
        }
    }
    c.vram = resident.or(legacy).or(total);
    (!c.driver.is_empty()).then_some(c)
}

/// Aggregated per-process usage on one device.
#[derive(Debug, Default, Clone)]
pub struct ProcUsage {
    pub vram: Option<u64>,
    /// Busiest engine, percent.
    pub util: Option<f32>,
    pub enc: Option<f32>,
    pub dec: Option<f32>,
}

/// Tracks cumulative engine counters between samples.
#[derive(Default)]
pub struct Scanner {
    /// (pid, pdev, client id) -> (engine ns, when)
    prev: HashMap<(u32, String, u64), (HashMap<String, u64>, Instant)>,
}

impl Scanner {
    /// Scan /proc for clients of `driver`. Returns (pdev, pid) -> usage.
    pub fn scan(&mut self, driver: &str) -> HashMap<(String, u32), ProcUsage> {
        let now = Instant::now();
        let mut out: HashMap<(String, u32), ProcUsage> = HashMap::new();
        let mut next = HashMap::new();
        let Ok(procs) = std::fs::read_dir("/proc") else { return out };
        for p in procs.flatten() {
            let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
            let mut seen = std::collections::HashSet::new();
            for fd in fds.flatten() {
                let Ok(target) = std::fs::read_link(fd.path()) else { continue };
                if !target.to_string_lossy().starts_with("/dev/dri/") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{}", fd.file_name().to_string_lossy())) else {
                    continue;
                };
                let Some(c) = parse(&text) else { continue };
                if c.driver != driver {
                    continue;
                }
                let pdev = c.pdev.clone().unwrap_or_default();
                // Duplicated fds share one client.
                if !seen.insert((pdev.clone(), c.client_id)) {
                    continue;
                }
                let key = (pid, pdev.clone(), c.client_id.unwrap_or(0));
                let u = out.entry((pdev, pid)).or_default();
                if let Some(v) = c.vram {
                    u.vram = Some(u.vram.unwrap_or(0) + v);
                }
                if let Some((prev, t)) = self.prev.get(&key) {
                    let dt = now.duration_since(*t).as_nanos() as f64;
                    if dt > 0.0 {
                        for (eng, &ns) in &c.engines {
                            let d = ns.saturating_sub(*prev.get(eng).unwrap_or(&ns)) as f64;
                            let cap = *c.capacity.get(eng).unwrap_or(&1) as f64;
                            let pct = (d / dt / cap * 100.0).clamp(0.0, 100.0) as f32;
                            let slot = if eng.starts_with("enc") {
                                &mut u.enc
                            } else if eng.starts_with("dec") || eng.starts_with("vcn") || eng.starts_with("video") {
                                &mut u.dec
                            } else {
                                &mut u.util
                            };
                            *slot = Some(slot.unwrap_or(0.0).max(pct));
                        }
                        u.util.get_or_insert(0.0);
                    }
                }
                next.insert(key, (c.engines, now));
            }
        }
        self.prev = next;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_amdgpu_fdinfo() {
        let t = "pos:\t0\nflags:\t02100002\nmnt_id:\t26\ndrm-driver:\tamdgpu\ndrm-client-id:\t42\ndrm-pdev:\t0000:03:00.0\n\
                 drm-memory-vram:\t1024 KiB\ndrm-memory-gtt:\t2048 KiB\ndrm-engine-gfx:\t123456 ns\ndrm-engine-compute:\t10 ns\ndrm-engine-enc:\t0 ns\n";
        let c = parse(t).unwrap();
        assert_eq!(c.driver, "amdgpu");
        assert_eq!(c.client_id, Some(42));
        assert_eq!(c.pdev.as_deref(), Some("0000:03:00.0"));
        assert_eq!(c.vram, Some(1024 * 1024));
        assert_eq!(c.engines["gfx"], 123456);
        assert_eq!(c.engines.len(), 3);
    }

    #[test]
    fn prefers_resident_and_reads_capacity() {
        let t = "drm-driver: i915\ndrm-total-vram: 8 MiB\ndrm-resident-vram: 4 MiB\ndrm-engine-video: 5 ns\ndrm-engine-capacity-video: 2\n";
        let c = parse(t).unwrap();
        assert_eq!(c.vram, Some(4 << 20));
        assert_eq!(c.capacity["video"], 2);
        assert!(!c.engines.contains_key("capacity-video"));
    }

    #[test]
    fn non_drm_is_none() {
        assert!(parse("pos: 0\nflags: 0\n").is_none());
    }
}
