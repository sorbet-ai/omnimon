//! Windows per-process GPU usage from the OS's WDDM performance counters,
//! the same source as Task Manager. Vendor-neutral.
//!
//! NVML under WDDM reports per-process memory as N/A; these counters fill
//! that gap (and give per-process engine utilization for any GPU vendor).
//!
//! Counters (instance names encode pid + adapter LUID):
//!   \GPU Process Memory(pid_1234_luid_0x00000000_0x0000D1A5_phys_0)\Dedicated Usage
//!   \GPU Engine(pid_1234_luid_..._phys_0_eng_3_engtype_3D)\Utilization Percentage
//! Adapters are mapped LUID -> PCI bus via D3DKMT so they can be matched to
//! NVML devices. pdh.dll / gdi32.dll are loaded at runtime.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::collections::HashMap;
use std::ffi::c_void;

use libloading::Library;

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

#[derive(Clone, Debug, Default)]
pub struct ProcUsage {
    /// Dedicated (VRAM) bytes.
    pub dedicated: Option<u64>,
    /// Max engine utilization across engines, percent (Task Manager's "GPU").
    pub util: Option<f32>,
    pub enc: Option<f32>,
    pub dec: Option<f32>,
}

pub struct Pdh {
    _lib: Library,
    collect: Collect,
    get_array: GetArray,
    close: CloseQuery,
    query: Handle,
    mem: Handle,
    engine: Handle,
    primed: bool,
    /// LUID -> PCI (bus, device, function).
    pub adapters: HashMap<Luid, (u32, u32, u32)>,
}

// SAFETY: PDH handles are used from one thread at a time (the sampler).
unsafe impl Send for Pdh {}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Pdh {
    pub fn new() -> Option<Self> {
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
            let mut mem: Handle = std::ptr::null_mut();
            let mut engine: Handle = std::ptr::null_mut();
            let ok_mem = add(query, wide(r"\GPU Process Memory(*)\Dedicated Usage").as_ptr(), 0, &mut mem) == 0;
            let ok_eng = add(query, wide(r"\GPU Engine(*)\Utilization Percentage").as_ptr(), 0, &mut engine) == 0;
            if !ok_mem && !ok_eng {
                close(query);
                return None;
            }
            Some(Pdh {
                collect,
                get_array,
                close,
                query,
                mem: if ok_mem { mem } else { std::ptr::null_mut() },
                engine: if ok_eng { engine } else { std::ptr::null_mut() },
                primed: false,
                adapters: adapters(),
                _lib: lib,
            })
        }
    }

    fn read(&self, counter: Handle) -> Vec<(String, f64)> {
        if counter.is_null() {
            return Vec::new();
        }
        unsafe {
            let (mut size, mut count) = (0u32, 0u32);
            let r = (self.get_array)(counter, PDH_FMT_DOUBLE | PDH_FMT_NOCAP100, &mut size, &mut count, std::ptr::null_mut());
            if r != PDH_MORE_DATA || size == 0 {
                return Vec::new();
            }
            // Buffer holds the item array followed by the name strings.
            let mut buf: Vec<u64> = vec![0; (size as usize).div_ceil(8) + 1];
            let items = buf.as_mut_ptr() as *mut FmtItem;
            if (self.get_array)(counter, PDH_FMT_DOUBLE | PDH_FMT_NOCAP100, &mut size, &mut count, items) != 0 {
                return Vec::new();
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
            out
        }
    }

    /// Per (luid, pid) usage. Rate counters need two collections, so the
    /// first call primes and returns memory only.
    pub fn sample(&mut self) -> HashMap<(Luid, u32), ProcUsage> {
        let mut out: HashMap<(Luid, u32), ProcUsage> = HashMap::new();
        if unsafe { (self.collect)(self.query) } != 0 {
            return out;
        }
        for (name, v) in self.read(self.mem) {
            if let Some((pid, luid, _)) = parse_instance(&name) {
                let e = out.entry((luid, pid)).or_default();
                e.dedicated = Some(e.dedicated.unwrap_or(0) + v.max(0.0) as u64);
            }
        }
        if self.primed {
            // Sum per (pid, engine), then take the max engine (Task Manager's rule).
            let mut per_engine: HashMap<(Luid, u32, String), (f64, String)> = HashMap::new();
            for (name, v) in self.read(self.engine) {
                if let Some((pid, luid, rest)) = parse_instance(&name) {
                    let eng = rest.split("_engtype_").next().unwrap_or("").to_string();
                    let ty = rest.split("_engtype_").nth(1).unwrap_or("").to_string();
                    let e = per_engine.entry((luid, pid, eng)).or_insert((0.0, ty));
                    e.0 += v;
                }
            }
            for ((luid, pid, _), (v, ty)) in per_engine {
                let e = out.entry((luid, pid)).or_default();
                let v = v.clamp(0.0, 100.0) as f32;
                e.util = Some(e.util.unwrap_or(0.0).max(v));
                if ty.starts_with("VideoEncode") {
                    e.enc = Some(e.enc.unwrap_or(0.0).max(v));
                } else if ty.starts_with("VideoDecode") {
                    e.dec = Some(e.dec.unwrap_or(0.0).max(v));
                }
            }
        }
        self.primed = true;
        out
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe { (self.close)(self.query) };
    }
}

/// "pid_1234_luid_0x00000000_0x0000D1A5_phys_0[_eng_3_engtype_3D]"
fn parse_instance(name: &str) -> Option<(u32, Luid, &str)> {
    let rest = name.strip_prefix("pid_")?;
    let (pid, rest) = rest.split_once('_')?;
    let rest = rest.strip_prefix("luid_0x")?;
    let (hi, rest) = rest.split_once("_0x")?;
    let (lo, rest) = rest.split_once('_').unwrap_or((rest, ""));
    let hi = u32::from_str_radix(hi, 16).ok()? as i32;
    let lo = u32::from_str_radix(lo, 16).ok()?;
    Some((pid.parse().ok()?, (hi, lo), rest))
}

/// Enumerate WDDM adapters: LUID -> PCI location.
fn adapters() -> HashMap<Luid, (u32, u32, u32)> {
    let mut out = HashMap::new();
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
        for a in &list[..e.num as usize] {
            let mut addr = AdapterAddress::default();
            let qi = QueryAdapterInfo {
                h_adapter: a.h_adapter,
                kind: KMTQAITYPE_ADAPTERADDRESS,
                data: &mut addr as *mut _ as *mut c_void,
                size: std::mem::size_of::<AdapterAddress>() as u32,
            };
            if q(&qi) == 0 {
                out.insert((a.luid_high, a.luid_low), (addr.bus, addr.device, addr.function));
            }
            cl(&a.h_adapter);
        }
    }
    out
}

/// Parse an NVML-style PCI bus id "00000000:01:00.0" -> (bus, device, function).
pub fn parse_bus_id(s: &str) -> Option<(u32, u32, u32)> {
    let mut parts = s.rsplitn(3, ':');
    let devfn = parts.next()?;
    let bus = parts.next()?;
    let (dev, func) = devfn.split_once('.')?;
    Some((u32::from_str_radix(bus, 16).ok()?, u32::from_str_radix(dev, 16).ok()?, u32::from_str_radix(func, 16).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names() {
        let (pid, luid, rest) = parse_instance("pid_1234_luid_0x00000000_0x0000D1A5_phys_0_eng_3_engtype_3D").unwrap();
        assert_eq!(pid, 1234);
        assert_eq!(luid, (0, 0xD1A5));
        assert_eq!(rest, "phys_0_eng_3_engtype_3D");
        let (_, _, rest) = parse_instance("pid_8_luid_0x00000001_0x0000D1A5_phys_0").unwrap();
        assert_eq!(rest, "phys_0");
    }

    #[test]
    fn bus_ids() {
        assert_eq!(parse_bus_id("00000000:01:00.0"), Some((1, 0, 0)));
        assert_eq!(parse_bus_id("0000:af:1f.3"), Some((0xaf, 0x1f, 3)));
    }
}
