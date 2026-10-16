//! Windows per-process GPU usage from the OS's WDDM performance counters,
//! the same source as Task Manager. Vendor-neutral.
//!
//! NVML under WDDM reports per-process memory as N/A; these counters fill
//! that gap, give per-process engine utilization for any GPU vendor, and
//! measure spill ("Shared Usage" = GPU allocations in system memory).
//!
//! Counters (instance names encode pid + adapter LUID):
//!   \GPU Process Memory(pid_1234_luid_0x00000000_0x0000D1A5_phys_0)\Dedicated Usage
//!   \GPU Process Memory(...)\Shared Usage
//!   \GPU Engine(pid_1234_luid_..._phys_0_eng_3_engtype_3D)\Utilization Percentage
//!   \GPU Adapter Memory(luid_..._phys_0)\Shared Usage
//! Adapters are mapped LUID -> PCI bus via D3DKMT so they can be matched to
//! NVML devices. pdh.dll / gdi32.dll are loaded at runtime.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use libloading::Library;

use crate::model::{GpuInfo, GpuProcess, GpuSample, PidScope, ProcKind};

type PdhStatus = u32;
type Handle = *mut c_void;

const PDH_MORE_DATA: PdhStatus = 0x800007D2;
const PDH_FMT_DOUBLE: u32 = 0x0000_0200;
const PDH_FMT_NOCAP100: u32 = 0x0000_8000;

#[repr(C)]
struct FmtItem {
    name: *const u16,
    cstatus: u32,
    value: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AdapterInfo {
    h_adapter: u32,
    luid_low: u32,
    luid_high: i32,
    num_sources: u32,
    precise: i32,
}

#[repr(C)]
struct EnumAdapters2 {
    num: u32,
    adapters: *mut AdapterInfo,
}

#[repr(C)]
struct QueryAdapterInfo {
    h_adapter: u32,
    kind: u32,
    data: *mut c_void,
    size: u32,
}

#[repr(C)]
#[derive(Default)]
struct AdapterAddress {
    bus: u32,
    device: u32,
    function: u32,
}

const KMTQAITYPE_ADAPTERADDRESS: u32 = 6;

type OpenQuery = unsafe extern "system" fn(*const u16, usize, *mut Handle) -> PdhStatus;
type AddCounter = unsafe extern "system" fn(Handle, *const u16, usize, *mut Handle) -> PdhStatus;
type Collect = unsafe extern "system" fn(Handle) -> PdhStatus;
type GetArray = unsafe extern "system" fn(Handle, u32, *mut u32, *mut u32, *mut FmtItem) -> PdhStatus;
type CloseQuery = unsafe extern "system" fn(Handle) -> PdhStatus;

/// (luid_high, luid_low)
pub type Luid = (i32, u32);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcUsage {
    /// Dedicated (VRAM) bytes.
    pub dedicated: Option<u64>,
    /// Bytes in system memory.
    pub shared: Option<u64>,
    /// Max engine utilization across engines, percent (Task Manager's "GPU").
    pub util: Option<f32>,
    pub enc: Option<f32>,
    pub dec: Option<f32>,
}

struct Pdh {
    _lib: Library,
    collect: Collect,
    get_array: GetArray,
    close: CloseQuery,
    query: Handle,
    proc_dedicated: Handle,
    proc_shared: Handle,
    engine: Handle,
    adapter_shared: Handle,
    adapter_dedicated: Handle,
    primed: bool,
}

/// Adapter-wide usage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AdapterUsage {
    pub dedicated: Option<u64>,
    pub shared: Option<u64>,
    /// Busiest engine, percent (Task Manager's adapter "GPU" figure).
    pub util: Option<f32>,
    pub enc: Option<f32>,
    pub dec: Option<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct PdhSample {
    pub procs: HashMap<(Luid, u32), ProcUsage>,
    pub adapters: HashMap<Luid, AdapterUsage>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Pdh {
    fn new() -> Option<Self> {
        unsafe {
            let lib = Library::new("pdh.dll").ok()?;
            let open: OpenQuery = *lib.get(b"PdhOpenQueryW\0").ok()?;
            let add: AddCounter = *lib.get(b"PdhAddEnglishCounterW\0").ok()?;
            let collect: Collect = *lib.get(b"PdhCollectQueryData\0").ok()?;
            let get_array: GetArray = *lib.get(b"PdhGetFormattedCounterArrayW\0").ok()?;
            let close: CloseQuery = *lib.get(b"PdhCloseQuery\0").ok()?;
            let mut query: Handle = std::ptr::null_mut();
            if open(std::ptr::null(), 0, &mut query) != 0 {
                return None;
            }
            let counter = |path: &str| {
                let mut h: Handle = std::ptr::null_mut();
                if add(query, wide(path).as_ptr(), 0, &mut h) == 0 { h } else { std::ptr::null_mut() }
            };
            let proc_dedicated = counter(r"\GPU Process Memory(*)\Dedicated Usage");
            let proc_shared = counter(r"\GPU Process Memory(*)\Shared Usage");
            let engine = counter(r"\GPU Engine(*)\Utilization Percentage");
            let adapter_shared = counter(r"\GPU Adapter Memory(*)\Shared Usage");
            let adapter_dedicated = counter(r"\GPU Adapter Memory(*)\Dedicated Usage");
            if proc_dedicated.is_null() && engine.is_null() {
                log::debug!("PDH GPU counters unavailable");
                close(query);
                return None;
            }
            Some(Pdh {
                collect,
                get_array,
                close,
                query,
                proc_dedicated,
                proc_shared,
                engine,
                adapter_shared,
                adapter_dedicated,
                primed: false,
                _lib: lib,
            })
        }
    }

    fn read(&self, counter: Handle) -> Vec<(String, f64)> {
        if counter.is_null() {
            return Vec::new();
        }
        let fmt = PDH_FMT_DOUBLE | PDH_FMT_NOCAP100;
        // Instances can appear between the size query and the read; retry.
        for _ in 0..3 {
            unsafe {
                let (mut size, mut count) = (0u32, 0u32);
                let r = (self.get_array)(counter, fmt, &mut size, &mut count, std::ptr::null_mut());
                if r != PDH_MORE_DATA || size == 0 {
                    return Vec::new();
                }
                // The buffer holds the item array followed by the name strings.
                let mut buf: Vec<u64> = vec![0; (size as usize).div_ceil(8) + 512];
                size = (buf.len() * 8) as u32;
                let items = buf.as_mut_ptr() as *mut FmtItem;
                match (self.get_array)(counter, fmt, &mut size, &mut count, items) {
                    0 => {}
                    PDH_MORE_DATA => continue,
                    _ => return Vec::new(),
                }
                let mut out = Vec::with_capacity(count as usize);
                for i in 0..count as usize {
                    let it = &*items.add(i);
                    if it.cstatus > 1 || it.name.is_null() {
                        continue;
                    }
                    let mut len = 0;
                    while *it.name.add(len) != 0 {
                        len += 1;
                    }
                    let name = String::from_utf16_lossy(std::slice::from_raw_parts(it.name, len));
                    out.push((name, it.value));
                }
                return out;
            }
        }
        Vec::new()
    }

    fn sample(&mut self) -> PdhSample {
        if unsafe { (self.collect)(self.query) } != 0 {
            return PdhSample::default();
        }
        // Rate counters (engine utilization) need two collections.
        let engine = if self.primed { self.read(self.engine) } else { Vec::new() };
        self.primed = true;
        let (procs, mut adapters) = aggregate(&self.read(self.proc_dedicated), &self.read(self.proc_shared), &engine);
        for (counter, shared) in [(self.adapter_shared, true), (self.adapter_dedicated, false)] {
            for (name, v) in self.read(counter) {
                if let Some(luid) = parse_adapter_instance(&name) {
                    let a = adapters.entry(luid).or_default();
                    let slot = if shared { &mut a.shared } else { &mut a.dedicated };
                    *slot = Some(slot.unwrap_or(0) + v.max(0.0) as u64);
                }
            }
        }
        PdhSample { procs, adapters }
    }
}

// SAFETY: the query is only touched under the mutex below.
unsafe impl Send for Pdh {}

/// One counter query per process, shared by the NVIDIA enricher and the
/// WDDM backend; a sample is reused within a tick so counters are collected once.
struct PdhCache {
    pdh: Pdh,
    last: Option<(Instant, Arc<PdhSample>)>,
}

fn pdh_cache() -> Option<&'static Mutex<PdhCache>> {
    static CACHE: OnceLock<Option<Mutex<PdhCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Pdh::new().map(|pdh| Mutex::new(PdhCache { pdh, last: None }))).as_ref()
}

pub fn pdh_sample() -> Option<Arc<PdhSample>> {
    let mut c = pdh_cache()?.lock().ok()?;
    if let Some((t, s)) = &c.last
        && t.elapsed() < Duration::from_millis(150)
    {
        return Some(s.clone());
    }
    let s = Arc::new(c.pdh.sample());
    c.last = Some((Instant::now(), s.clone()));
    Some(s)
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe { (self.close)(self.query) };
    }
}

/// Fold raw counter instances into per-(adapter, pid) usage. Memory sums
/// over instances (one per physical adapter part); engine utilization sums
/// per engine, then takes the busiest engine (Task Manager's rule). The
/// adapter figure sums every process per engine, then takes the busiest.
type Aggregated = (HashMap<(Luid, u32), ProcUsage>, HashMap<Luid, AdapterUsage>);

fn aggregate(dedicated: &[(String, f64)], shared: &[(String, f64)], engine: &[(String, f64)]) -> Aggregated {
    let mut out: HashMap<(Luid, u32), ProcUsage> = HashMap::new();
    for (list, shared_mem) in [(dedicated, false), (shared, true)] {
        for (name, v) in list {
            if let Some((pid, luid, _)) = parse_instance(name) {
                let e = out.entry((luid, pid)).or_default();
                let slot = if shared_mem { &mut e.shared } else { &mut e.dedicated };
                *slot = Some(slot.unwrap_or(0) + v.max(0.0) as u64);
            }
        }
    }
    let mut per_engine: HashMap<(Luid, u32, &str), (f64, &str)> = HashMap::new();
    let mut adapter_engine: HashMap<(Luid, &str), (f64, &str)> = HashMap::new();
    for (name, v) in engine {
        if let Some((pid, luid, rest)) = parse_instance(name) {
            let (eng, ty) = rest.split_once("_engtype_").unwrap_or((rest, ""));
            per_engine.entry((luid, pid, eng)).or_insert((0.0, ty)).0 += v;
            adapter_engine.entry((luid, eng)).or_insert((0.0, ty)).0 += v;
        }
    }
    fn fold(util: &mut Option<f32>, enc: &mut Option<f32>, dec: &mut Option<f32>, v: f64, ty: &str) {
        let v = v.clamp(0.0, 100.0) as f32;
        *util = Some(util.unwrap_or(0.0).max(v));
        let slot = if ty.starts_with("VideoEncode") {
            Some(enc)
        } else if ty.starts_with("VideoDecode") {
            Some(dec)
        } else {
            None
        };
        if let Some(slot) = slot {
            *slot = Some(slot.unwrap_or(0.0).max(v));
        }
    }
    for ((luid, pid, _), (v, ty)) in per_engine {
        let e = out.entry((luid, pid)).or_default();
        fold(&mut e.util, &mut e.enc, &mut e.dec, v, ty);
    }
    let mut adapters: HashMap<Luid, AdapterUsage> = HashMap::new();
    for ((luid, _), (v, ty)) in adapter_engine {
        let a = adapters.entry(luid).or_default();
        fold(&mut a.util, &mut a.enc, &mut a.dec, v, ty);
    }
    (out, adapters)
}

/// "pid_1234_luid_0x00000000_0x0000D1A5_phys_0[_eng_3_engtype_3D]"
fn parse_instance(name: &str) -> Option<(u32, Luid, &str)> {
    let rest = name.strip_prefix("pid_")?;
    let (pid, rest) = rest.split_once('_')?;
    let (luid, rest) = parse_luid(rest)?;
    Some((pid.parse().ok()?, luid, rest))
}

/// "luid_0x00000000_0x0000D1A5_phys_0"
fn parse_adapter_instance(name: &str) -> Option<Luid> {
    parse_luid(name).map(|(l, _)| l)
}

fn parse_luid(s: &str) -> Option<(Luid, &str)> {
    let rest = s.strip_prefix("luid_0x")?;
    let (hi, rest) = rest.split_once("_0x")?;
    let (lo, rest) = rest.split_once('_').unwrap_or((rest, ""));
    Some(((u32::from_str_radix(hi, 16).ok()? as i32, u32::from_str_radix(lo, 16).ok()?), rest))
}

#[derive(Clone, Debug, Default)]
pub struct AdapterDesc {
    pub luid: Luid,
    pub name: String,
    pub vendor_id: Option<u32>,
    pub device_id: Option<u32>,
    pub pci: Option<(u32, u32, u32)>,
    /// Dedicated VRAM (small carve-out on integrated GPUs).
    pub dedicated: u64,
    /// System memory the adapter may use.
    pub shared_system: u64,
    /// KMTQAITYPE_ADAPTERTYPE flags (None if the query failed).
    pub flags: Option<u32>,
}

/// D3DKMT_ADAPTERTYPE bits.
pub mod adapter_flags {
    pub const RENDER: u32 = 1 << 0;
    pub const SOFTWARE: u32 = 1 << 2;
    pub const HYBRID_DISCRETE: u32 = 1 << 4;
    pub const HYBRID_INTEGRATED: u32 = 1 << 5;
    pub const INDIRECT_DISPLAY: u32 = 1 << 6;
}

impl AdapterDesc {
    /// A real GPU that renders (not WARP, a virtual display or an NPU).
    /// GPU-P adapters inside VMs are paravirtualized but real, so they stay.
    pub fn is_gpu(&self) -> bool {
        use adapter_flags::*;
        let flags_ok = self.flags.is_none_or(|f| f & RENDER != 0 && f & (SOFTWARE | INDIRECT_DISPLAY) == 0);
        flags_ok && self.vendor_id != Some(0x1414) && !self.name.is_empty() && !self.name.contains("Microsoft Basic")
    }

    /// Integrated GPUs live in system memory: a hybrid laptop says so, and
    /// otherwise a tiny dedicated carve-out gives it away.
    pub fn is_integrated(&self) -> bool {
        use adapter_flags::*;
        match self.flags {
            Some(f) if f & HYBRID_INTEGRATED != 0 => true,
            Some(f) if f & HYBRID_DISCRETE != 0 => false,
            _ => self.dedicated < (1 << 30),
        }
    }
}

const KMTQAITYPE_GETSEGMENTSIZE: u32 = 3;
const KMTQAITYPE_ADAPTERREGISTRYINFO: u32 = 8;
const KMTQAITYPE_ADAPTERTYPE: u32 = 15;
const KMTQAITYPE_PHYSICALADAPTERDEVICEIDS: u32 = 31;

#[repr(C)]
struct SegmentSizeInfo {
    dedicated_video: u64,
    dedicated_system: u64,
    shared_system: u64,
}

#[repr(C)]
struct RegistryInfo {
    adapter_string: [u16; 260],
    bios_string: [u16; 260],
    dac_type: [u16; 260],
    chip_type: [u16; 260],
}

#[repr(C)]
#[derive(Default)]
struct QueryDeviceIds {
    physical_adapter_index: u32,
    vendor_id: u32,
    device_id: u32,
    sub_vendor_id: u32,
    sub_system_id: u32,
    revision_id: u32,
    bus_type: u32,
}

fn wstr(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end]).trim().to_string()
}

/// Every WDDM adapter with its name, IDs, PCI location and memory sizes.
pub fn adapter_descs() -> Vec<AdapterDesc> {
    let mut out = Vec::new();
    unsafe {
        let Ok(lib) = Library::new("gdi32.dll") else { return out };
        type Enum = unsafe extern "system" fn(*mut EnumAdapters2) -> i32;
        type Query = unsafe extern "system" fn(*const QueryAdapterInfo) -> i32;
        type Close = unsafe extern "system" fn(*const u32) -> i32;
        let (Ok(en), Ok(q), Ok(cl)) = (
            lib.get::<Enum>(b"D3DKMTEnumAdapters2\0"),
            lib.get::<Query>(b"D3DKMTQueryAdapterInfo\0"),
            lib.get::<Close>(b"D3DKMTCloseAdapter\0"),
        ) else {
            return out;
        };
        let mut e = EnumAdapters2 { num: 0, adapters: std::ptr::null_mut() };
        if en(&mut e) != 0 || e.num == 0 {
            return out;
        }
        let mut list = vec![AdapterInfo::default(); e.num as usize];
        e.adapters = list.as_mut_ptr();
        if en(&mut e) != 0 {
            return out;
        }
        let query =
            |h: u32, kind: u32, data: *mut c_void, size: usize| q(&QueryAdapterInfo { h_adapter: h, kind, data, size: size as u32 }) == 0;
        for a in &list[..(e.num as usize).min(list.len())] {
            let mut d = AdapterDesc { luid: (a.luid_high, a.luid_low), ..Default::default() };
            let mut addr = AdapterAddress::default();
            if query(a.h_adapter, KMTQAITYPE_ADAPTERADDRESS, &mut addr as *mut _ as *mut c_void, std::mem::size_of::<AdapterAddress>()) {
                d.pci = Some((addr.bus, addr.device, addr.function));
            }
            let mut reg: RegistryInfo = std::mem::zeroed();
            if query(a.h_adapter, KMTQAITYPE_ADAPTERREGISTRYINFO, &mut reg as *mut _ as *mut c_void, std::mem::size_of::<RegistryInfo>()) {
                d.name = wstr(&reg.adapter_string);
            }
            let mut seg = SegmentSizeInfo { dedicated_video: 0, dedicated_system: 0, shared_system: 0 };
            if query(a.h_adapter, KMTQAITYPE_GETSEGMENTSIZE, &mut seg as *mut _ as *mut c_void, std::mem::size_of::<SegmentSizeInfo>()) {
                d.dedicated = seg.dedicated_video;
                d.shared_system = seg.shared_system;
            }
            let mut ids = QueryDeviceIds::default();
            if query(
                a.h_adapter,
                KMTQAITYPE_PHYSICALADAPTERDEVICEIDS,
                &mut ids as *mut _ as *mut c_void,
                std::mem::size_of::<QueryDeviceIds>(),
            ) {
                d.vendor_id = Some(ids.vendor_id);
                d.device_id = Some(ids.device_id);
            }
            let mut flags = 0u32;
            if query(a.h_adapter, KMTQAITYPE_ADAPTERTYPE, &mut flags as *mut _ as *mut c_void, std::mem::size_of::<u32>()) {
                d.flags = Some(flags);
            }
            cl(&a.h_adapter);
            out.push(d);
        }
    }
    out
}

/// Parse a PCI bus id "00000000:01:00.0" -> (bus, device, function).
pub fn parse_bus_id(s: &str) -> Option<(u32, u32, u32)> {
    let mut parts = s.rsplitn(3, ':');
    let devfn = parts.next()?;
    let bus = parts.next()?;
    let (dev, func) = devfn.split_once('.')?;
    Some((u32::from_str_radix(bus, 16).ok()?, u32::from_str_radix(dev, 16).ok()?, u32::from_str_radix(func, 16).ok()?))
}

/// Fills per-process data NVML can't provide under WDDM.
pub struct Enricher {
    /// WDDM adapter of each GPU (by global index).
    luids: Vec<Option<Luid>>,
    totals: Vec<Option<u64>>,
}

impl Enricher {
    pub fn new(gpus: &[GpuInfo]) -> Option<Self> {
        pdh_cache()?;
        let descs = adapter_descs();
        let luids: Vec<Option<Luid>> = gpus
            .iter()
            .map(|g| {
                let loc = g.pci_bus_id.as_deref().and_then(parse_bus_id)?;
                descs.iter().find(|d| d.pci == Some(loc)).map(|d| d.luid)
            })
            .collect();
        if luids.iter().all(Option::is_none) {
            log::debug!("no GPU matched a WDDM adapter ({} adapters)", descs.len());
        }
        Some(Enricher { luids, totals: gpus.iter().map(|g| g.memory_total).collect() })
    }

    pub fn enrich(&mut self, samples: &mut [GpuSample], procs: &mut Vec<(usize, GpuProcess)>) {
        let Some(sample) = pdh_sample() else { return };
        for (i, s) in samples.iter_mut().enumerate() {
            if let Some(Some(luid)) = self.luids.get(i) {
                s.mem_shared = s.mem_shared.or(sample.adapters.get(luid).and_then(|a| a.shared));
            }
        }
        merge(&sample.procs, &self.luids, &self.totals, procs);
    }
}

impl ProcUsage {
    /// Worth listing: holds VRAM, a real amount of shared memory, or is busy.
    /// (Every desktop app maps a few MiB of shared memory.)
    pub fn is_active(&self) -> bool {
        self.dedicated.unwrap_or(0) > 0 || self.shared.unwrap_or(0) > (16 << 20) || self.util.unwrap_or(0.0) > 0.0
    }
}

/// Fill in what the GPU backend couldn't report and add processes it didn't
/// list. The counters occasionally report garbage (terabytes): anything
/// larger than the card is dropped.
fn merge(usage: &HashMap<(Luid, u32), ProcUsage>, luids: &[Option<Luid>], totals: &[Option<u64>], procs: &mut Vec<(usize, GpuProcess)>) {
    let sane = |gpu: usize, v: Option<u64>| v.filter(|&b| totals.get(gpu).copied().flatten().is_none_or(|t| b <= t));
    let mut seen = HashSet::new();
    for (gpu, p) in procs.iter_mut() {
        let Some(Some(luid)) = luids.get(*gpu) else { continue };
        seen.insert((*gpu, p.pid));
        if let Some(u) = usage.get(&(*luid, p.pid)) {
            p.gpu_mem = p.gpu_mem.or(sane(*gpu, u.dedicated));
            p.gpu_mem_shared = p.gpu_mem_shared.or(u.shared);
            p.sm_util = p.sm_util.or(u.util);
            p.enc_util = p.enc_util.or(u.enc);
            p.dec_util = p.dec_util.or(u.dec);
        }
    }
    for (&(luid, pid), u) in usage {
        let Some(gpu) = luids.iter().position(|l| *l == Some(luid)) else { continue };
        if seen.contains(&(gpu, pid)) || !u.is_active() {
            continue;
        }
        procs.push((
            gpu,
            GpuProcess {
                pid,
                scope: PidScope::Local,
                kind: ProcKind::Graphics,
                gpu_mem: sane(gpu, u.dedicated),
                gpu_mem_shared: u.shared,
                sm_util: u.util,
                enc_util: u.enc,
                dec_util: u.dec,
                ..Default::default()
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_classification() {
        use adapter_flags::*;
        let gpu = |flags, dedicated| AdapterDesc { name: "GPU".into(), flags, dedicated, ..Default::default() };
        assert!(gpu(Some(RENDER), 24 << 30).is_gpu());
        assert!(!gpu(Some(RENDER | SOFTWARE), 0).is_gpu(), "WARP");
        assert!(!gpu(Some(RENDER | INDIRECT_DISPLAY), 0).is_gpu(), "virtual display");
        assert!(!gpu(Some(0), 0).is_gpu(), "compute-only (NPU)");
        assert!(gpu(None, 0).is_gpu(), "flags unknown: keep");
        assert!(gpu(Some(RENDER), 128 << 20).is_integrated());
        assert!(gpu(Some(RENDER | HYBRID_INTEGRATED), 4 << 30).is_integrated());
        assert!(!gpu(Some(RENDER | HYBRID_DISCRETE), 512 << 20).is_integrated());
        assert!(!gpu(Some(RENDER), 8 << 30).is_integrated());
    }

    #[test]
    fn instance_names() {
        let (pid, luid, rest) = parse_instance("pid_1234_luid_0x00000000_0x0000D1A5_phys_0_eng_3_engtype_3D").unwrap();
        assert_eq!((pid, luid, rest), (1234, (0, 0xD1A5), "phys_0_eng_3_engtype_3D"));
        let (_, _, rest) = parse_instance("pid_8_luid_0x00000001_0x0000D1A5_phys_0").unwrap();
        assert_eq!(rest, "phys_0");
        assert_eq!(parse_adapter_instance("luid_0x00000000_0x00015be1_phys_0"), Some((0, 0x15be1)));
        assert!(parse_instance("pid_x_luid_0x0_0x1_phys_0").is_none());
        assert!(parse_instance("pid_1_luid_0x0").is_none());
    }

    #[test]
    fn bus_ids() {
        assert_eq!(parse_bus_id("00000000:01:00.0"), Some((1, 0, 0)));
        assert_eq!(parse_bus_id("0000:af:1f.3"), Some((0xaf, 0x1f, 3)));
    }

    #[test]
    fn aggregation_rules() {
        let n = |s: &str, v: f64| (s.to_string(), v);
        let ded = [n("pid_5_luid_0x0_0xA_phys_0", 100.0), n("pid_5_luid_0x0_0xA_phys_1", 50.0)];
        let sh = [n("pid_5_luid_0x0_0xA_phys_0", 7.0)];
        let eng = [
            n("pid_5_luid_0x0_0xA_phys_0_eng_0_engtype_3D", 20.0),
            n("pid_5_luid_0x0_0xA_phys_0_eng_0_engtype_3D", 15.0), // same engine: sums
            n("pid_5_luid_0x0_0xA_phys_0_eng_4_engtype_VideoDecode", 60.0),
        ];
        let (procs, adapters) = aggregate(&ded, &sh, &eng);
        let u = &procs[&((0, 0xA), 5)];
        assert_eq!(adapters[&(0, 0xA)].util, Some(60.0));
        assert_eq!(u.dedicated, Some(150));
        assert_eq!(u.shared, Some(7));
        assert_eq!(u.util, Some(60.0)); // busiest engine
        assert_eq!(u.dec, Some(60.0));
    }

    #[test]
    fn merge_drops_impossible_sizes_and_adds_missing() {
        let mut usage = HashMap::new();
        usage.insert(((0, 1), 10), ProcUsage { dedicated: Some(1 << 40), ..Default::default() });
        usage.insert(((0, 1), 11), ProcUsage { dedicated: Some(1 << 20), shared: Some(1 << 30), ..Default::default() });
        let mut procs = vec![(0, GpuProcess { pid: 10, ..Default::default() })];
        merge(&usage, &[Some((0, 1))], &[Some(24 << 30)], &mut procs);
        assert_eq!(procs[0].1.gpu_mem, None);
        let added = procs.iter().find(|p| p.1.pid == 11).unwrap();
        assert_eq!(added.1.gpu_mem_shared, Some(1 << 30));
        assert_eq!(added.1.scope, PidScope::Local);
    }
}
