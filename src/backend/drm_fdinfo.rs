//! Per-process GPU usage from DRM fdinfo (Linux), shared by every DRM driver
//! that implements the spec: amdgpu, i915, xe, msm, panfrost, v3d...
//! See Documentation/gpu/drm-usage-stats.rst.
//!
//! For each process we find fds pointing at /dev/dri/*, read
//! /proc/<pid>/fdinfo/<fd>, dedupe DRM clients by (pdev, client-id), and turn
//! cumulative counters into utilization between samples: `drm-engine-*` busy
//! ns (amdgpu, i915) or `drm-cycles-*` / `drm-total-cycles-*` (xe). Only
//! processes we're allowed to inspect (our own, or all with root) show up.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Cumulative engine counters of one DRM client.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Counters {
    /// engine -> busy ns.
    pub ns: HashMap<String, u64>,
    /// engine -> busy cycles / total cycles (xe).
    pub cycles: HashMap<String, u64>,
    pub total_cycles: HashMap<String, u64>,
    /// engine -> number of identical engines.
    pub capacity: HashMap<String, u64>,
}

/// One DRM client's stats as parsed from an fdinfo file.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ClientInfo {
    pub driver: String,
    pub pdev: Option<String>,
    pub client_id: Option<u64>,
    pub counters: Counters,
    /// VRAM bytes (resident if reported, else total / legacy memory key).
    pub vram: Option<u64>,
    /// GPU-mapped system memory (GTT) bytes.
    pub gtt: Option<u64>,
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
    let (mut gtt_resident, mut gtt_legacy) = (None, None);
    let num = |v: &str| v.split_whitespace().next().and_then(|n| n.parse::<u64>().ok());
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "drm-driver" => c.driver = v.to_string(),
            "drm-pdev" => c.pdev = Some(v.to_string()),
            "drm-client-id" => c.client_id = v.parse().ok(),
            "drm-resident-vram" | "drm-resident-local0" | "drm-resident-vram0" => resident = parse_size(v),
            "drm-total-vram" | "drm-total-local0" | "drm-total-vram0" => total = parse_size(v),
            "drm-memory-vram" => legacy = parse_size(v),
            "drm-resident-gtt" => gtt_resident = parse_size(v),
            "drm-memory-gtt" => gtt_legacy = parse_size(v),
            _ => {
                let ctr = &mut c.counters;
                let (map, name) = if let Some(e) = k.strip_prefix("drm-engine-capacity-") {
                    (&mut ctr.capacity, e)
                } else if let Some(e) = k.strip_prefix("drm-engine-") {
                    (&mut ctr.ns, e)
                } else if let Some(e) = k.strip_prefix("drm-total-cycles-") {
                    (&mut ctr.total_cycles, e)
                } else if let Some(e) = k.strip_prefix("drm-cycles-") {
                    (&mut ctr.cycles, e)
                } else {
                    continue;
                };
                if let Some(n) = num(v) {
                    map.insert(name.to_string(), n);
                }
            }
        }
    }
    c.vram = resident.or(legacy).or(total);
    c.gtt = gtt_resident.or(gtt_legacy);
    (!c.driver.is_empty()).then_some(c)
}

/// Per-engine utilization (percent of the engine group) between two reads
/// `dt_ns` apart. Counters that went backwards (client restarted) read 0.
pub fn engine_util(prev: &Counters, cur: &Counters, dt_ns: f64) -> HashMap<String, f32> {
    let mut out = HashMap::new();
    if dt_ns > 0.0 {
        for (eng, &ns) in &cur.ns {
            let d = ns.saturating_sub(*prev.ns.get(eng).unwrap_or(&ns)) as f64;
            let cap = *cur.capacity.get(eng).unwrap_or(&1).max(&1) as f64;
            out.insert(eng.clone(), (d / dt_ns / cap * 100.0).clamp(0.0, 100.0) as f32);
        }
    }
    for (eng, &c) in &cur.cycles {
        let (Some(&t), Some(&pc), Some(&pt)) = (cur.total_cycles.get(eng), prev.cycles.get(eng), prev.total_cycles.get(eng)) else {
            continue;
        };
        let dt = t.saturating_sub(pt) as f64;
        if dt > 0.0 {
            out.insert(eng.clone(), (c.saturating_sub(pc) as f64 / dt * 100.0).clamp(0.0, 100.0) as f32);
        }
    }
    out
}

fn is_encode(eng: &str) -> bool {
    eng.starts_with("enc") || eng == "vecs" || eng.starts_with("video-enhance")
}

fn is_decode(eng: &str) -> bool {
    eng.starts_with("dec") || eng.starts_with("vcn") || eng == "video" || eng == "vcs"
}

/// Aggregated usage of one process or one device.
#[derive(Debug, Default, Clone)]
pub struct Usage {
    pub vram: Option<u64>,
    pub gtt: Option<u64>,
    /// Busiest compute/render engine, percent.
    pub util: Option<f32>,
    pub enc: Option<f32>,
    pub dec: Option<f32>,
}

impl Usage {
    fn add_engines(&mut self, utils: &HashMap<String, f32>) {
        for (eng, &pct) in utils {
            let slot = if is_encode(eng) {
                &mut self.enc
            } else if is_decode(eng) {
                &mut self.dec
            } else {
                &mut self.util
            };
            *slot = Some(slot.unwrap_or(0.0).max(pct));
        }
    }
}

#[derive(Debug, Default)]
pub struct Scan {
    /// (pdev, pid) -> usage.
    pub procs: HashMap<(String, u32), Usage>,
    /// pdev -> usage summed over every visible client (device utilization
    /// when the driver has no global counter).
    pub devices: HashMap<String, Usage>,
}

/// Tracks cumulative engine counters between samples.
#[derive(Default)]
pub struct Scanner {
    /// (pid, pdev, client id) -> (counters, when)
    prev: HashMap<(u32, String, u64), (Counters, Instant)>,
}

impl Scanner {
    /// Scan /proc for clients of any of `drivers`.
    pub fn scan(&mut self, drivers: &[&str]) -> Scan {
        let now = Instant::now();
        let mut out = Scan::default();
        // pdev -> engine -> summed percent across clients
        let mut device_engines: HashMap<String, HashMap<String, f32>> = HashMap::new();
        let mut next = HashMap::new();
        let Ok(procs) = std::fs::read_dir("/proc") else { return out };
        for p in procs.flatten() {
            let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
            let mut seen = HashSet::new();
            for fd in fds.flatten() {
                let Ok(target) = std::fs::read_link(fd.path()) else { continue };
                if !target.to_string_lossy().starts_with("/dev/dri/") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{}", fd.file_name().to_string_lossy())) else {
                    continue;
                };
                let Some(c) = parse(&text) else { continue };
                if !drivers.contains(&c.driver.as_str()) {
                    continue;
                }
                let pdev = c.pdev.clone().unwrap_or_default();
                // Duplicated fds share one client.
                if !seen.insert((pdev.clone(), c.client_id)) {
                    continue;
                }
                let key = (pid, pdev.clone(), c.client_id.unwrap_or(0));
                let u = out.procs.entry((pdev.clone(), pid)).or_default();
                if let Some(v) = c.vram {
                    u.vram = Some(u.vram.unwrap_or(0) + v);
                }
                if let Some(v) = c.gtt {
                    u.gtt = Some(u.gtt.unwrap_or(0) + v);
                }
                if let Some((prev, t)) = self.prev.get(&key) {
                    let utils = engine_util(prev, &c.counters, now.duration_since(*t).as_nanos() as f64);
                    u.add_engines(&utils);
                    u.util.get_or_insert(0.0);
                    let dev = device_engines.entry(pdev).or_default();
                    for (eng, pct) in utils {
                        *dev.entry(eng).or_default() += pct;
                    }
                }
                next.insert(key, (c.counters, now));
            }
        }
        for (pdev, engines) in device_engines {
            let clamped: HashMap<String, f32> = engines.into_iter().map(|(e, v)| (e, v.min(100.0))).collect();
            out.devices.entry(pdev).or_default().add_engines(&clamped);
        }
        for ((pdev, _), u) in &out.procs {
            let d = out.devices.entry(pdev.clone()).or_default();
            if let Some(v) = u.vram {
                d.vram = Some(d.vram.unwrap_or(0) + v);
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
        assert_eq!(c.gtt, Some(2048 * 1024));
        assert_eq!(c.counters.ns["gfx"], 123456);
        assert_eq!(c.counters.ns.len(), 3);
    }

    #[test]
    fn prefers_resident_and_reads_capacity() {
        let t = "drm-driver: i915\ndrm-total-vram: 8 MiB\ndrm-resident-vram: 4 MiB\ndrm-engine-video: 5 ns\ndrm-engine-capacity-video: 2\n";
        let c = parse(t).unwrap();
        assert_eq!(c.vram, Some(4 << 20));
        assert_eq!(c.counters.capacity["video"], 2);
        assert!(!c.counters.ns.contains_key("capacity-video"));
    }

    #[test]
    fn non_drm_is_none() {
        assert!(parse("pos: 0\nflags: 0\n").is_none());
    }

    #[test]
    fn ns_and_cycle_utilization() {
        let a = parse(
            "drm-driver: xe\ndrm-cycles-rcs: 100\ndrm-total-cycles-rcs: 1000\ndrm-engine-render: 0 ns\ndrm-engine-capacity-render: 2\n",
        )
        .unwrap();
        let b = parse("drm-driver: xe\ndrm-cycles-rcs: 600\ndrm-total-cycles-rcs: 2000\ndrm-engine-render: 500000000 ns\ndrm-engine-capacity-render: 2\n").unwrap();
        let u = engine_util(&a.counters, &b.counters, 1e9);
        assert_eq!(u["rcs"], 50.0); // 500 of 1000 cycles
        assert_eq!(u["render"], 25.0); // 0.5 s busy over 1 s, 2 engines
        // A counter that went backwards (new client) is not negative.
        assert_eq!(engine_util(&b.counters, &a.counters, 1e9)["render"], 0.0);
    }
}
