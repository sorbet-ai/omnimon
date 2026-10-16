//! Host-side data: CPU/RAM, environment detection, and joining GPU
//! processes with OS process info.

use std::collections::HashMap;

use sysinfo::{Components, Disks, Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind, Users};

use crate::model::{GpuProcess, HostInfo, HostSample, PidMapping, PidScope, ProcRow};
use crate::os::PidResolver;

pub struct Host {
    sys: System,
    users: Users,
    /// uid -> user name (users rarely change; refreshed on a miss).
    user_names: HashMap<String, Option<String>>,
    mapper: PidResolver,
    networks: Networks,
    disks: Disks,
    components: Components,
    last_io: std::time::Instant,
    pub info: HostInfo,
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
        if let Ok(e) = std::fs::read_to_string("/proc/1/environ") {
            if e.contains("container=") {
                return Some("container".into());
            }
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
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
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
        self.components.refresh(true);
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
        let cpu_temp = self
            .components
            .iter()
            .filter(|c| {
                let l = c.label().to_lowercase();
                ["cpu", "package", "tctl", "tdie", "core", "k10temp", "coretemp"].iter().any(|k| l.contains(k))
            })
            .filter_map(|c| c.temperature())
            .filter(|t| t.is_finite() && *t > 0.0 && *t < 150.0)
            .reduce(f32::max);
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

        // A process using several GPUs appears once per GPU; refresh it once.
        let mut local: Vec<Pid> = joined.iter().filter_map(|j| j.2.map(Pid::from_u32)).collect();
        local.sort_unstable();
        local.dedup();
        if !local.is_empty() {
            let kind = ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_user(UpdateKind::OnlyIfNotSet);
            self.sys.refresh_processes_specifics(ProcessesToUpdate::Some(&local), true, kind);
        }

        joined
            .into_iter()
            .map(|(gpu, usage, local_pid, mapping)| {
                let os = local_pid.and_then(|l| self.sys.process(Pid::from_u32(l)));
                let (name, cmdline, user, cpu, rss, runtime) = match os {
                    Some(o) => {
                        let user = o.user_id().and_then(|uid| {
                            let users = &mut self.users;
                            self.user_names
                                .entry(uid.to_string())
                                .or_insert_with(|| {
                                    users.refresh();
                                    users.get_user_by_id(uid).map(|u| u.name().to_string())
                                })
                                .clone()
                        });
                        let name = o.name().to_string_lossy().into_owned();
                        let cmd: Vec<String> = o.cmd().iter().map(|s| s.to_string_lossy().into_owned()).collect();
                        let mut cmdline = if cmd.is_empty() { name.clone() } else { cmd.join(" ") };
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
                ProcRow { gpu, usage, local_pid, mapping, name, cmdline, user, cpu, rss, runtime, spilling: false }
            })
            .collect()
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
