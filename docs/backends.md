# Backend notes

Research notes for existing and future backends: where each metric comes
from, what needs privileges, and what's been verified. "Verified" means run on
real hardware during development.

## NVIDIA (NVML), verified on RTX 4090: Windows 11, WSL2, Docker Desktop

- Library search: `$OMNIMON_NVML_PATH`, `libnvidia-ml.so.1`, `libnvidia-ml.so`,
  `/usr/lib/wsl/lib`, distro lib dirs, `/usr/local/nvidia/lib64`
  (device plugin), `/home/kubernetes/bin/nvidia/lib64` (GKE),
  `/run/nvidia/driver/...` (GPU operator), versioned `libnvidia-ml.so.<ver>`;
  Windows: `nvml.dll`, `System32`, `NVSMI`.
- Process info layouts: v1 `{pid, mem}` 16 B; v2/v3 `{pid, mem, gi, ci}` 24 B;
  r530 v3 adds `usedGpuCcProtectedMemory` → 32 B (identified like nvitop: has
  `nvmlDeviceGetConfComputeMemSizeInfo` but not
  `nvmlDeviceGetRunningProcessDetailList`). Parsed with sanity checks and a
  fallback stride.
- `nvmlDeviceGetPcieThroughput` blocks ~20 ms per call; GPUs are sampled in
  parallel threads.
- `nvmlDeviceGetProcessUtilization` is queried with a ~1 s lookback.
- WDDM (Windows): per-process memory is `NVML_VALUE_NOT_AVAILABLE`; filled in
  from WDDM performance counters (`src/os/windows.rs`).
- WSL2: no process lists at all (driver limitation; nvidia-smi shows none).

### GPM (Hopper and newer), implemented, untested on hardware
- `nvmlGpmQueryDeviceSupport` per device; two buffers from `nvmlGpmSampleAlloc`,
  one `nvmlGpmSampleGet` per tick, `nvmlGpmMetricsGet` between consecutive
  ticks: SM active (2), occupancy (3), tensor any/DMMA/HMMA/IMMA (5/6/7/9),
  DRAM bandwidth % of peak (10), FP64/32/16 pipes (11-13), NVLink total rx/tx
  MiB/s (60/61). Shown as `Activity`; Ada and older GeForce answer "not
  supported" (checked on an RTX 4090), so nothing is shown there.

### NVLink, implemented, untested on hardware (`nvml/links.rs`)
- Ports: `nvmlDeviceGetNvLinkState` for links 0..36 (whatever answers).
  Peer: `nvmlDeviceGetNvLinkRemoteDeviceType` (2 = NVSwitch), else
  `nvmlDeviceGetNvLinkRemotePciInfo_v2` → matched to our GPUs by bus id.
- Traffic: `nvmlDeviceGetFieldValues` with `NVML_FI_DEV_NVLINK_THROUGHPUT_DATA_RX`
  / `_TX` (139 / 138, KiB counters, scope = link), as rates between ticks.
  Hopper+ also has GPM totals. (The old utilization-counter API is deprecated.)
- Errors: `nvmlDeviceGetNvLinkErrorCounter` replay (0) + recovery (1) + CRC
  flit (2). Events on link down/up and each doubling of a link's error count.
- Speed: field `NVML_FI_DEV_NVLINK_SPEED_MBPS_COMMON` (90). Generation from
  the architecture (`nvmlDeviceGetNvLinkVersion` changed its numbering to an
  enum, so it is only logged).

### MIG, implemented, untested on hardware (`nvml/mig.rs`)
- `nvmlDeviceGetMigMode` (current = 1) → `nvmlDeviceGetMaxMigDeviceCount` →
  `nvmlDeviceGetMigDeviceHandleByIndex` each tick (skips `NOT_FOUND`), then per
  partition: GPU/compute instance ids, name (profile after "MIG "), UUID,
  memory, `nvmlDeviceGetAttributes_v2` (SM count), its compute processes, and
  on Hopper+ `nvmlGpmMigSampleGet` for per-partition activity. The parent GPU
  reports no utilization under MIG; the UI says so.

### Not yet implemented
- **Instant power**: `nvmlDeviceGetFieldValues` with `NVML_FI_DEV_POWER_INSTANT`
  (Ampere+ `GetPowerUsage` is a 1 s average). Verify the field id against nvml.h.
- `nvmlDeviceGetRunningProcessDetailList` (r550+, struct-versioned): avoids
  layout guessing entirely.
- Xid / ECC events via `nvmlEventSetCreate` (the event timeline exists; it
  currently sees ECC counts and errors, not Xids).

## Apple Silicon, verified on M4 Pro (macOS 26)

- `IOAccelerator` → `PerformanceStatistics`: `Device Utilization %`,
  `Renderer/Tiler Utilization %`, `In use system memory`, `Alloc system memory`;
  `model`, `gpu-core-count`.
- Per process: children `AGXDeviceUserClient` with `IOUserClientCreator`
  ("pid N, name") and `AppUsage[].accumulatedGPUTime` (ns). Δ/Δt = GPU %.
  There's no per-process memory.
- IOReport (`/usr/lib/libIOReport.dylib`, private, dlopen'd from the shared
  cache): `Energy Model` channel `GPU Energy` (unit label mJ/uJ/nJ) → watts;
  `GPU Stats / GPU Performance States` channel `GPUPH` residencies × DVFS table
  from IORegistry `pmgr` → `voltage-states9` (u32 freq, u32 voltage pairs; Hz on
  M4) → average MHz.
- Temperature: HID event system (`PrimaryUsagePage 0xff00`, `PrimaryUsage 5`)
  `PMU tdie*` sensors, max taken. No GPU-specific sensor on M4.
- Private APIs change with chips and OS releases; every field is optional.

## AMD on Linux, experimental (`src/backend/amd.rs`)

- sysfs (no root): `gpu_busy_percent`, `mem_busy_percent` (dGPU only),
  `mem_info_vram_{used,total}`, `mem_info_gtt_*` (APUs: VRAM is a small
  carve-out, real usage is GTT), `pp_dpm_{sclk,mclk}` (`*` marks the level),
  `current_link_{speed,width}`, `product_name`, `unique_id`.
- hwmon: `temp{1,2,3}_input` labelled edge/junction/mem (m°C), `power1_average`
  or `power1_input` (µW, APUs use input), `power1_cap`, `fan1_input`, `pwm1`,
  `freq1_input` (Hz, instantaneous sclk).
- `gpu_metrics` binary blob (header `u16 size, u8 format, u8 content`;
  v1.x dGPU, v2.x/v3.x APU): richer data, not parsed yet.
- Per process: DRM fdinfo (`src/backend/drm_fdinfo.rs`), dedupe by
  `(drm-pdev, drm-client-id)`, keys `drm-engine-{gfx,compute,dma,enc,dec,...}`
  ns, `drm-memory-vram` / `drm-resident-vram`. Only processes we can ptrace-read
  (own user unless root).
- AMD SMI (`libamd_smi.so`) could be dlopened for Instinct parts; the ABI
  changes between ROCm releases.

## Intel on Linux, experimental (`src/backend/intel.rs`, fake-sysfs tested)

- fdinfo: i915 `drm-engine-{render,copy,video,video-enhance}` ns (+
  `drm-engine-capacity-*`); xe `drm-cycles-*` / `drm-total-cycles-*`
  (util = Δcycles/Δtotal). Device util = sum over clients unless PMU access.
- sysfs freq: `gt_act_freq_mhz` / `gt/gt0/rps_act_freq_mhz`; xe
  `device/tile0/gt0/freq0/act_freq`. hwmon for dGPU power/temp/fans.
- PMU (`perf_event_open`, intel_gpu_top) needs CAP_PERFMON; avoid by default.
- Discrete vs integrated: local-memory size (`tile0/physical_vram_size_bytes`,
  `lmem_total_bytes`), hwmon presence, or a discrete Arc device id.

## Windows, any vendor (`src/backend/wddm.rs` + `src/os/windows.rs`)

- `\GPU Engine(*)\Utilization Percentage`, instance
  `pid_<pid>_luid_0x<hi>_0x<lo>_phys_<n>_eng_<m>_engtype_<type>`; per process =
  max over engines (Task Manager's rule).
- `\GPU Process Memory(*)\Dedicated Usage` / `Shared Usage`.
- `\GPU Adapter Memory(*)\Dedicated Usage` for device totals.
- `\GPU Process Memory(*)\Shared Usage` on a discrete GPU is spill (measured,
  not estimated); on an integrated GPU it is simply its memory.
- Adapters via `D3DKMTEnumAdapters2` + `D3DKMTQueryAdapterInfo`: address (6,
  LUID → PCI), registry info (8, name), segment sizes (3), adapter type (15:
  render/software/indirect-display/hybrid flags; WARP, virtual displays and
  compute-only NPUs are skipped, GPU-P adapters in VMs kept), device ids (31).
- Used two ways: to enrich NVML's per-process data, and as a standalone
  backend for AMD and Intel (and NVIDIA without NVML). No clocks, power or
  temperature this way; ADLX (AMD) and IGCL (Intel) would add them.
