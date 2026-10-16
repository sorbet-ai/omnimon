//! VRAM spill detection. Vendor-neutral and pure, so it is unit-tested.
//!
//! "Spill" = GPU allocations pushed into system memory because VRAM is full
//! (Windows' sysmem fallback, AMD GTT). They're reached over PCIe, so
//! bandwidth-bound work gets slower.
//!
//! Processes keep system-memory allocations for many legitimate reasons
//! (pinned staging buffers, the desktop compositor, WSL's VM worker), so
//! shared memory alone isn't spill: it only counts while VRAM is nearly full.
//!
//! Unified-memory GPUs (Apple, APUs) can't spill in this sense.

use crate::model::{GpuInfo, GpuSample, ProcRow, Spill, SpillLevel};

#[derive(Clone, Copy, Debug)]
pub struct Thresholds {
    /// VRAM fraction shown as "pressure" (informational).
    pub pressure: f64,
    /// VRAM fraction above which system-memory allocations count as spill.
    pub spill_vram: f64,
    /// A process's system-memory allocations must be at least this big.
    pub spill_bytes: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        // Desktop compositors and session processes routinely keep 100-250 MiB
        // in shared memory, so the per-process threshold sits above that.
        Thresholds { pressure: 0.98, spill_vram: 0.95, spill_bytes: 256 << 20 }
    }
}

/// Analyse one GPU. Marks spilling processes in `procs` (rows of this GPU).
pub fn analyze(info: &GpuInfo, s: &GpuSample, procs: &mut [&mut ProcRow], t: &Thresholds) -> Spill {
    if info.integrated {
        return Spill::default();
    }
    let full = s.mem_frac().is_some_and(|f| f >= t.spill_vram);
    let mut spilled = 0u64;
    for p in procs.iter_mut() {
        let shared = p.usage.gpu_mem_shared.unwrap_or(0);
        p.spilling = full && shared >= t.spill_bytes;
        if p.spilling {
            spilled += shared;
        }
    }
    if spilled > 0 {
        let n = procs.iter().filter(|p| p.spilling).count();
        return Spill {
            level: SpillLevel::Spilling,
            bytes: Some(spilled),
            reason: format!("{} in system memory ({n} process{})", crate::fmt::bytes(spilled), if n == 1 { "" } else { "es" }),
        };
    }
    if s.mem_frac().is_some_and(|f| f >= t.pressure) {
        return Spill {
            level: SpillLevel::Pressure,
            bytes: None,
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
            container: None,
        }
    }

    fn sample(used: u64) -> GpuSample {
        GpuSample { mem_used: Some(used), mem_total: Some(24 * GIB), ..Default::default() }
    }

    #[test]
    fn shared_memory_with_free_vram_is_not_spill() {
        // WSL's VM worker keeps GiBs in shared memory while VRAM has room.
        let mut a = row(20 * GIB, 4 * GIB);
        let s = analyze(&gpu(), &sample(17 * GIB), &mut [&mut a], &Thresholds::default());
        assert_eq!(s.level, SpillLevel::None);
        assert!(!a.spilling);
    }

    #[test]
    fn shared_memory_with_full_vram_is_spill() {
        let (mut a, mut b) = (row(20 * GIB, 4 * GIB), row(GIB / 2, 10 << 20));
        let s = analyze(&gpu(), &sample(23 * GIB + GIB / 2), &mut [&mut a, &mut b], &Thresholds::default());
        assert_eq!(s.level, SpillLevel::Spilling);
        assert_eq!(s.bytes, Some(4 * GIB));
        assert!(a.spilling && !b.spilling);
    }

    #[test]
    fn desktop_shared_allocations_are_not_spill() {
        // dwm / csrss under VRAM pressure: 85 MiB and 223 MiB shared.
        let (mut a, mut b) = (row(1400 << 20, 85 << 20), row(34 << 20, 223 << 20));
        let s = analyze(&gpu(), &sample(24 * GIB - GIB / 8), &mut [&mut a, &mut b], &Thresholds::default());
        assert_eq!(s.level, SpillLevel::Pressure);
    }

    #[test]
    fn small_shared_counts_only_under_pressure() {
        let mut a = row(GIB, 300 << 20);
        let s = analyze(&gpu(), &sample(12 * GIB), &mut [&mut a], &Thresholds::default());
        assert_eq!(s.level, SpillLevel::None);
        let s = analyze(&gpu(), &sample(24 * GIB - GIB / 4), &mut [&mut a], &Thresholds::default());
        assert_eq!(s.level, SpillLevel::Spilling);
    }

    #[test]
    fn pressure_without_measurements() {
        let mut a = ProcRow { usage: GpuProcess::default(), ..row(0, 0) };
        let s = analyze(&gpu(), &sample(24 * GIB - GIB / 4), &mut [&mut a], &Thresholds::default());
        assert_eq!(s.level, SpillLevel::Pressure);
    }

    #[test]
    fn unified_memory_never_spills() {
        let g = GpuInfo { integrated: true, ..gpu() };
        let mut a = row(GIB, 4 * GIB);
        assert_eq!(analyze(&g, &sample(24 * GIB), &mut [&mut a], &Thresholds::default()).level, SpillLevel::None);
    }
}
