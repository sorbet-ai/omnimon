# nvtui

A fast, good-looking terminal GPU monitor. One self-contained binary for
Linux, Windows and macOS that keeps working in the places other monitors give
up: Docker, Kubernetes, WSL2, Windows (WDDM) and Apple Silicon without sudo.

![overview](docs/img/overview.png)

## Why another one

| | nvtop | nvitop | **nvtui** |
|---|---|---|---|
| Single static-ish binary, no runtime deps | ✗ (ncurses, libdrm) | ✗ (Python) | ✓ |
| Finds NVML outside the loader path (WSL, k8s operator, GKE) | ✗ | ✗ | ✓ |
| Correct processes inside containers (host PID namespace) | blank rows | `N/A` rows | verified + translated |
| Windows native, per-process GPU memory under WDDM | ✗ | `N/A` | ✓ (WDDM counters) |
| Apple Silicon, no sudo, per-process GPU % | partial | ✗ | ✓ |
| AMD (Linux) | ✓ | ✗ | experimental |
| Prometheus / JSON exporter built in | ✗ | separate package | ✓ |

## Features

- **Overview**: a card per GPU with gradient bars (util, memory, power,
  temperature, clocks, memory bandwidth, fan, encoder/decoder) and a mirrored
  braille history (utilization up, memory down). Cards shrink to one line each
  on 8+ GPU boxes. Warning/critical borders on thermal or power-brake
  throttling and ECC errors.
- **Charts**: big braille charts per GPU for utilization, memory, power,
  temperature, SM clock and PCIe rx/tx, with avg/max.
- **Details**: everything the driver knows (UUID, PCI, VBIOS, limits,
  thresholds, clock-limit reasons, ECC, BAR1, MIG/persistence/compute mode).
- **Processes**: GPU memory, SM %, enc/dec, CPU, RAM, runtime, user, command.
  Sort (keys or click headers), filter (`/`), "this GPU only" (`g`), tag
  several (`space`) and send SIGINT/SIGTERM/SIGKILL (`K`), details with
  per-process history (`Enter`). Other users' processes are dimmed.
- **Headless**: `--once` (gpustat-style), `--json`, `--json --watch` (JSON
  lines), `--serve` (Prometheus `/metrics` + `/json`), `--diagnose`.
- Themes (`t`): neon, catppuccin, dracula, nord, matrix, mono. Truecolor
  with automatic 256-color fallback. Mouse support. Settings persist.
- `--demo N` simulates N GPUs (screenshots, UI work, machines without GPUs).

## How it gets data (and why it works where others don't)

**NVIDIA**: NVML is loaded at runtime (never linked) from a list of known
locations: the versioned `libnvidia-ml.so.1` that the NVIDIA container toolkit
mounts, `/usr/lib/wsl/lib`, Kubernetes GPU-operator and GKE paths, and
`nvml.dll` on Windows. Every symbol is optional, so old or stub drivers lose
individual metrics instead of everything. Unsupported metrics are remembered
per device and not queried again. The three historical `nvmlProcessInfo_t`
layouts (including the r530 one that garbles nvtop's process list) are
detected by symbol set and sanity-checked.

**Containers**: NVML reports *host* PIDs. Inside a container those either
don't exist or belong to a different process. nvtui checks that the process
actually holds a `/dev/nvidia*` fd before trusting a PID, translates host PIDs
via `/proc/*/sched` where the kernel leaks them, and otherwise matches the
remaining GPU processes to local processes holding the device. PIDs it can't
map are shown as `h<pid>` (outside this namespace) instead of being dropped
or mislabeled, and you can't kill them by accident.

**Windows**: under WDDM, NVML gives no per-process memory. nvtui reads the
same `GPU Process Memory` / `GPU Engine` performance counters Task Manager uses
and maps each WDDM adapter to its NVML device by PCI location. GPU work done
inside WSL2 or Docker Desktop shows up as `vmwp.exe` and is labeled as such.

**WSL2**: device metrics work. The WSL driver exposes no process list at all
(even `nvidia-smi` shows none), and the UI says so. Run the Windows build to
see processes.

**Apple Silicon**: IOKit `IOAccelerator` for utilization and GPU memory,
`AGXDeviceUserClient` entries for per-process GPU time, IOReport for GPU power
and residency-weighted frequency, and HID sensors for temperature. No sudo,
no `powermetrics`.

**AMD (Linux, experimental)**: amdgpu sysfs + hwmon, per-process usage via
DRM fdinfo. Written against the documented interfaces and tested against a fake
sysfs tree, but not yet on real hardware.

## Install / build

```sh
cargo build --release                 # native
scripts/build-linux.sh                # portable Linux x86_64 + aarch64 (glibc >= 2.17) via Docker + cargo-zigbuild
```

The Linux binary is dynamically linked against glibc only (2.17+, i.e.
any distro from the last decade). It deliberately isn't static musl: a
static binary can't `dlopen` the glibc-linked NVIDIA library. The Windows
binary links the C runtime statically.

In Docker: `docker run --gpus all ...`. NVML is mounted when
`NVIDIA_DRIVER_CAPABILITIES` includes `utility` (the default). To see
processes from other containers, add `--pid=host`.

## Usage

```
nvtui                     interactive
nvtui --once              one-shot summary
nvtui --json [--watch]    snapshot / JSON lines
nvtui --serve [ADDR]      Prometheus exporter (default 127.0.0.1:9465)
nvtui --diagnose          what was detected and why
nvtui --demo 8            8 simulated GPUs
```

Keys: `1 2 3`/`Tab` views, `←→` GPU, `↑↓`/`jk` select, `s`/`S` sort,
`r` reverse, `/` filter, `g` this GPU, `c` full command, `space` tag,
`Enter` details, `K` kill, `t` theme, `+ -` rate, `p` pause, `?` help, `q` quit.

Environment: `NVTUI_NVML_PATH` (explicit NVML library), `NVTUI_THEME`,
`NVTUI_CONFIG` (config file path), `NVTUI_COLOR=256` (force 256 colors).
Settings are saved to `~/.config/nvtui/config.json` (`%APPDATA%\nvtui` on Windows).

## Renaming the app

The name lives in one place: `name` in `Cargo.toml`. The binary, the UI
title, env-var prefixes (`<NAME>_*`), the config directory, Prometheus metric
names and the build scripts all derive from it (`src/brand.rs`). Change it
there and update this README.

## Architecture

```
src/
  main.rs          CLI and mode dispatch
  brand.rs         app name, env-var helpers (rename here via Cargo.toml)
  model.rs         vendor-neutral data model (Option = "not reported")
  backend/         one module per vendor, all behind the `Backend` trait
    nvml/          NVIDIA: hand-rolled, runtime-loaded NVML bindings
    apple.rs       Apple Silicon: IOKit, IOReport, HID
    amd.rs         AMD Linux: sysfs/hwmon
    drm_fdinfo.rs  per-process usage for any DRM driver (AMD, Intel, ...)
    demo.rs        simulated GPUs
  collector.rs     samples all backends + host into a Snapshot; sampler thread
  host.rs          CPU/RAM, container/WSL detection, process join, signals
  pidmap.rs        driver PID -> local PID mapping (containers)
  winpdh.rs        Windows WDDM per-process counters
  report.rs        --once / --json / --diagnose
  serve.rs         --serve exporter
  config.rs        persisted preferences
  ui/              ratatui TUI (state + input in mod.rs, rendering in draw.rs,
                   custom widgets, themes)
```

Adding a vendor means implementing `Backend` (static `GpuInfo`, then a
`GpuSample` per tick) and registering it in `backend::detect`. Anything the
vendor can't report stays `None` and the UI hides or greys it out. See
[docs/backends.md](docs/backends.md) for research notes on the next ones
(Intel, AMD on Windows, MIG, Hopper GPM metrics).

## Development

- `cargo test`. The Linux-only tests (AMD, fdinfo) can be run in Docker:
  `docker run --rm -v $PWD:/io -w /io messense/cargo-zigbuild cargo test`
- `scripts/tui-shot.sh` runs the Linux binary in a detached tmux session and
  dumps the screen; `scripts/ansi2png.py` renders an ANSI capture to PNG.
- `scripts/mac-sync.sh` builds and runs on a Mac over SSH.
