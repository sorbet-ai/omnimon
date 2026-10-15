//! Apple Silicon backend. No sudo, no powermetrics:
//!
//! - IOKit `IOAccelerator` -> `PerformanceStatistics`: device/renderer/tiler
//!   utilization and GPU-resident memory.
//! - `AGXDeviceUserClient` children of the accelerator: one per process with
//!   `IOUserClientCreator` ("pid 123, name") and `AppUsage[].accumulatedGPUTime`
//!   (ns), which gives per-process GPU utilization from deltas.
//! - IOReport (private, dlopen'd from the shared cache): "Energy Model" for GPU
//!   power, "GPU Stats / GPU Performance States" residency for frequency,
//!   weighted by the DVFS table in the `pmgr` node (`voltage-states9`).

#![allow(non_upper_case_globals, non_snake_case)]

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::time::Instant;

use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation_sys::base::{CFAllocatorRef, CFGetTypeID, CFRelease, CFTypeRef, kCFAllocatorDefault};
use core_foundation_sys::data::{CFDataGetBytePtr, CFDataGetLength, CFDataGetTypeID, CFDataRef};
use core_foundation_sys::dictionary::{CFDictionaryGetTypeID, CFDictionaryGetValue, CFDictionaryRef, CFMutableDictionaryRef};
use core_foundation_sys::number::{CFNumberGetTypeID, CFNumberGetValue, CFNumberRef, kCFNumberSInt64Type};
use core_foundation_sys::string::{CFStringCreateWithCString, CFStringGetCString, CFStringGetTypeID, CFStringRef, kCFStringEncodingUTF8};
use libloading::Library;

use super::Backend;
use crate::model::{BackendInfo, GpuInfo, GpuProcess, GpuSample, ProcKind, Vendor};

type IoObject = u32;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingServices(main_port: u32, matching: CFDictionaryRef, existing: *mut IoObject) -> i32;
    fn IOIteratorNext(it: IoObject) -> IoObject;
    fn IOObjectRelease(o: IoObject) -> i32;
    fn IORegistryEntryCreateCFProperties(e: IoObject, props: *mut CFMutableDictionaryRef, a: CFAllocatorRef, opts: u32) -> i32;
    fn IORegistryEntryGetChildIterator(e: IoObject, plane: *const c_char, it: *mut IoObject) -> i32;
    fn IORegistryEntryGetName(e: IoObject, name: *mut c_char) -> i32;
}

// ---------------------------------------------------------------- CF helpers

/// Owned CF object, released on drop.
struct Cf(CFTypeRef);
impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

fn cfstr(s: &str) -> Cf {
    let c = CString::new(s).unwrap();
    Cf(unsafe { CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr(), kCFStringEncodingUTF8) } as CFTypeRef)
}

fn get(dict: CFTypeRef, key: &str) -> Option<CFTypeRef> {
    if dict.is_null() || unsafe { CFGetTypeID(dict) } != unsafe { CFDictionaryGetTypeID() } {
        return None;
    }
    let k = cfstr(key);
    let v = unsafe { CFDictionaryGetValue(dict as CFDictionaryRef, k.0) };
    (!v.is_null()).then_some(v)
}

fn as_i64(v: CFTypeRef) -> Option<i64> {
    if unsafe { CFGetTypeID(v) } != unsafe { CFNumberGetTypeID() } {
        return None;
    }
    let mut out: i64 = 0;
    unsafe { CFNumberGetValue(v as CFNumberRef, kCFNumberSInt64Type, &mut out as *mut i64 as *mut c_void) }.then_some(out)
}

fn as_string(v: CFTypeRef) -> Option<String> {
    if v.is_null() || unsafe { CFGetTypeID(v) } != unsafe { CFStringGetTypeID() } {
        return None;
    }
    let mut buf = [0 as c_char; 512];
    unsafe { CFStringGetCString(v as CFStringRef, buf.as_mut_ptr(), buf.len() as _, kCFStringEncodingUTF8) }
        .ne(&0)
        .then(|| unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
}

fn as_array(v: CFTypeRef) -> Vec<CFTypeRef> {
    if v.is_null() || unsafe { CFGetTypeID(v) } != unsafe { CFArrayGetTypeID() } {
        return Vec::new();
    }
    let n = unsafe { CFArrayGetCount(v as CFArrayRef) };
    (0..n).map(|i| unsafe { CFArrayGetValueAtIndex(v as CFArrayRef, i) }).collect()
}

fn as_bytes(v: CFTypeRef) -> Option<Vec<u8>> {
    if unsafe { CFGetTypeID(v) } != unsafe { CFDataGetTypeID() } {
        return None;
    }
    let len = unsafe { CFDataGetLength(v as CFDataRef) } as usize;
    let p = unsafe { CFDataGetBytePtr(v as CFDataRef) };
    Some(unsafe { std::slice::from_raw_parts(p, len) }.to_vec())
}

fn properties(e: IoObject) -> Option<Cf> {
    let mut props: CFMutableDictionaryRef = std::ptr::null_mut();
    let r = unsafe { IORegistryEntryCreateCFProperties(e, &mut props, kCFAllocatorDefault, 0) };
    (r == 0 && !props.is_null()).then(|| Cf(props as CFTypeRef))
}

fn services(class: &str) -> Vec<IoObject> {
    let c = CString::new(class).unwrap();
    let mut it: IoObject = 0;
    // IOServiceGetMatchingServices consumes the matching dictionary.
    if unsafe { IOServiceGetMatchingServices(0, IOServiceMatching(c.as_ptr()) as CFDictionaryRef, &mut it) } != 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    loop {
        let s = unsafe { IOIteratorNext(it) };
        if s == 0 {
            break;
        }
        out.push(s);
    }
    unsafe { IOObjectRelease(it) };
    out
}

fn children(e: IoObject) -> Vec<IoObject> {
    let mut it: IoObject = 0;
    if unsafe { IORegistryEntryGetChildIterator(e, c"IOService".as_ptr(), &mut it) } != 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    loop {
        let s = unsafe { IOIteratorNext(it) };
        if s == 0 {
            break;
        }
        out.push(s);
    }
    unsafe { IOObjectRelease(it) };
    out
}

fn entry_name(e: IoObject) -> String {
    let mut buf = [0 as c_char; 128];
    unsafe { IORegistryEntryGetName(e, buf.as_mut_ptr()) };
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

fn sysctl_string(name: &str) -> Option<String> {
    let out = std::process::Command::new("sysctl").args(["-n", name]).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------- IOReport

type CopyChannels = unsafe extern "C" fn(CFStringRef, CFStringRef, u64, u64, u64) -> CFMutableDictionaryRef;
type MergeChannels = unsafe extern "C" fn(CFMutableDictionaryRef, CFMutableDictionaryRef, CFTypeRef);
type CreateSub = unsafe extern "C" fn(*const c_void, CFMutableDictionaryRef, *mut CFMutableDictionaryRef, u64, CFTypeRef) -> *const c_void;
type CreateSamples = unsafe extern "C" fn(*const c_void, CFMutableDictionaryRef, CFTypeRef) -> CFDictionaryRef;
type SamplesDelta = unsafe extern "C" fn(CFDictionaryRef, CFDictionaryRef, CFTypeRef) -> CFDictionaryRef;
type ChanStr = unsafe extern "C" fn(CFDictionaryRef) -> CFStringRef;
type SimpleInt = unsafe extern "C" fn(CFDictionaryRef, i32) -> i64;
type StateCount = unsafe extern "C" fn(CFDictionaryRef) -> i32;
type StateName = unsafe extern "C" fn(CFDictionaryRef, i32) -> CFStringRef;
type StateRes = unsafe extern "C" fn(CFDictionaryRef, i32) -> i64;

struct IoReport {
    _lib: Library,
    create_samples: CreateSamples,
    delta: SamplesDelta,
    group: ChanStr,
    subgroup: ChanStr,
    name: ChanStr,
    unit: ChanStr,
    int_value: SimpleInt,
    state_count: StateCount,
    state_name: StateName,
    state_res: StateRes,
    sub: *const c_void,
    subbed: CFMutableDictionaryRef,
    prev: Option<(Cf, Instant)>,
}

struct Reading {
    gpu_watts: Option<f64>,
    /// (state name, residency) for the GPU performance-state channel.
    gpu_states: Vec<(String, i64)>,
}

impl IoReport {
    fn new() -> Option<Self> {
        unsafe {
            let lib = Library::new("/usr/lib/libIOReport.dylib").ok()?;
            let copy: CopyChannels = *lib.get(b"IOReportCopyChannelsInGroup\0").ok()?;
            let merge: MergeChannels = *lib.get(b"IOReportMergeChannels\0").ok()?;
            let create_sub: CreateSub = *lib.get(b"IOReportCreateSubscription\0").ok()?;
            let r = IoReport {
                create_samples: *lib.get(b"IOReportCreateSamples\0").ok()?,
                delta: *lib.get(b"IOReportCreateSamplesDelta\0").ok()?,
                group: *lib.get(b"IOReportChannelGetGroup\0").ok()?,
                subgroup: *lib.get(b"IOReportChannelGetSubGroup\0").ok()?,
                name: *lib.get(b"IOReportChannelGetChannelName\0").ok()?,
                unit: *lib.get(b"IOReportChannelGetUnitLabel\0").ok()?,
                int_value: *lib.get(b"IOReportSimpleGetIntegerValue\0").ok()?,
                state_count: *lib.get(b"IOReportStateGetCount\0").ok()?,
                state_name: *lib.get(b"IOReportStateGetNameForIndex\0").ok()?,
                state_res: *lib.get(b"IOReportStateGetResidency\0").ok()?,
                sub: std::ptr::null(),
                subbed: std::ptr::null_mut(),
                prev: None,
                _lib: lib,
            };
            let energy = copy(cfstr("Energy Model").0 as CFStringRef, std::ptr::null(), 0, 0, 0);
            let gpu = copy(cfstr("GPU Stats").0 as CFStringRef, cfstr("GPU Performance States").0 as CFStringRef, 0, 0, 0);
            if energy.is_null() {
                return None;
            }
            if !gpu.is_null() {
                merge(energy, gpu, std::ptr::null());
                CFRelease(gpu as CFTypeRef);
            }
            let mut subbed: CFMutableDictionaryRef = std::ptr::null_mut();
            let sub = create_sub(std::ptr::null(), energy, &mut subbed, 0, std::ptr::null());
            CFRelease(energy as CFTypeRef);
            if sub.is_null() || subbed.is_null() {
                return None;
            }
            Some(IoReport { sub, subbed, ..r })
        }
    }

    fn read(&mut self) -> Option<Reading> {
        let cur = unsafe { (self.create_samples)(self.sub, self.subbed, std::ptr::null()) };
        if cur.is_null() {
            return None;
        }
        let cur = Cf(cur as CFTypeRef);
        let now = Instant::now();
        let Some((prev, t0)) = self.prev.replace((cur, now)) else { return None };
        let cur_ref = self.prev.as_ref().unwrap().0.0;
        let dt = now.duration_since(t0).as_secs_f64();
        let delta = Cf(unsafe { (self.delta)(prev.0 as CFDictionaryRef, cur_ref as CFDictionaryRef, std::ptr::null()) } as CFTypeRef);
        if delta.0.is_null() || dt <= 0.0 {
            return None;
        }
        let mut r = Reading { gpu_watts: None, gpu_states: Vec::new() };
        let chans = get(delta.0, "IOReportChannels").map(as_array).unwrap_or_default();
        for ch in chans {
            let ch = ch as CFDictionaryRef;
            let group = as_string(unsafe { (self.group)(ch) } as CFTypeRef).unwrap_or_default();
            let name = as_string(unsafe { (self.name)(ch) } as CFTypeRef).unwrap_or_default();
            if group == "Energy Model" && (name == "GPU Energy" || name == "GPU") {
                let unit = as_string(unsafe { (self.unit)(ch) } as CFTypeRef).unwrap_or_default();
                let v = unsafe { (self.int_value)(ch, 0) } as f64;
                let joules = match unit.trim() {
                    "mJ" => v / 1e3,
                    "uJ" | "µJ" => v / 1e6,
                    "nJ" => v / 1e9,
                    _ => v / 1e3,
                };
                *r.gpu_watts.get_or_insert(0.0) += joules / dt;
            } else if group == "GPU Stats" && name == "GPUPH" {
                let sub = as_string(unsafe { (self.subgroup)(ch) } as CFTypeRef).unwrap_or_default();
                if sub == "GPU Performance States" {
                    let n = unsafe { (self.state_count)(ch) };
                    for i in 0..n {
                        let nm = as_string(unsafe { (self.state_name)(ch, i) } as CFTypeRef).unwrap_or_default();
                        r.gpu_states.push((nm, unsafe { (self.state_res)(ch, i) }));
                    }
                }
            }
        }
        Some(r)
    }
}

// ---------------------------------------------------------------- backend

pub struct Apple {
    accel: IoObject,
    info: GpuInfo,
    report: Option<IoReport>,
    /// GPU DVFS frequencies in MHz, lowest first (excluding the off state).
    freqs: Vec<u32>,
    /// pid -> (accumulated GPU ns, when)
    prev_gpu_time: HashMap<u32, (u64, Instant)>,
    os_version: Option<String>,
}

// SAFETY: IOKit/CF handles are only touched from the sampler thread.
unsafe impl Send for Apple {}

fn gpu_freq_table() -> Vec<u32> {
    for e in services("AppleARMIODevice") {
        let is_pmgr = entry_name(e) == "pmgr";
        if is_pmgr {
            if let Some(props) = properties(e) {
                if let Some(bytes) = get(props.0, "voltage-states9").and_then(as_bytes) {
                    let mut f: Vec<u32> = bytes
                        .chunks_exact(8)
                        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .filter(|&v| v > 0)
                        // Hz on recent chips, kHz on some older ones.
                        .map(|v| if v > 10_000_000 { v / 1_000_000 } else { v / 1_000 })
                        .collect();
                    f.dedup();
                    unsafe { IOObjectRelease(e) };
                    return f;
                }
            }
        }
        unsafe { IOObjectRelease(e) };
    }
    Vec::new()
}

impl Apple {
    pub fn new() -> Result<Self, String> {
        let mut accels = services("IOAccelerator");
        if accels.is_empty() {
            return Err("no IOAccelerator found".into());
        }
        let accel = accels.remove(0);
        for a in accels {
            unsafe { IOObjectRelease(a) };
        }
        let props = properties(accel).ok_or("cannot read IOAccelerator properties")?;
        let model = get(props.0, "model").and_then(as_string).unwrap_or_else(|| "Apple GPU".into());
        let cores = get(props.0, "gpu-core-count").and_then(as_i64).map(|v| v as u32);
        let mut info = GpuInfo::new(0, Vendor::Apple, model);
        info.cores = cores;
        info.integrated = true;
        info.architecture = sysctl_string("machdep.cpu.brand_string");
        info.memory_total = sysctl_string("hw.memsize").and_then(|s| s.parse().ok());
        let freqs = gpu_freq_table();
        info.max_clock_graphics = freqs.last().copied();
        Ok(Apple {
            accel,
            info,
            report: IoReport::new(),
            freqs,
            prev_gpu_time: HashMap::new(),
            os_version: sysctl_string("kern.osproductversion"),
        })
    }

    fn processes(&mut self) -> Vec<GpuProcess> {
        let now = Instant::now();
        let mut per_pid: HashMap<u32, (u64, String)> = HashMap::new();
        for c in children(self.accel) {
            if let Some(props) = properties(c) {
                let creator = get(props.0, "IOUserClientCreator").and_then(as_string).unwrap_or_default();
                // "pid 123, name"
                if let Some((pid, name)) = creator.strip_prefix("pid ").and_then(|r| r.split_once(", ")) {
                    if let Ok(pid) = pid.parse::<u32>() {
                        let ns: u64 = get(props.0, "AppUsage")
                            .map(as_array)
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|u| get(u, "accumulatedGPUTime").and_then(as_i64))
                            .map(|v| v.max(0) as u64)
                            .sum();
                        let e = per_pid.entry(pid).or_insert((0, name.to_string()));
                        e.0 += ns;
                    }
                }
            }
            unsafe { IOObjectRelease(c) };
        }
        let mut out = Vec::new();
        let mut next = HashMap::new();
        for (pid, (ns, _name)) in per_pid {
            let util = self.prev_gpu_time.get(&pid).map(|(prev, t)| {
                let dt = now.duration_since(*t).as_nanos() as f64;
                if dt > 0.0 { ((ns.saturating_sub(*prev)) as f64 / dt * 100.0).clamp(0.0, 100.0) as f32 } else { 0.0 }
            });
            next.insert(pid, (ns, now));
            // Clients that never submitted GPU work are just Metal device handles.
            if ns == 0 {
                continue;
            }
            out.push(GpuProcess {
                pid,
                kind: ProcKind::Graphics,
                gpu_mem: None,
                sm_util: util,
                mem_util: None,
                enc_util: None,
                dec_util: None,
                gpu_instance: None,
                name: None,
            });
        }
        self.prev_gpu_time = next;
        out.sort_by_key(|p| p.pid);
        out
    }
}

impl Drop for Apple {
    fn drop(&mut self) {
        unsafe { IOObjectRelease(self.accel) };
    }
}

impl Backend for Apple {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            name: "IOKit".into(),
            library: Some(if self.report.is_some() { "IOKit + IOReport".into() } else { "IOKit".into() }),
            driver_version: self.os_version.as_ref().map(|v| format!("macOS {v}")),
            api_version: None,
            runtime_version: None,
        }
    }

    fn gpus(&self) -> Vec<GpuInfo> {
        vec![self.info.clone()]
    }

    fn sample(&mut self) -> Vec<GpuSample> {
        let mut s = GpuSample::default();
        if let Some(props) = properties(self.accel) {
            if let Some(ps) = get(props.0, "PerformanceStatistics") {
                s.util_gpu = get(ps, "Device Utilization %").and_then(as_i64).map(|v| v.clamp(0, 100) as f32);
                s.mem_used = get(ps, "In use system memory").and_then(as_i64).map(|v| v.max(0) as u64);
            }
        }
        s.mem_total = self.info.memory_total;
        if let Some(r) = self.report.as_mut().and_then(|r| r.read()) {
            s.power = r.gpu_watts;
            // Residency-weighted frequency over active states.
            let active: Vec<(usize, i64)> = r
                .gpu_states
                .iter()
                .filter(|(n, _)| n != "OFF" && n != "IDLE" && n != "DOWN")
                .enumerate()
                .map(|(i, (_, res))| (i, *res))
                .collect();
            let total: i64 = active.iter().map(|(_, r)| r).sum();
            if total > 0 && !self.freqs.is_empty() {
                let mhz: f64 = active
                    .iter()
                    .map(|(i, res)| *self.freqs.get(*i).unwrap_or(self.freqs.last().unwrap()) as f64 * *res as f64)
                    .sum::<f64>()
                    / total as f64;
                s.clock_graphics = Some(mhz.round() as u32);
            } else if !r.gpu_states.is_empty() {
                s.clock_graphics = Some(0);
            }
        }
        s.processes = self.processes();
        vec![s]
    }
}
