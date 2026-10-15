//! NVIDIA backend on top of runtime-loaded NVML.

mod ffi;

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_uint, c_ulonglong, c_void};

use ffi::*;

use super::Backend;
use crate::model::{BackendInfo, GpuInfo, GpuProcess, GpuSample, ProcKind, Vendor};

/// Call an optional NVML function pointer; `Err(code)` on failure.
macro_rules! call {
    ($f:expr $(, $a:expr)* $(,)?) => {
        match $f {
            Some(f) => {
                #[allow(unused_unsafe)]
                let r = unsafe { f($($a),*) };
                if r == NVML_SUCCESS { Ok(()) } else { Err(r) }
            }
            None => Err(NVML_ERROR_FUNCTION_NOT_FOUND),
        }
    };
}

/// Metrics we remember as "not supported" per device so we stop asking.
#[derive(Clone, Copy)]
#[repr(u8)]
enum M {
    Util,
    MemV2,
    MemV1,
    Bar1,
    Temp,
    TempMem,
    Fan,
    Power,
    PowerLimit,
    Energy,
    ClkGr,
    ClkSm,
    ClkMem,
    ClkVid,
    PState,
    PcieGen,
    PcieWidth,
    PcieTx,
    PcieRx,
    Enc,
    Dec,
    Jpg,
    Ofa,
    Throttle,
    EccCorr,
    EccUncorr,
    ComputeProcs,
    GraphicsProcs,
    MpsProcs,
    ProcUtil,
}

#[derive(Clone, Copy)]
struct Handle(nvmlDevice_t);
// SAFETY: NVML device handles are opaque tokens; NVML is thread-safe.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

struct Dev {
    h: Handle,
    info: GpuInfo,
    unsupported: u64,
    /// Last CPU timestamp (us) seen from nvmlDeviceGetProcessUtilization.
    last_util_ts: u64,
}

impl Dev {
    /// Run a query; on NOT_SUPPORTED/missing symbol, never ask again.
    fn q<T>(&mut self, m: M, f: impl FnOnce() -> Result<T, nvmlReturn_t>) -> Option<T> {
        let bit = 1u64 << (m as u8);
        if self.unsupported & bit != 0 {
            return None;
        }
        match f() {
            Ok(v) => Some(v),
            Err(NVML_ERROR_NOT_SUPPORTED | NVML_ERROR_FUNCTION_NOT_FOUND | NVML_ERROR_INVALID_ARGUMENT) => {
                self.unsupported |= bit;
                None
            }
            Err(_) => None,
        }
    }
}

pub struct Nvml {
    api: Api,
    devs: Vec<Dev>,
    info: BackendInfo,
    /// nvmlProcessInfo_t stride used by the `_v3` process functions.
    v3_stride: usize,
}

impl Nvml {
    pub fn new() -> Result<Self, String> {
        let api = Api::load()?;
        let r = match (api.init_v2, api.init_v1) {
            (Some(f), _) | (None, Some(f)) => unsafe { f() },
            _ => unreachable!(),
        };
        if r != NVML_SUCCESS {
            return Err(format!("NVML init failed ({}): {}", api.path, api.err_str(r)));
        }

        let driver = sys_string(api.sys_driver_version);
        let nvml_ver = sys_string(api.sys_nvml_version);
        let mut cuda: c_int = 0;
        let cuda = call!(api.sys_cuda_version_v2, &mut cuda)
            .or_else(|_| call!(api.sys_cuda_version, &mut cuda))
            .ok()
            .filter(|_| cuda > 0)
            .map(|_| format!("{}.{}", cuda / 1000, (cuda % 1000) / 10));
        let driver_major: u32 = driver
            .as_deref()
            .and_then(|d| d.split('.').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        // r530 briefly appended `usedGpuCcProtectedMemory` to nvmlProcessInfo_t
        // (32 bytes); r535 moved it to the DetailList API. Identify that NVML by
        // its symbol set (same trick as nvitop), with the version as a backup.
        let legacy_v3 = (api.conf_compute_mem_size_info.is_some() && api.running_process_detail_list.is_none())
            || (530..535).contains(&driver_major);
        let v3_stride = if legacy_v3 { 32 } else { 24 };

        let mut count: c_uint = 0;
        call!(api.count_v2, &mut count)
            .or_else(|_| call!(api.count_v1, &mut count))
            .map_err(|e| format!("nvmlDeviceGetCount: {}", api.err_str(e)))?;

        let mut devs = Vec::new();
        for i in 0..count {
            let mut h: nvmlDevice_t = std::ptr::null_mut();
            let r = call!(api.handle_by_index_v2, i, &mut h).or_else(|_| call!(api.handle_by_index_v1, i, &mut h));
            if r.is_err() {
                // e.g. a GPU we lack permission for; skip it rather than fail.
                continue;
            }
            let info = static_info(&api, h, i as usize);
            devs.push(Dev { h: Handle(h), info, unsupported: 0, last_util_ts: 0 });
        }

        let info = BackendInfo {
            name: "NVML".into(),
            library: Some(api.path.clone()),
            driver_version: driver,
            api_version: nvml_ver,
            runtime_version: cuda,
        };
        Ok(Nvml { api, devs, info, v3_stride })
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        let _ = call!(self.api.shutdown);
    }
}

impl Backend for Nvml {
    fn info(&self) -> BackendInfo {
        self.info.clone()
    }

    fn gpus(&self) -> Vec<GpuInfo> {
        self.devs.iter().map(|d| d.info.clone()).collect()
    }

    fn sample(&mut self) -> Vec<GpuSample> {
        let api = &self.api;
        let dm = self.v3_stride;
        if self.devs.len() <= 1 {
            return self.devs.iter_mut().map(|d| sample_dev(api, d, dm)).collect();
        }
        // Some NVML calls (PCIe throughput) block ~20ms each; sample GPUs in parallel.
        std::thread::scope(|s| {
            let hs: Vec<_> = self.devs.iter_mut().map(|d| s.spawn(move || sample_dev(api, d, dm))).collect();
            hs.into_iter()
                .map(|h| h.join().unwrap_or_else(|_| GpuSample { error: Some("sampler panicked".into()), ..Default::default() }))
                .collect()
        })
    }
}

fn sys_string(f: Option<unsafe extern "C" fn(*mut c_char, c_uint) -> nvmlReturn_t>) -> Option<String> {
    let mut buf = [0 as c_char; 96];
    call!(f, buf.as_mut_ptr(), buf.len() as c_uint).ok().map(|_| cstr(&buf))
}

fn dev_string(f: Option<unsafe extern "C" fn(nvmlDevice_t, *mut c_char, c_uint) -> nvmlReturn_t>, h: nvmlDevice_t) -> Option<String> {
    let mut buf = [0 as c_char; 96];
    call!(f, h, buf.as_mut_ptr(), buf.len() as c_uint).ok().map(|_| cstr(&buf)).filter(|s| !s.is_empty())
}

fn dev_u32(f: Option<unsafe extern "C" fn(nvmlDevice_t, *mut c_uint) -> nvmlReturn_t>, h: nvmlDevice_t) -> Option<u32> {
    let mut v: c_uint = 0;
    call!(f, h, &mut v).ok().map(|_| v)
}

fn dev_u32_arg(f: Option<unsafe extern "C" fn(nvmlDevice_t, c_uint, *mut c_uint) -> nvmlReturn_t>, h: nvmlDevice_t, a: c_uint) -> Result<u32, nvmlReturn_t> {
    let mut v: c_uint = 0;
    call!(f, h, a, &mut v).map(|_| v)
}

fn arch_name(a: u32) -> Option<&'static str> {
    Some(match a {
        2 => "Kepler",
        3 => "Maxwell",
        4 => "Pascal",
        5 => "Volta",
        6 => "Turing",
        7 => "Ampere",
        8 => "Ada Lovelace",
        9 => "Hopper",
        10 => "Blackwell",
        _ => return None,
    })
}

fn static_info(api: &Api, h: nvmlDevice_t, index: usize) -> GpuInfo {
    let name = dev_string(api.name, h).unwrap_or_else(|| format!("NVIDIA GPU {index}"));
    let mut g = GpuInfo::new(index, Vendor::Nvidia, name);
    g.uuid = dev_string(api.uuid, h);
    g.serial = dev_string(api.serial, h).filter(|s| s != "0" && !s.starts_with("N/A"));
    g.vbios = dev_string(api.vbios, h);

    let mut pci: nvmlPciInfo_t = unsafe { std::mem::zeroed() };
    if call!(api.pci_info_v3, h, &mut pci).or_else(|_| call!(api.pci_info_v2, h, &mut pci)).is_ok() {
        let id = cstr(&pci.bus_id);
        g.pci_bus_id = Some(if id.is_empty() { cstr(&pci.bus_id_legacy) } else { id });
    }
    g.minor_number = dev_u32(api.minor_number, h);
    let (mut maj, mut min): (c_int, c_int) = (0, 0);
    if call!(api.cuda_cc, h, &mut maj, &mut min).is_ok() {
        g.compute_capability = Some(format!("{maj}.{min}"));
    }
    g.architecture = dev_u32(api.architecture, h).and_then(arch_name).map(String::from);
    g.cores = dev_u32(api.num_cores, h).filter(|&c| c > 0);
    g.persistence_mode = dev_u32(api.persistence_mode, h).map(|v| v == 1);
    g.display_active = dev_u32(api.display_active, h).map(|v| v == 1);
    g.compute_mode = dev_u32(api.compute_mode, h).map(|m| {
        match m {
            0 => "Default",
            1 => "Exclusive Thread",
            2 => "Prohibited",
            3 => "Exclusive Process",
            _ => "Unknown",
        }
        .to_string()
    });
    let (mut cur, mut pend): (c_uint, c_uint) = (0, 0);
    if call!(api.mig_mode, h, &mut cur, &mut pend).is_ok() {
        g.mig_enabled = Some(cur == 1);
    }
    g.num_fans = dev_u32(api.num_fans, h).unwrap_or_else(|| if dev_u32(api.fan_speed, h).is_some() { 1 } else { 0 });

    let mut m1 = nvmlMemory_t::default();
    if call!(api.memory_v1, h, &mut m1).is_ok() {
        g.memory_total = Some(m1.total);
    }
    let (mut pmin, mut pmax): (c_uint, c_uint) = (0, 0);
    if call!(api.power_limit_constraints, h, &mut pmin, &mut pmax).is_ok() {
        g.power_limit_min = Some(pmin as f64 / 1000.0);
        g.power_limit_max = Some(pmax as f64 / 1000.0);
    }
    g.power_limit_default = dev_u32(api.power_default_limit, h).map(|v| v as f64 / 1000.0);
    g.max_clock_graphics = dev_u32_arg(api.max_clock_info, h, NVML_CLOCK_GRAPHICS).ok().filter(|&v| v > 0);
    g.max_clock_sm = dev_u32_arg(api.max_clock_info, h, NVML_CLOCK_SM).ok().filter(|&v| v > 0);
    g.max_clock_mem = dev_u32_arg(api.max_clock_info, h, NVML_CLOCK_MEM).ok().filter(|&v| v > 0);
    g.max_clock_video = dev_u32_arg(api.max_clock_info, h, NVML_CLOCK_VIDEO).ok().filter(|&v| v > 0);
    g.pcie_max_gen = dev_u32(api.pcie_max_gen, h);
    g.pcie_max_width = dev_u32(api.pcie_max_width, h);
    g.temp_shutdown = dev_u32_arg(api.temperature_threshold, h, NVML_TEMPERATURE_THRESHOLD_SHUTDOWN).ok().filter(|&v| v > 0);
    g.temp_slowdown = dev_u32_arg(api.temperature_threshold, h, NVML_TEMPERATURE_THRESHOLD_SLOWDOWN).ok().filter(|&v| v > 0);
    g.temp_max_operating = dev_u32_arg(api.temperature_threshold, h, NVML_TEMPERATURE_THRESHOLD_GPU_MAX).ok().filter(|&v| v > 0);
    g
}

fn throttle_names(bits: u64) -> Vec<String> {
    const NAMES: [(u64, &str); 9] = [
        (0x1, "idle"),
        (0x2, "app clocks"),
        (0x4, "power cap"),
        (0x8, "hw slowdown"),
        (0x10, "sync boost"),
        (0x20, "sw thermal"),
        (0x40, "hw thermal"),
        (0x80, "power brake"),
        (0x100, "display clocks"),
    ];
    NAMES.iter().filter(|(b, _)| bits & b != 0).map(|(_, n)| n.to_string()).collect()
}

fn sample_dev(api: &Api, d: &mut Dev, v3_stride: usize) -> GpuSample {
    let h = d.h.0;
    let mut s = GpuSample::default();

    // Memory: v2 excludes driver-reserved memory from `used` (matches nvidia-smi).
    let mut m2 = nvmlMemory_v2_t { version: NVML_MEMORY_V2, ..Default::default() };
    let mut lost = false;
    let v2 = d.q(M::MemV2, || call!(api.memory_v2, h, &mut m2).map(|_| ()));
    if v2.is_some() {
        s.mem_used = Some(m2.used);
        s.mem_total = Some(m2.total);
        s.mem_reserved = Some(m2.reserved);
    } else {
        let mut m1 = nvmlMemory_t::default();
        match call!(api.memory_v1, h, &mut m1) {
            Ok(()) => {
                s.mem_used = Some(m1.used);
                s.mem_total = Some(m1.total);
            }
            Err(NVML_ERROR_GPU_IS_LOST) => lost = true,
            Err(_) => {}
        }
    }
    if lost {
        s.error = Some("GPU is lost (fell off the bus)".into());
        return s;
    }

    let mut u = nvmlUtilization_t::default();
    if d.q(M::Util, || call!(api.utilization, h, &mut u)).is_some() {
        s.util_gpu = Some(u.gpu as f32);
        s.util_mem = Some(u.memory as f32);
    }
    let mut b = nvmlBAR1Memory_t::default();
    if d.q(M::Bar1, || call!(api.bar1, h, &mut b)).is_some() && b.total > 0 {
        s.bar1_used = Some(b.used);
        s.bar1_total = Some(b.total);
    }

    s.temp = d.q(M::Temp, || dev_u32_arg(api.temperature, h, NVML_TEMPERATURE_GPU));
    s.temp_mem = d.q(M::TempMem, || {
        let mut fv: nvmlFieldValue_t = unsafe { std::mem::zeroed() };
        fv.field_id = NVML_FI_DEV_MEMORY_TEMP;
        call!(api.field_values, h, 1, &mut fv)?;
        if fv.nvml_return != NVML_SUCCESS {
            return Err(fv.nvml_return);
        }
        let v = match fv.value_type {
            0 => f64::from_bits(fv.value) as u32,
            _ => fv.value as u32,
        };
        if v == 0 || v > 200 { Err(NVML_ERROR_NOT_SUPPORTED) } else { Ok(v) }
    });

    let nf = d.info.num_fans;
    s.fans = d
        .q(M::Fan, || {
            let mut fans = Vec::new();
            if api.fan_speed_v2.is_some() && nf > 0 {
                for i in 0..nf {
                    if let Ok(v) = dev_u32_arg(api.fan_speed_v2, h, i) {
                        fans.push(v);
                    }
                }
            }
            if fans.is_empty() {
                let mut v: c_uint = 0;
                call!(api.fan_speed, h, &mut v)?;
                fans.push(v);
            }
            Ok(fans)
        })
        .unwrap_or_default();

    s.power = d.q(M::Power, || {
        let mut v: c_uint = 0;
        call!(api.power_usage, h, &mut v).map(|_| v as f64 / 1000.0)
    });
    s.power_limit = d.q(M::PowerLimit, || {
        let mut v: c_uint = 0;
        call!(api.enforced_power_limit, h, &mut v).map(|_| v as f64 / 1000.0)
    });
    s.energy_joules = d.q(M::Energy, || {
        let mut v: c_ulonglong = 0;
        call!(api.total_energy, h, &mut v).map(|_| v as f64 / 1000.0)
    });

    s.clock_graphics = d.q(M::ClkGr, || dev_u32_arg(api.clock_info, h, NVML_CLOCK_GRAPHICS));
    s.clock_sm = d.q(M::ClkSm, || dev_u32_arg(api.clock_info, h, NVML_CLOCK_SM));
    s.clock_mem = d.q(M::ClkMem, || dev_u32_arg(api.clock_info, h, NVML_CLOCK_MEM));
    s.clock_video = d.q(M::ClkVid, || dev_u32_arg(api.clock_info, h, NVML_CLOCK_VIDEO));
    s.pstate = d
        .q(M::PState, || {
            let mut v: c_uint = 0;
            call!(api.performance_state, h, &mut v).map(|_| v)
        })
        .filter(|&p| p < 32);

    s.pcie_gen = d.q(M::PcieGen, || {
        let mut v: c_uint = 0;
        call!(api.pcie_gen, h, &mut v).map(|_| v)
    });
    s.pcie_width = d.q(M::PcieWidth, || {
        let mut v: c_uint = 0;
        call!(api.pcie_width, h, &mut v).map(|_| v)
    });
    s.pcie_tx = d.q(M::PcieTx, || dev_u32_arg(api.pcie_throughput, h, NVML_PCIE_UTIL_TX_BYTES).map(|v| v as u64 * 1024));
    s.pcie_rx = d.q(M::PcieRx, || dev_u32_arg(api.pcie_throughput, h, NVML_PCIE_UTIL_RX_BYTES).map(|v| v as u64 * 1024));

    let util_period = |f: Option<unsafe extern "C" fn(nvmlDevice_t, *mut c_uint, *mut c_uint) -> nvmlReturn_t>| {
        let (mut v, mut p): (c_uint, c_uint) = (0, 0);
        call!(f, h, &mut v, &mut p).map(|_| v as f32)
    };
    s.enc_util = d.q(M::Enc, || util_period(api.encoder_util));
    s.dec_util = d.q(M::Dec, || util_period(api.decoder_util));
    s.jpg_util = d.q(M::Jpg, || util_period(api.jpg_util));
    s.ofa_util = d.q(M::Ofa, || util_period(api.ofa_util));

    if let Some(bits) = d.q(M::Throttle, || {
        let mut v: c_ulonglong = 0;
        call!(api.clocks_event_reasons, h, &mut v)
            .or_else(|_| call!(api.clocks_throttle_reasons, h, &mut v))
            .map(|_| v)
    }) {
        s.throttle = throttle_names(bits);
    }

    let ecc = |t: c_uint| {
        let mut v: c_ulonglong = 0;
        call!(api.total_ecc_errors, h, t, NVML_VOLATILE_ECC, &mut v).map(|_| v)
    };
    s.ecc_corrected = d.q(M::EccCorr, || ecc(NVML_MEMORY_ERROR_TYPE_CORRECTED));
    s.ecc_uncorrected = d.q(M::EccUncorr, || ecc(NVML_MEMORY_ERROR_TYPE_UNCORRECTED));

    s.processes = processes(api, d, v3_stride);
    s
}

/// Raw entry parsed from any nvmlProcessInfo_t version.
struct RawProc {
    pid: u32,
    mem: Option<u64>,
    gpu_instance: Option<u32>,
}

type ProcFn = Option<unsafe extern "C" fn(nvmlDevice_t, *mut c_uint, *mut c_void) -> nvmlReturn_t>;

/// Query a running-process list, coping with the three nvmlProcessInfo_t
/// layouts NVML has shipped:
///   v1 {pid, mem}                    16 bytes
///   v2/v3 {pid, mem, gi, ci}         24 bytes
///   v3 on r530 {.. , ccProtectedMem} 32 bytes (reverted in r535)
fn list_procs(h: nvmlDevice_t, v3: ProcFn, v2: ProcFn, v1: ProcFn, v3_stride: usize) -> Result<Vec<RawProc>, nvmlReturn_t> {
    let (f, strides): (_, &[usize]) = if v3.is_some() {
        (v3, if v3_stride == 32 { &[32, 24] } else { &[24, 32] })
    } else if v2.is_some() {
        (v2, &[24])
    } else {
        (v1, &[16])
    };
    let Some(func) = f else { return Err(NVML_ERROR_FUNCTION_NOT_FOUND) };

    let mut count: c_uint = 0;
    let r = unsafe { func(h, &mut count, std::ptr::null_mut()) };
    match r {
        NVML_SUCCESS => return Ok(Vec::new()),
        NVML_ERROR_INSUFFICIENT_SIZE => {}
        e => return Err(e),
    }
    // Leave headroom: processes may start between the two calls. Size the buffer
    // for the largest layout so a stride mismatch can never overflow it.
    let mut cap = count + 8;
    let mut buf: Vec<u64> = vec![0; cap as usize * 32 / 8];
    let r = unsafe { func(h, &mut cap, buf.as_mut_ptr() as *mut c_void) };
    if r != NVML_SUCCESS {
        return Err(r);
    }
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len() * 8) };
    let n = cap as usize;

    let parse = |stride: usize| -> Option<Vec<RawProc>> {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let o = i * stride;
            let pid = u32::from_ne_bytes(bytes[o..o + 4].try_into().ok()?);
            let mem = u64::from_ne_bytes(bytes[o + 8..o + 16].try_into().ok()?);
            let gi = if stride >= 24 { Some(u32::from_ne_bytes(bytes[o + 16..o + 20].try_into().ok()?)) } else { None };
            if pid == 0 || pid > (1 << 30) {
                return None;
            }
            if let Some(g) = gi {
                if g != u32::MAX && g > 64 {
                    return None;
                }
            }
            out.push(RawProc {
                pid,
                mem: (mem != NVML_VALUE_NOT_AVAILABLE).then_some(mem),
                gpu_instance: gi.filter(|&g| g != u32::MAX),
            });
        }
        Some(out)
    };
    for &st in strides {
        if let Some(v) = parse(st) {
            return Ok(v);
        }
    }
    Ok(Vec::new())
}

fn processes(api: &Api, d: &mut Dev, v3_stride: usize) -> Vec<GpuProcess> {
    let h = d.h.0;
    let mut map: HashMap<u32, GpuProcess> = HashMap::new();
    let mut add = |list: Vec<RawProc>, kind: ProcKind| {
        for p in list {
            map.entry(p.pid)
                .and_modify(|e| {
                    if e.kind != kind {
                        e.kind = ProcKind::Both;
                    }
                    // Same memory is reported in both lists; keep the larger.
                    e.gpu_mem = e.gpu_mem.max(p.mem);
                })
                .or_insert(GpuProcess {
                    pid: p.pid,
                    pid_is_local: false,
                    kind,
                    gpu_mem: p.mem,
                    sm_util: None,
                    mem_util: None,
                    enc_util: None,
                    dec_util: None,
                    gpu_instance: p.gpu_instance,
                    name: None,
                });
        }
    };
    if let Some(l) = d.q(M::ComputeProcs, || list_procs(h, api.compute_procs_v3, api.compute_procs_v2, api.compute_procs_v1, v3_stride)) {
        add(l, ProcKind::Compute);
    }
    if let Some(l) = d.q(M::GraphicsProcs, || list_procs(h, api.graphics_procs_v3, api.graphics_procs_v2, api.graphics_procs_v1, v3_stride)) {
        add(l, ProcKind::Graphics);
    }
    if let Some(l) = d.q(M::MpsProcs, || list_procs(h, api.mps_procs_v3, api.mps_procs_v2, None, v3_stride)) {
        add(l, ProcKind::Compute);
    }

    if !map.is_empty() {
        // Look back ~1s (like nvitop) so the sample buffer is never empty just
        // because our tick and the driver's sampling period don't line up.
        let now_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let last = now_us.saturating_sub(1_500_000).max(d.last_util_ts.saturating_sub(1_000_000));
        let samples = d.q(M::ProcUtil, || {
            let mut n: c_uint = 0;
            let r = call!(api.process_utilization, h, std::ptr::null_mut(), &mut n, last);
            match r {
                Ok(()) => return Ok(Vec::new()),
                Err(NVML_ERROR_INSUFFICIENT_SIZE) => {}
                Err(NVML_ERROR_NOT_FOUND) => return Ok(Vec::new()),
                Err(e) => return Err(e),
            }
            let mut buf = vec![nvmlProcessUtilizationSample_t::default(); n as usize + 8];
            let mut n2 = buf.len() as c_uint;
            match call!(api.process_utilization, h, buf.as_mut_ptr(), &mut n2, last) {
                Ok(()) => {
                    buf.truncate(n2 as usize);
                    Ok(buf)
                }
                Err(NVML_ERROR_NOT_FOUND) => Ok(Vec::new()),
                Err(e) => Err(e),
            }
        });
        if let Some(samples) = samples {
            // Keep the newest sample per pid.
            let mut newest: HashMap<u32, nvmlProcessUtilizationSample_t> = HashMap::new();
            for smp in samples {
                d.last_util_ts = d.last_util_ts.max(smp.time_stamp);
                let e = newest.entry(smp.pid).or_insert(smp);
                if smp.time_stamp > e.time_stamp {
                    *e = smp;
                }
            }
            let pct = |v: c_uint| (v <= 100).then_some(v as f32);
            // If memory is N/A (WDDM, MIG) a missing sample means "unknown", not 0.
            let zero = if map.values().any(|p| p.gpu_mem.is_none()) { None } else { Some(0.0) };
            for p in map.values_mut() {
                match newest.get(&p.pid) {
                    Some(smp) => {
                        p.sm_util = pct(smp.sm_util);
                        p.mem_util = pct(smp.mem_util);
                        p.enc_util = pct(smp.enc_util);
                        p.dec_util = pct(smp.dec_util);
                    }
                    None => {
                        p.sm_util = zero;
                        p.mem_util = zero;
                        p.enc_util = zero;
                        p.dec_util = zero;
                    }
                }
            }
        }
    }

    let mut v: Vec<GpuProcess> = map.into_values().collect();
    v.sort_by_key(|p| p.pid);
    v
}
