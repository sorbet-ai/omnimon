//! Host-side data: CPU/RAM, environment detection, and joining GPU
//! processes with OS process info.

use std::collections::{HashMap, HashSet};

use sysinfo::{Components, Disks, Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind, Users};

use crate::model::{GpuProcess, HostInfo, HostSample, NO_GPU, PidMapping, PidScope, ProcRow};
use crate::os::PidResolver;

/// Host process list size: the busiest by CPU, plus the largest by RAM.
pub const HOST_PROCS_BY_CPU: usize = 80;
pub const HOST_PROCS_BY_RAM: usize = 20;

fn user_of(o: &sysinfo::Process, users: &mut Users, names: &mut HashMap<String, Option<String>>) -> Option<String> {
    let uid = o.user_id()?;
    names
        .entry(uid.to_string())
        .or_insert_with(|| {
            users.refresh();
            users.get_user_by_id(uid).map(|u| u.name().to_string())
        })
        .clone()
}

fn name_and_cmd(o: &sysinfo::Process) -> (String, String) {
    let name = o.name().to_string_lossy().into_owned();
    let cmd: Vec<String> = o.cmd().iter().map(|s| s.to_string_lossy().into_owned()).collect();
    let cmdline = if cmd.is_empty() { name.clone() } else { cmd.join(" ") };
    (name, cmdline)
}

pub struct Host {
    sys: System,
    users: Users,
    /// uid -> user name (users rarely change; refreshed on a miss).
    user_names: HashMap<String, Option<String>>,
    mapper: PidResolver,
    /// pid -> container (a process never changes container).
    containers: HashMap<u32, Option<String>>,
    networks: Networks,
    disks: Disks,
    components: Components,
    /// Sensor reads are slow on some systems (WMI on Windows): refresh them
    /// every few samples and reuse the reading in between.
    cpu_temp: Option<f32>,
    ticks: u64,
    last_io: std::time::Instant,
    pub info: HostInfo,
}

/// Container runtime and short id from /proc/<pid>/cgroup, e.g.
/// ".../docker-<64 hex>.scope" -> "docker:<12 hex>". Kubernetes pods add the
/// pod uid prefix.
pub fn container_of(cgroup: &str) -> Option<String> {
    let hex_id = |s: &str| -> Option<String> {
        let id: String = s.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        (id.len() >= 32).then(|| id[..12].to_string())
    };
    let mut pod = None;
    for line in cgroup.lines() {
        let path = line.rsplit(':').next().unwrap_or("");
        for part in path.split('/') {
            if part.starts_with("kubepods")
                && let Some(i) = part.rfind("-pod")
            {
                let uid: String =
                    part[i + 4..].trim_end_matches(".slice").chars().take(8).map(|c| if c == '_' { '-' } else { c }).collect();
                if uid.chars().count() == 8 {
                    pod = Some(uid);
                }
            }
            for (prefix, runtime) in [
                ("docker-", "docker"),
                ("cri-containerd-", "containerd"),
                ("crio-", "cri-o"),
                ("libpod-", "podman"),
                ("containerd-", "containerd"),
            ] {
                if let Some(rest) = part.strip_prefix(prefix)
                    && let Some(id) = hex_id(rest)
                {
                    return Some(match pod {
                        Some(p) => format!("pod {p} · {runtime}:{id}"),
                        None => format!("{runtime}:{id}"),
                    });
                }
            }
        }
        // cgroup v1 style: /docker/<id>
        if let Some(i) = path.find("/docker/")
            && let Some(id) = hex_id(&path[i + 8..])
        {
            return Some(format!("docker:{id}"));
        }
    }
    None
}

fn is_loopback(name: &str) -> bool {
    name == "lo" || name.starts_with("lo0") || name.to_lowercase().contains("loopback")
}

fn detect_container() -> Option<String> {
    if std::env::var_os("KUBERNETES_SERVICE_HOST").is_some() {
        return Some("kubernetes".into());
    }
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new("/.dockerenv").exists() {
            return Some("docker".into());
        }
        if std::path::Path::new("/run/.containerenv").exists() {
            return Some("podman".into());
        }
        if let Ok(c) = std::fs::read_to_string("/proc/1/cgroup") {
            for (k, v) in [("kubepods", "kubernetes"), ("docker", "docker"), ("lxc", "lxc"), ("containerd", "containerd")] {
                if c.contains(k) {
                    return Some(v.into());
                }
            }
        }
        if let Ok(e) = std::fs::read_to_string("/proc/1/environ")
            && e.contains("container=")
        {
            return Some("container".into());
        }
    }
    None
}

fn detect_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists()
            || std::fs::read_to_string("/proc/version").is_ok_and(|v| v.to_lowercase().contains("microsoft"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

impl Host {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        sys.refresh_memory();
        let container = detect_container();
        let in_container = container.is_some();
        let info = HostInfo {
            hostname: System::host_name().unwrap_or_else(|| "localhost".into()),
            os: System::long_os_version().or_else(System::name).unwrap_or_else(|| std::env::consts::OS.into()),
            kernel: System::kernel_version(),
            cpu_brand: sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default(),
            cpu_cores: sys.cpus().len(),
            wsl: detect_wsl(),
            container,
        };
        Host {
            sys,
            users: Users::new_with_refreshed_list(),
            user_names: HashMap::new(),
            mapper: PidResolver::new(in_container),
            containers: HashMap::new(),
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            cpu_temp: None,
            ticks: 0,
            last_io: std::time::Instant::now(),
            info,
        }
    }

    pub fn sample(&mut self) -> HostSample {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let load = if cfg!(windows) {
            None
        } else {
            let l = System::load_average();
            Some([l.one, l.five, l.fifteen])
        };
        // Network and disk counters are deltas since the previous refresh.
        let dt = self.last_io.elapsed().as_secs_f64().max(0.001);
        self.last_io = std::time::Instant::now();
        self.networks.refresh(true);
        self.disks.refresh(true);
        let per_s = |b: u64| Some((b as f64 / dt) as u64);
        let (rx, tx) = self
            .networks
            .iter()
            .filter(|(name, _)| !is_loopback(name))
            .fold((0, 0), |(r, t), (_, d)| (r + d.received(), t + d.transmitted()));
        // Partitions of one device report the same counters; count each device once.
        let mut seen = std::collections::HashSet::new();
        let (rd, wr) = self.disks.iter().filter(|d| seen.insert(d.name().to_os_string())).fold((0, 0), |(r, w), d| {
            let u = d.usage();
            (r + u.read_bytes, w + u.written_bytes)
        });
        if self.ticks.is_multiple_of(5) {
            self.components.refresh(true);
            self.cpu_temp = self
                .components
                .iter()
                .filter(|c| {
                    let l = c.label().to_lowercase();
                    ["cpu", "package", "tctl", "tdie", "core", "k10temp", "coretemp"].iter().any(|k| l.contains(k))
                })
                .filter_map(|c| c.temperature())
                .filter(|t| t.is_finite() && *t > 0.0 && *t < 150.0)
                .reduce(f32::max);
        }
        self.ticks += 1;
        let cpu_temp = self.cpu_temp;
        HostSample {
            net_rx: per_s(rx),
            net_tx: per_s(tx),
            disk_read: per_s(rd),
            disk_write: per_s(wr),
            cpu_temp,
            cpu: self.sys.global_cpu_usage(),
            per_core: self.sys.cpus().iter().map(|c| c.cpu_usage()).collect(),
            mem_used: self.sys.used_memory(),
            mem_total: self.sys.total_memory(),
            swap_used: self.sys.used_swap(),
            swap_total: self.sys.total_swap(),
            load,
            uptime: System::uptime(),
        }
    }

    /// Join (gpu index, process) pairs with OS info.
    pub fn processes(&mut self, procs: Vec<(usize, GpuProcess)>) -> Vec<ProcRow> {
        let driver_pids: Vec<u32> = procs.iter().filter(|(_, p)| p.scope == PidScope::Driver).map(|(_, p)| p.pid).collect();
        let map = self.mapper.resolve(&driver_pids);

        let resolve = |p: &GpuProcess| -> (Option<u32>, PidMapping) {
            match p.scope {
                PidScope::Synthetic => (None, PidMapping::Direct),
                PidScope::Local => (Some(p.pid), PidMapping::Direct),
                PidScope::Driver => map.get(&p.pid).copied().unwrap_or((None, PidMapping::External)),
            }
        };
        let joined: Vec<(usize, GpuProcess, Option<u32>, PidMapping)> = procs
            .into_iter()
            .map(|(gpu, p)| {
                let (l, m) = resolve(&p);
                (gpu, p, l, m)
            })
            .collect();

        // Every process, once per tick: GPU rows join against it and the
        // host process list (`other_processes`) reads from it. Refreshing
        // twice in one tick would measure CPU over a few milliseconds.
        let kind =
            ProcessRefreshKind::nothing().with_cpu().with_memory().with_cmd(UpdateKind::OnlyIfNotSet).with_user(UpdateKind::OnlyIfNotSet);
        self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);

        let rows: Vec<ProcRow> = joined
            .into_iter()
            .map(|(gpu, usage, local_pid, mapping)| {
                let os = local_pid.and_then(|l| self.sys.process(Pid::from_u32(l)));
                let (name, cmdline, user, cpu, rss, runtime) = match os {
                    Some(o) => {
                        let user = user_of(o, &mut self.users, &mut self.user_names);
                        let (name, mut cmdline) = name_and_cmd(o);
                        if name.eq_ignore_ascii_case("vmwp.exe") {
                            // GPU work done inside WSL2 / Docker Desktop / Hyper-V VMs is
                            // attributed to the VM worker process on the Windows side.
                            cmdline = format!("{name}  [WSL2 / Hyper-V VM: GPU work from inside the VM]");
                        }
                        (name, cmdline, user, Some(o.cpu_usage()), Some(o.memory()), Some(o.run_time()))
                    }
                    None => {
                        let n = usage.name_hint.clone().unwrap_or_else(|| match mapping {
                            PidMapping::External => "[outside this namespace]".into(),
                            _ => "?".into(),
                        });
                        (n.clone(), n, None, None, None, None)
                    }
                };
                let container = local_pid.and_then(|l| self.container(l));
                ProcRow { gpu, usage, local_pid, mapping, name, cmdline, user, cpu, rss, runtime, spilling: false, container }
            })
            .collect();
        rows
    }

    fn container(&mut self, pid: u32) -> Option<String> {
        self.containers
            .entry(pid)
            .or_insert_with(|| std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok().and_then(|t| container_of(&t)))
            .clone()
    }

    /// The busiest processes that aren't in `gpu_rows`: the top
    /// [`HOST_PROCS_BY_CPU`] by CPU plus the top [`HOST_PROCS_BY_RAM`] by
    /// memory, from the process table [`Host::processes`] refreshed this tick.
    pub fn other_processes(&mut self, gpu_rows: &[ProcRow]) -> Vec<ProcRow> {
        let on_gpu: HashSet<u32> = gpu_rows.iter().filter_map(|r| r.local_pid).collect();
        let me = std::process::id();
        let mut cands: Vec<(u32, f32, u64)> = self
            .sys
            .processes()
            .iter()
            .filter(|(pid, p)| p.thread_kind().is_none() && pid.as_u32() != 0 && !on_gpu.contains(&pid.as_u32()))
            .map(|(pid, p)| (pid.as_u32(), p.cpu_usage(), p.memory()))
            .collect();
        cands.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)));
        let mut pick: Vec<u32> = cands.iter().take(HOST_PROCS_BY_CPU).map(|c| c.0).collect();
        let by_cpu: HashSet<u32> = pick.iter().copied().collect();
        cands.sort_by(|a, b| b.2.cmp(&a.2));
        pick.extend(cands.iter().map(|c| c.0).filter(|p| !by_cpu.contains(p)).take(HOST_PROCS_BY_RAM));

        let mut rows = Vec::with_capacity(pick.len());
        for pid in pick {
            let Some(o) = self.sys.process(Pid::from_u32(pid)) else { continue };
            let user = user_of(o, &mut self.users, &mut self.user_names);
            let (name, cmdline) = name_and_cmd(o);
            let (cpu, rss, runtime) = (o.cpu_usage(), o.memory(), o.run_time());
            let container = self.container(pid);
            rows.push(ProcRow {
                gpu: NO_GPU,
                usage: GpuProcess { pid, scope: PidScope::Local, ..Default::default() },
                local_pid: Some(pid),
                mapping: PidMapping::Direct,
                name,
                cmdline: if pid == me { format!("{cmdline}  [this monitor]") } else { cmdline },
                user,
                cpu: Some(cpu),
                rss: Some(rss),
                runtime: Some(runtime),
                spilling: false,
                container,
            });
        }
        // Forget containers of processes that are gone (pids get reused).
        if self.containers.len() > 4096 {
            let live: HashSet<u32> = self.sys.processes().keys().map(|p| p.as_u32()).collect();
            self.containers.retain(|pid, _| live.contains(pid));
        }
        rows
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillSignal {
    Interrupt,
    Terminate,
    Kill,
}

impl KillSignal {
    /// Signals this platform can actually deliver. Windows only has
    /// TerminateProcess.
    pub fn available() -> &'static [KillSignal] {
        if cfg!(windows) { &[KillSignal::Kill] } else { &[KillSignal::Interrupt, KillSignal::Terminate, KillSignal::Kill] }
    }

    pub fn label(self) -> &'static str {
        if cfg!(windows) {
            return "terminate";
        }
        match self {
            KillSignal::Interrupt => "SIGINT",
            KillSignal::Terminate => "SIGTERM",
            KillSignal::Kill => "SIGKILL",
        }
    }
}

/// Send a signal to a local PID. On Windows everything becomes TerminateProcess.
pub fn send_signal(pid: u32, sig: KillSignal) -> Result<(), String> {
    let mut sys = System::new();
    let p = Pid::from_u32(pid);
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[p]), true, ProcessRefreshKind::nothing());
    let proc_ = sys.process(p).ok_or_else(|| format!("process {pid} not found"))?;
    let signal = match sig {
        KillSignal::Interrupt => Signal::Interrupt,
        KillSignal::Terminate => Signal::Term,
        KillSignal::Kill => Signal::Kill,
    };
    match proc_.kill_with(signal) {
        Some(true) => Ok(()),
        Some(false) => Err(format!("failed to signal {pid} (permission denied?)")),
        None => {
            // Signal not supported on this platform (Windows): hard kill.
            if proc_.kill() { Ok(()) } else { Err(format!("failed to kill {pid}")) }
        }
    }
}

/// On-demand detail for one local process (process popup): ancestry,
/// children, executable, working directory and environment.
#[derive(Clone, Debug, Default)]
pub struct ProcDetails {
    /// Root first, the process itself last.
    pub ancestors: Vec<(u32, String)>,
    pub children: Vec<(u32, String)>,
    pub exe: Option<String>,
    pub cwd: Option<String>,
    /// GPU-relevant variables first, then the rest; empty when unreadable.
    pub env: Vec<(String, String)>,
}

/// Prefixes of environment variables that steer GPU work.
const GPU_ENV: &[&str] = &[
    "CUDA_",
    "NVIDIA_",
    "NCCL_",
    "HIP_",
    "ROCR_",
    "ROCM_",
    "HSA_",
    "GPU_",
    "ONEAPI_",
    "ZE_",
    "SYCL_",
    "PYTORCH_",
    "TORCH_",
    "TF_",
    "XLA_",
    "JAX_",
    "OMP_NUM_THREADS",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "VK_",
    "MESA_",
    "DXVK_",
    "MTL_",
    "METAL_",
];

pub fn is_gpu_env(key: &str) -> bool {
    GPU_ENV.iter().any(|p| key.starts_with(p))
}

/// Takes a full process-table refresh (tens of ms), so only call on demand.
pub fn process_details(pid: u32) -> Option<ProcDetails> {
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let me = Pid::from_u32(pid);
    let full = ProcessRefreshKind::nothing().with_exe(UpdateKind::Always).with_cwd(UpdateKind::Always).with_environ(UpdateKind::Always);
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[me]), false, full);
    let p = sys.process(me)?;
    let name = |p: &sysinfo::Process| p.name().to_string_lossy().into_owned();

    let mut ancestors = vec![(pid, name(p))];
    let mut cur = p.parent();
    while let Some(pp) = cur {
        // Guard against cycles from PID reuse.
        if ancestors.len() > 64 || ancestors.iter().any(|(q, _)| *q == pp.as_u32()) {
            break;
        }
        let Some(q) = sys.process(pp) else { break };
        ancestors.push((pp.as_u32(), name(q)));
        cur = q.parent();
    }
    ancestors.reverse();

    let mut children: Vec<(u32, String)> = sys
        .processes()
        .values()
        .filter(|c| c.parent() == Some(me) && c.thread_kind().is_none())
        .map(|c| (c.pid().as_u32(), name(c)))
        .collect();
    children.sort();

    let mut env: Vec<(String, String)> = p
        .environ()
        .iter()
        .filter_map(|kv| {
            let kv = kv.to_string_lossy();
            let (k, v) = kv.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect();
    env.sort_by(|a, b| is_gpu_env(&b.0).cmp(&is_gpu_env(&a.0)).then(a.0.cmp(&b.0)));

    Some(ProcDetails {
        ancestors,
        children,
        exe: p.exe().map(|e| e.display().to_string()),
        cwd: p.cwd().map(|c| c.display().to_string()),
        env,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_from_cgroups() {
        let id = "3f2a1b9c0d12aabbccddeeff00112233445566778899aabbccddeeff00112233";
        assert_eq!(container_of(&format!("0::/system.slice/docker-{id}.scope\n")).as_deref(), Some("docker:3f2a1b9c0d12"));
        assert_eq!(container_of(&format!("12:memory:/docker/{id}\n")).as_deref(), Some("docker:3f2a1b9c0d12"));
        let k8s =
            format!("0::/kubepods.slice/kubepods-besteffort.slice/kubepods-besteffort-pod1a2b3c4d_5e6f.slice/cri-containerd-{id}.scope");
        assert_eq!(container_of(&k8s).as_deref(), Some("pod 1a2b3c4d · containerd:3f2a1b9c0d12"));
        assert_eq!(container_of("0::/user.slice/user-1000.slice/session-2.scope\n"), None);
    }

    #[test]
    fn details_of_self() {
        let d = process_details(std::process::id()).expect("own process is visible");
        assert_eq!(d.ancestors.last().unwrap().0, std::process::id());
        assert!(d.ancestors.len() >= 2, "has a parent: {:?}", d.ancestors);
        assert!(d.exe.is_some());
        assert!(!d.env.is_empty());
        // GPU-relevant variables sort first.
        if let Some(first_other) = d.env.iter().position(|(k, _)| !is_gpu_env(k)) {
            assert!(d.env[first_other..].iter().all(|(k, _)| !is_gpu_env(k)));
        }
    }
}
