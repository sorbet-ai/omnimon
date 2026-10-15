//! VRAM spill detection. Vendor-neutral and pure, so it is unit-tested.
//!
//! "Spill" = GPU allocations living in system memory, reached over PCIe at a
//! fraction of VRAM bandwidth. Nothing crashes; bandwidth-bound work (LLM
//! token generation) just gets several times slower.
//!
//! Evidence, strongest first:
//! - Measured: per-process system-memory allocations (`gpu_mem_shared`:
//!   Windows WDDM "Shared Usage", AMD GTT). Desktop apps keep a little there
//!   normally, so small amounts only count while VRAM is under pressure.
//! - Pressure: VRAM used / total above a threshold (the next allocation may
//!   spill, or fail with OOM where the driver doesn't fall back).
//!
//! Unified-memory GPUs (Apple, APUs) can't spill in this sense.

use crate::model::{GpuInfo, GpuSample, ProcRow, Spill, SpillLevel};

#[derive(Clone, Copy, Debug)]
pub struct Thresholds {
    /// VRAM fraction considered "under pressure".
    pub pressure: f64,
    /// A process with at least this much in system memory is spilling no
    /// matter how full VRAM is (allocations placed there stay there).
    pub spill_bytes: u64,
    /// Smaller system-memory allocations count as spill only under pressure.
    pub spill_bytes_under_pressure: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        // Desktop compositors and session processes routinely keep 100-250 MiB
        // in shared memory, so the pressure threshold sits above that.
        Thresholds { pressure: 0.95, spill_bytes: 512 << 20, spill_bytes_under_pressure: 256 << 20 }
    }
}

/// Bandwidth-bound slowdown when fraction `f` of the working set is read
/// over the host link instead of VRAM: 1 / ((1-f) + f * vram/link).
pub fn slowdown(f: f64, vram_bw: f64, link_bw: f64) -> f64 {
    let f = f.clamp(0.0, 1.0);
    (1.0 - f) + f * (vram_bw / link_bw).max(1.0)
}

/// Analyse one GPU. Marks spilling processes in `procs` (rows of this GPU).
pub fn analyze(info: &GpuInfo, s: &GpuSample, procs: &mut [&mut ProcRow], t: &Thresholds) -> Spill {
    if info.integrated {
        return Spill::default();
    }
    let pressure = s.mem_frac().is_some_and(|f| f >= t.pressure);

    let mut spilled = 0u64;
    // The slowdown that matters is the biggest spiller's (a process with a
    // tiny VRAM footprint has an extreme but meaningless ratio).
    let mut biggest: Option<(u64, u64)> = None;
    for p in procs.iter_mut() {
        let shared = p.usage.gpu_mem_shared.unwrap_or(0);
        let spills = shared >= t.spill_bytes || (pressure && shared >= t.spill_bytes_under_pressure);
        p.spilling = spills;
        if !spills {
            continue;
        }
        spilled += shared;
        if biggest.is_none_or(|(b, _)| shared > b) {
            biggest = Some((shared, p.usage.gpu_mem.unwrap_or(0)));
        }
    }
    let worst = match (biggest, info.memory_bandwidth, info.pcie_bandwidth()) {
        (Some((shared, local)), Some(vram), Some(link)) => Some(slowdown(shared as f64 / (shared + local).max(1) as f64, vram, link)),
        _ => None,
    };

    if spilled > 0 {
        let n = procs.iter().filter(|p| p.spilling).count();
        return Spill {
            level: SpillLevel::Spilling,
            bytes: Some(spilled),
            slowdown: worst,
            reason: format!("{} in system memory ({n} process{})", crate::fmt::bytes(spilled), if n == 1 { "" } else { "es" }),
        };
    }
    if pressure {
        return Spill {
            level: SpillLevel::Pressure,
            bytes: None,
            slowdown: None,
            reason: format!("VRAM {:.0}% full", s.mem_frac().unwrap_or(0.0) * 100.0),
        };
    }
    Spill::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GpuProcess, PidMapping};

    const GIB: u64 = 1 << 30;

    fn gpu() -> GpuInfo {
        GpuInfo { memory_bandwidth: Some(1008e9), pcie_max_gen: Some(4), pcie_max_width: Some(16), ..Default::default() }
    }

    fn row(local: u64, shared: u64) -> ProcRow {
        ProcRow {
            gpu: 0,
            usage: GpuProcess { gpu_mem: Some(local), gpu_mem_shared: Some(shared), ..Default::default() },
            local_pid: None,
            mapping: PidMapping::Direct,
            name: String::new(),
            cmdline: String::new(),
            user: None,
            cpu: None,
            rss: None,
            runtime: None,
            spilling: false,
        }
    }

    fn sample(used: u64) -> GpuSample {
        GpuSample { mem_used: Some(used), mem_total: Some(24 * GIB), ..Default::default() }
    }

    #[test]
    fn big_shared_allocation_is_spill_even_with_free_vram() {
        // The real case seen on a 4090: WSL's VM worker kept 4.2 GiB in shared
        // memory after VRAM usage dropped to 17 GiB.
        let mut a = row(20 * GIB, 4 * GIB);
        let mut b = row(GIB / 2, 10 << 20);
        let s = analyze(&gpu(), &sample(17 * GIB), &mut [&mut a, &mut b], &Thresholds::default());
        assert_eq!(s.level, SpillLevel::Spilling);
        assert_eq!(s.bytes, Some(4 * GIB));
        assert!(a.spilling && !b.spilling);
        let x = s.slowdown.unwrap();
        assert!(x > 5.0 && x < 7.0, "{x}");
    }

    #[test]
    fn desktop_shared_allocations_are_not_spill() {
        // dwm / csrss under VRAM pressure: 85 MiB and 223 MiB shared.
        let (mut a, mut b) = (row(1400 << 20, 85 << 20), row(34 << 20, 223 << 20));
        let s = analyze(&gpu(), &sample(23 * GIB), &mut [&mut a, &mut b], &Thresholds::default());
        assert_eq!(s.level, SpillLevel::Pressure);
    }

    #[test]
    fn slowdown_comes_from_the_biggest_spiller() {
        // A process with a tiny VRAM footprint has a huge ratio; it must not
        // dominate the estimate.
        let (mut big, mut tiny) = (row(20 * GIB, 4 * GIB), row(30 << 20, 600 << 20));
        let s = analyze(&gpu(), &sample(23 * GIB), &mut [&mut big, &mut tiny], &Thresholds::default());
        assert!(s.slowdown.unwrap() < 8.0);
    }

    #[test]
    fn small_shared_counts_only_under_pressure() {
        let mut a = row(GIB, 300 << 20);
        let s = analyze(&gpu(), &sample(12 * GIB), &mut [&mut a], &Thresholds::default());
        assert_eq!(s.level, SpillLevel::None);
        let s = analyze(&gpu(), &sample(23 * GIB + GIB / 2), &mut [&mut a], &Thresholds::default());
        assert_eq!(s.level, SpillLevel::Spilling);
    }

    #[test]
    fn pressure_without_measurements() {
        let mut a = ProcRow { usage: GpuProcess::default(), ..row(0, 0) };
        let s = analyze(&gpu(), &sample(23 * GIB + GIB / 2), &mut [&mut a], &Thresholds::default());
        assert_eq!(s.level, SpillLevel::Pressure);
    }

    #[test]
    fn unified_memory_never_spills() {
        let g = GpuInfo { integrated: true, ..gpu() };
        let mut a = row(GIB, 4 * GIB);
        assert_eq!(analyze(&g, &sample(24 * GIB), &mut [&mut a], &Thresholds::default()).level, SpillLevel::None);
    }

    #[test]
    fn slowdown_formula() {
        assert!((slowdown(0.0, 1000.0, 25.0) - 1.0).abs() < 1e-9);
        assert!((slowdown(0.1, 1000.0, 25.0) - 4.9).abs() < 1e-9);
    }
}
