//! Host-side data: CPU/RAM, environment detection, and joining GPU
//! processes with OS process info.

use std::collections::HashMap;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind, Users};

use crate::model::{GpuProcess, HostInfo, HostSample, PidMapping, PidScope, ProcRow};
use crate::os::PidResolver;

pub struct Host {
    sys: System,
    users: Users,
    /// uid -> user name (users rarely change; refreshed on a miss).
    user_names: HashMap<String, Option<String>>,
    mapper: PidResolver,
    pub info: HostInfo,
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
        Host { sys, users: Users::new_with_refreshed_list(), user_names: HashMap::new(), mapper: PidResolver::new(in_container), info }
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
        HostSample {
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

        let local: Vec<Pid> = joined.iter().filter_map(|j| j.2.map(Pid::from_u32)).collect();
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
