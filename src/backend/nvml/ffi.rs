//! Minimal hand-rolled NVML bindings, loaded at runtime.
//!
//! We never link against NVML: the library is `dlopen`ed from a list of known
//! locations (containers often only ship `libnvidia-ml.so.1`, WSL keeps it in
//! `/usr/lib/wsl/lib`, Windows in System32 or NVSMI). Every symbol is optional,
//! so old drivers missing a newer entry point just lose that metric.

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{CStr, c_char, c_int, c_uint, c_ulonglong, c_void};

use libloading::Library;

pub type nvmlReturn_t = c_uint;
pub type nvmlDevice_t = *mut c_void;

pub const NVML_SUCCESS: nvmlReturn_t = 0;
pub const NVML_ERROR_UNINITIALIZED: nvmlReturn_t = 1;
pub const NVML_ERROR_INVALID_ARGUMENT: nvmlReturn_t = 2;
pub const NVML_ERROR_NOT_SUPPORTED: nvmlReturn_t = 3;
pub const NVML_ERROR_NO_PERMISSION: nvmlReturn_t = 4;
pub const NVML_ERROR_NOT_FOUND: nvmlReturn_t = 6;
pub const NVML_ERROR_INSUFFICIENT_SIZE: nvmlReturn_t = 7;
pub const NVML_ERROR_DRIVER_NOT_LOADED: nvmlReturn_t = 9;
pub const NVML_ERROR_FUNCTION_NOT_FOUND: nvmlReturn_t = 13;
pub const NVML_ERROR_GPU_IS_LOST: nvmlReturn_t = 15;

pub const NVML_VALUE_NOT_AVAILABLE: c_ulonglong = c_ulonglong::MAX;

pub const NVML_CLOCK_GRAPHICS: c_uint = 0;
pub const NVML_CLOCK_SM: c_uint = 1;
pub const NVML_CLOCK_MEM: c_uint = 2;
pub const NVML_CLOCK_VIDEO: c_uint = 3;

pub const NVML_TEMPERATURE_GPU: c_uint = 0;
pub const NVML_TEMPERATURE_THRESHOLD_SHUTDOWN: c_uint = 0;
pub const NVML_TEMPERATURE_THRESHOLD_SLOWDOWN: c_uint = 1;
pub const NVML_TEMPERATURE_THRESHOLD_GPU_MAX: c_uint = 3;

pub const NVML_PCIE_UTIL_TX_BYTES: c_uint = 0;
pub const NVML_PCIE_UTIL_RX_BYTES: c_uint = 1;

pub const NVML_MEMORY_ERROR_TYPE_CORRECTED: c_uint = 0;
pub const NVML_MEMORY_ERROR_TYPE_UNCORRECTED: c_uint = 1;
pub const NVML_VOLATILE_ECC: c_uint = 0;

pub const NVML_FI_DEV_MEMORY_TEMP: c_uint = 82;
/// Common NVLink speed of the device's links, MB/s.
pub const NVML_FI_DEV_NVLINK_SPEED_MBPS_COMMON: c_uint = 90;
/// Per-link (scope = link) payload counters, KiB.
pub const NVML_FI_DEV_NVLINK_THROUGHPUT_DATA_TX: c_uint = 138;
pub const NVML_FI_DEV_NVLINK_THROUGHPUT_DATA_RX: c_uint = 139;
pub const NVML_VALUE_TYPE_DOUBLE: c_uint = 0;

pub const NVML_NVLINK_MAX_LINKS: c_uint = 36;
pub const NVML_NVLINK_ERROR_DL_REPLAY: c_uint = 0;
pub const NVML_NVLINK_ERROR_DL_RECOVERY: c_uint = 1;
pub const NVML_NVLINK_ERROR_DL_CRC_FLIT: c_uint = 2;
pub const NVML_NVLINK_DEVICE_TYPE_SWITCH: c_uint = 2;

#[repr(C)]
#[derive(Default)]
pub struct nvmlDeviceAttributes_t {
    pub multiprocessor_count: c_uint,
    pub shared_copy_engine_count: c_uint,
    pub shared_decoder_count: c_uint,
    pub shared_encoder_count: c_uint,
    pub shared_jpeg_count: c_uint,
    pub shared_ofa_count: c_uint,
    pub gpu_instance_slice_count: c_uint,
    pub compute_instance_slice_count: c_uint,
    pub memory_size_mb: c_ulonglong,
}

#[repr(C)]
#[derive(Default)]
pub struct nvmlUtilization_t {
    pub gpu: c_uint,
    pub memory: c_uint,
}

#[repr(C)]
#[derive(Default)]
pub struct nvmlMemory_t {
    pub total: c_ulonglong,
    pub free: c_ulonglong,
    pub used: c_ulonglong,
}

#[repr(C)]
#[derive(Default)]
pub struct nvmlMemory_v2_t {
    pub version: c_uint,
    pub total: c_ulonglong,
    pub reserved: c_ulonglong,
    pub free: c_ulonglong,
    pub used: c_ulonglong,
}
pub const NVML_MEMORY_V2: c_uint = std::mem::size_of::<nvmlMemory_v2_t>() as c_uint | (2 << 24);

#[repr(C)]
#[derive(Default)]
pub struct nvmlBAR1Memory_t {
    pub total: c_ulonglong,
    pub free: c_ulonglong,
    pub used: c_ulonglong,
}

#[repr(C)]
pub struct nvmlPciInfo_t {
    pub bus_id_legacy: [c_char; 16],
    pub domain: c_uint,
    pub bus: c_uint,
    pub device: c_uint,
    pub pci_device_id: c_uint,
    pub pci_subsystem_id: c_uint,
    pub bus_id: [c_char; 32],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct nvmlProcessUtilizationSample_t {
    pub pid: c_uint,
    pub time_stamp: c_ulonglong,
    pub sm_util: c_uint,
    pub mem_util: c_uint,
    pub enc_util: c_uint,
    pub dec_util: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct nvmlFieldValue_t {
    pub field_id: c_uint,
    pub scope_id: c_uint,
    pub timestamp: i64,
    pub latency_usec: i64,
    pub value_type: c_uint,
    pub nvml_return: nvmlReturn_t,
    pub value: u64,
}

/// GPM (GPU Performance Monitoring, Hopper+): opaque sample buffers
/// compared pairwise.
pub type nvmlGpmSample_t = *mut c_void;
pub const NVML_GPM_METRICS_GET_VERSION: c_uint = 1;
pub const NVML_GPM_SUPPORT_VERSION: c_uint = 1;

pub const NVML_GPM_METRIC_SM_UTIL: c_uint = 2;
pub const NVML_GPM_METRIC_SM_OCCUPANCY: c_uint = 3;
pub const NVML_GPM_METRIC_ANY_TENSOR_UTIL: c_uint = 5;
pub const NVML_GPM_METRIC_DFMA_TENSOR_UTIL: c_uint = 6;
pub const NVML_GPM_METRIC_HMMA_TENSOR_UTIL: c_uint = 7;
pub const NVML_GPM_METRIC_IMMA_TENSOR_UTIL: c_uint = 9;
pub const NVML_GPM_METRIC_DRAM_BW_UTIL: c_uint = 10;
pub const NVML_GPM_METRIC_FP64_UTIL: c_uint = 11;
pub const NVML_GPM_METRIC_FP32_UTIL: c_uint = 12;
pub const NVML_GPM_METRIC_FP16_UTIL: c_uint = 13;
/// MiB/s.
pub const NVML_GPM_METRIC_NVLINK_TOTAL_RX_PER_SEC: c_uint = 60;
pub const NVML_GPM_METRIC_NVLINK_TOTAL_TX_PER_SEC: c_uint = 61;

#[repr(C)]
pub struct nvmlGpmSupport_t {
    pub version: c_uint,
    pub is_supported_device: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct nvmlGpmMetric_t {
    pub metric_id: c_uint,
    pub nvml_return: nvmlReturn_t,
    pub value: f64,
    pub short_name: *const c_char,
    pub long_name: *const c_char,
    pub unit: *const c_char,
}

/// The C struct ends in `metrics[NVML_GPM_METRIC_MAX]` (98, now 477);
/// NVML only touches the first `num_metrics`, and we leave room for the
/// largest anyway.
pub const GPM_METRICS_CAP: usize = 512;

#[repr(C)]
pub struct nvmlGpmMetricsGet_t {
    pub version: c_uint,
    pub num_metrics: c_uint,
    pub sample1: nvmlGpmSample_t,
    pub sample2: nvmlGpmSample_t,
    pub metrics: [nvmlGpmMetric_t; GPM_METRICS_CAP],
}

type F<A> = Option<A>;
type DevU = unsafe extern "C" fn(nvmlDevice_t, *mut c_uint) -> nvmlReturn_t;
type DevUU = unsafe extern "C" fn(nvmlDevice_t, c_uint, *mut c_uint) -> nvmlReturn_t;
type DevStr = unsafe extern "C" fn(nvmlDevice_t, *mut c_char, c_uint) -> nvmlReturn_t;
type DevUtilPeriod = unsafe extern "C" fn(nvmlDevice_t, *mut c_uint, *mut c_uint) -> nvmlReturn_t;
type DevProcs = unsafe extern "C" fn(nvmlDevice_t, *mut c_uint, *mut c_void) -> nvmlReturn_t;
type SysStr = unsafe extern "C" fn(*mut c_char, c_uint) -> nvmlReturn_t;

macro_rules! nvml_api {
    ($( $field:ident : $ty:ty = $sym:literal ;)*) => {
        pub struct Api {
            _lib: Library,
            pub path: String,
            $( pub $field: F<$ty>, )*
        }
        impl Api {
            /// # Safety
            /// `lib` must be an NVML library.
            unsafe fn from_lib(lib: Library, path: String) -> Self {
                $(
                    let $field = unsafe {
                        lib.get::<$ty>(concat!($sym, "\0").as_bytes()).ok().map(|s| *s)
                    };
                )*
                Api { _lib: lib, path, $( $field, )* }
            }
        }
    };
}

nvml_api! {
    init_v2: unsafe extern "C" fn() -> nvmlReturn_t = "nvmlInit_v2";
    init_v1: unsafe extern "C" fn() -> nvmlReturn_t = "nvmlInit";
    shutdown: unsafe extern "C" fn() -> nvmlReturn_t = "nvmlShutdown";
    error_string: unsafe extern "C" fn(nvmlReturn_t) -> *const c_char = "nvmlErrorString";

    sys_driver_version: SysStr = "nvmlSystemGetDriverVersion";
    sys_nvml_version: SysStr = "nvmlSystemGetNVMLVersion";
    sys_cuda_version_v2: unsafe extern "C" fn(*mut c_int) -> nvmlReturn_t = "nvmlSystemGetCudaDriverVersion_v2";
    sys_cuda_version: unsafe extern "C" fn(*mut c_int) -> nvmlReturn_t = "nvmlSystemGetCudaDriverVersion";

    count_v2: unsafe extern "C" fn(*mut c_uint) -> nvmlReturn_t = "nvmlDeviceGetCount_v2";
    count_v1: unsafe extern "C" fn(*mut c_uint) -> nvmlReturn_t = "nvmlDeviceGetCount";
    handle_by_index_v2: unsafe extern "C" fn(c_uint, *mut nvmlDevice_t) -> nvmlReturn_t = "nvmlDeviceGetHandleByIndex_v2";
    handle_by_index_v1: unsafe extern "C" fn(c_uint, *mut nvmlDevice_t) -> nvmlReturn_t = "nvmlDeviceGetHandleByIndex";

    name: DevStr = "nvmlDeviceGetName";
    uuid: DevStr = "nvmlDeviceGetUUID";
    serial: DevStr = "nvmlDeviceGetSerial";
    vbios: DevStr = "nvmlDeviceGetVbiosVersion";
    pci_info_v3: unsafe extern "C" fn(nvmlDevice_t, *mut nvmlPciInfo_t) -> nvmlReturn_t = "nvmlDeviceGetPciInfo_v3";
    pci_info_v2: unsafe extern "C" fn(nvmlDevice_t, *mut nvmlPciInfo_t) -> nvmlReturn_t = "nvmlDeviceGetPciInfo_v2";
    minor_number: DevU = "nvmlDeviceGetMinorNumber";
    cuda_cc: unsafe extern "C" fn(nvmlDevice_t, *mut c_int, *mut c_int) -> nvmlReturn_t = "nvmlDeviceGetCudaComputeCapability";
    architecture: DevU = "nvmlDeviceGetArchitecture";
    num_cores: DevU = "nvmlDeviceGetNumGpuCores";
    persistence_mode: DevU = "nvmlDeviceGetPersistenceMode";
    display_active: DevU = "nvmlDeviceGetDisplayActive";
    compute_mode: DevU = "nvmlDeviceGetComputeMode";
    mig_mode: unsafe extern "C" fn(nvmlDevice_t, *mut c_uint, *mut c_uint) -> nvmlReturn_t = "nvmlDeviceGetMigMode";
    num_fans: DevU = "nvmlDeviceGetNumFans";
    memory_bus_width: DevU = "nvmlDeviceGetMemoryBusWidth";

    memory_v1: unsafe extern "C" fn(nvmlDevice_t, *mut nvmlMemory_t) -> nvmlReturn_t = "nvmlDeviceGetMemoryInfo";
    memory_v2: unsafe extern "C" fn(nvmlDevice_t, *mut nvmlMemory_v2_t) -> nvmlReturn_t = "nvmlDeviceGetMemoryInfo_v2";
    bar1: unsafe extern "C" fn(nvmlDevice_t, *mut nvmlBAR1Memory_t) -> nvmlReturn_t = "nvmlDeviceGetBAR1MemoryInfo";
    utilization: unsafe extern "C" fn(nvmlDevice_t, *mut nvmlUtilization_t) -> nvmlReturn_t = "nvmlDeviceGetUtilizationRates";
    temperature: DevUU = "nvmlDeviceGetTemperature";
    temperature_threshold: DevUU = "nvmlDeviceGetTemperatureThreshold";
    fan_speed: DevU = "nvmlDeviceGetFanSpeed";
    fan_speed_v2: DevUU = "nvmlDeviceGetFanSpeed_v2";
    power_usage: DevU = "nvmlDeviceGetPowerUsage";
    enforced_power_limit: DevU = "nvmlDeviceGetEnforcedPowerLimit";
    power_limit_constraints: unsafe extern "C" fn(nvmlDevice_t, *mut c_uint, *mut c_uint) -> nvmlReturn_t = "nvmlDeviceGetPowerManagementLimitConstraints";
    power_default_limit: DevU = "nvmlDeviceGetPowerManagementDefaultLimit";
    set_power_limit: unsafe extern "C" fn(nvmlDevice_t, c_uint) -> nvmlReturn_t = "nvmlDeviceSetPowerManagementLimit";
    total_energy: unsafe extern "C" fn(nvmlDevice_t, *mut c_ulonglong) -> nvmlReturn_t = "nvmlDeviceGetTotalEnergyConsumption";
    clock_info: DevUU = "nvmlDeviceGetClockInfo";
    max_clock_info: DevUU = "nvmlDeviceGetMaxClockInfo";
    performance_state: DevU = "nvmlDeviceGetPerformanceState";
    pcie_gen: DevU = "nvmlDeviceGetCurrPcieLinkGeneration";
    pcie_width: DevU = "nvmlDeviceGetCurrPcieLinkWidth";
    pcie_max_gen: DevU = "nvmlDeviceGetMaxPcieLinkGeneration";
    pcie_max_width: DevU = "nvmlDeviceGetMaxPcieLinkWidth";
    pcie_throughput: DevUU = "nvmlDeviceGetPcieThroughput";
    encoder_util: DevUtilPeriod = "nvmlDeviceGetEncoderUtilization";
    decoder_util: DevUtilPeriod = "nvmlDeviceGetDecoderUtilization";
    jpg_util: DevUtilPeriod = "nvmlDeviceGetJpgUtilization";
    ofa_util: DevUtilPeriod = "nvmlDeviceGetOfaUtilization";
    clocks_event_reasons: unsafe extern "C" fn(nvmlDevice_t, *mut c_ulonglong) -> nvmlReturn_t = "nvmlDeviceGetCurrentClocksEventReasons";
    clocks_throttle_reasons: unsafe extern "C" fn(nvmlDevice_t, *mut c_ulonglong) -> nvmlReturn_t = "nvmlDeviceGetCurrentClocksThrottleReasons";
    total_ecc_errors: unsafe extern "C" fn(nvmlDevice_t, c_uint, c_uint, *mut c_ulonglong) -> nvmlReturn_t = "nvmlDeviceGetTotalEccErrors";
    field_values: unsafe extern "C" fn(nvmlDevice_t, c_int, *mut nvmlFieldValue_t) -> nvmlReturn_t = "nvmlDeviceGetFieldValues";

    compute_procs_v3: DevProcs = "nvmlDeviceGetComputeRunningProcesses_v3";
    compute_procs_v2: DevProcs = "nvmlDeviceGetComputeRunningProcesses_v2";
    compute_procs_v1: DevProcs = "nvmlDeviceGetComputeRunningProcesses";
    graphics_procs_v3: DevProcs = "nvmlDeviceGetGraphicsRunningProcesses_v3";
    graphics_procs_v2: DevProcs = "nvmlDeviceGetGraphicsRunningProcesses_v2";
    graphics_procs_v1: DevProcs = "nvmlDeviceGetGraphicsRunningProcesses";
    mps_procs_v3: DevProcs = "nvmlDeviceGetMPSComputeRunningProcesses_v3";
    mps_procs_v2: DevProcs = "nvmlDeviceGetMPSComputeRunningProcesses_v2";
    // Presence-only probes used to identify the r530 nvmlProcessInfo_t layout.
    conf_compute_mem_size_info: unsafe extern "C" fn() -> nvmlReturn_t = "nvmlDeviceGetConfComputeMemSizeInfo";
    running_process_detail_list: unsafe extern "C" fn() -> nvmlReturn_t = "nvmlDeviceGetRunningProcessDetailList";
    process_utilization: unsafe extern "C" fn(nvmlDevice_t, *mut nvmlProcessUtilizationSample_t, *mut c_uint, c_ulonglong) -> nvmlReturn_t = "nvmlDeviceGetProcessUtilization";

    gpm_query_support: unsafe extern "C" fn(nvmlDevice_t, *mut nvmlGpmSupport_t) -> nvmlReturn_t = "nvmlGpmQueryDeviceSupport";
    gpm_sample_alloc: unsafe extern "C" fn(*mut nvmlGpmSample_t) -> nvmlReturn_t = "nvmlGpmSampleAlloc";
    gpm_sample_free: unsafe extern "C" fn(nvmlGpmSample_t) -> nvmlReturn_t = "nvmlGpmSampleFree";
    gpm_sample_get: unsafe extern "C" fn(nvmlDevice_t, nvmlGpmSample_t) -> nvmlReturn_t = "nvmlGpmSampleGet";
    gpm_metrics_get: unsafe extern "C" fn(*mut nvmlGpmMetricsGet_t) -> nvmlReturn_t = "nvmlGpmMetricsGet";
    gpm_mig_sample_get: unsafe extern "C" fn(nvmlDevice_t, c_uint, nvmlGpmSample_t) -> nvmlReturn_t = "nvmlGpmMigSampleGet";

    nvlink_state: DevUU = "nvmlDeviceGetNvLinkState";
    nvlink_version: DevUU = "nvmlDeviceGetNvLinkVersion";
    nvlink_error_counter: unsafe extern "C" fn(nvmlDevice_t, c_uint, c_uint, *mut c_ulonglong) -> nvmlReturn_t = "nvmlDeviceGetNvLinkErrorCounter";
    nvlink_remote_pci_v2: unsafe extern "C" fn(nvmlDevice_t, c_uint, *mut nvmlPciInfo_t) -> nvmlReturn_t = "nvmlDeviceGetNvLinkRemotePciInfo_v2";
    nvlink_remote_type: DevUU = "nvmlDeviceGetNvLinkRemoteDeviceType";

    max_mig_count: DevU = "nvmlDeviceGetMaxMigDeviceCount";
    mig_handle_by_index: unsafe extern "C" fn(nvmlDevice_t, c_uint, *mut nvmlDevice_t) -> nvmlReturn_t = "nvmlDeviceGetMigDeviceHandleByIndex";
    gpu_instance_id: DevU = "nvmlDeviceGetGpuInstanceId";
    compute_instance_id: DevU = "nvmlDeviceGetComputeInstanceId";
    attributes_v2: unsafe extern "C" fn(nvmlDevice_t, *mut nvmlDeviceAttributes_t) -> nvmlReturn_t = "nvmlDeviceGetAttributes_v2";
}

/// Places NVML is commonly found. Order matters: versioned soname first
/// (that's what the NVIDIA container toolkit mounts).
fn candidates() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(p) = crate::brand::env("NVML_PATH") {
        v.push(p);
    }
    #[cfg(windows)]
    {
        v.push("nvml.dll".into());
        let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        v.push(format!(r"{sysroot}\System32\nvml.dll"));
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        v.push(format!(r"{pf}\NVIDIA Corporation\NVSMI\nvml.dll"));
    }
    #[cfg(not(windows))]
    {
        v.push("libnvidia-ml.so.1".into());
        v.push("libnvidia-ml.so".into());
        for dir in [
            "/usr/lib/wsl/lib",
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib/aarch64-linux-gnu",
            "/usr/lib64",
            "/usr/lib",
            "/usr/local/nvidia/lib64",
            "/usr/local/nvidia/lib",
            "/home/kubernetes/bin/nvidia/lib64",
            "/run/nvidia/driver/usr/lib/x86_64-linux-gnu",
            "/run/nvidia/driver/usr/lib64",
            "/usr/local/cuda/lib64/stubs",
            "/opt/nvidia/lib64",
        ] {
            v.push(format!("{dir}/libnvidia-ml.so.1"));
            v.push(format!("{dir}/libnvidia-ml.so"));
        }
        // Also look for any versioned copy, e.g. libnvidia-ml.so.610.47.03 in
        // images where the driver libs were copied without symlinks.
        for dir in ["/usr/lib/x86_64-linux-gnu", "/usr/lib64", "/usr/lib/aarch64-linux-gnu"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if n.starts_with("libnvidia-ml.so.") && n != "libnvidia-ml.so.1" {
                        v.push(format!("{dir}/{n}"));
                    }
                }
            }
        }
    }
    v
}

impl Api {
    /// Try every candidate location; returns the API and a list of failure
    /// notes (for diagnostics) on error.
    pub fn load() -> Result<Api, String> {
        let mut errs = Vec::new();
        for path in candidates() {
            // SAFETY: loading a well-known vendor library.
            match unsafe { Library::new(&path) } {
                Ok(lib) => {
                    let api = unsafe { Api::from_lib(lib, path.clone()) };
                    if api.init_v2.is_some() || api.init_v1.is_some() {
                        return Ok(api);
                    }
                    errs.push(format!("{path}: not an NVML library"));
                }
                Err(e) => {
                    // Only record failures for absolute paths / overrides; bare
                    // sonames failing is the normal "no driver" case.
                    if path.contains('/') || path.contains('\\') {
                        let s = e.to_string();
                        if !s.contains("No such file") && !s.contains("cannot open") && !s.contains("os error 126") {
                            errs.push(format!("{path}: {s}"));
                        }
                    }
                }
            }
        }
        Err(if errs.is_empty() { "NVIDIA driver library (NVML) not found".into() } else { errs.join("; ") })
    }

    pub fn err_str(&self, code: nvmlReturn_t) -> String {
        if let Some(f) = self.error_string {
            let p = unsafe { f(code) };
            if !p.is_null() {
                return unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
            }
        }
        format!("NVML error {code}")
    }
}

pub fn cstr(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}
