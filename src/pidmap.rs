//! Map PIDs reported by the GPU driver to processes we can actually see.
//!
//! NVML reports PIDs in the *host* PID namespace. Inside a container those
//! PIDs either don't exist in our `/proc` or, worse, belong to an unrelated
//! process. Tools that trust them blindly show blank rows or the wrong
//! command (and would kill the wrong thing). We verify and translate:
//!
//! 1. `/proc/<pid>` exists *and* holds a GPU device fd open -> Direct.
//!    (If we can't read its fds and we're not in a container, trust it.)
//! 2. `/proc/<pid>/sched` leaks the host PID on some kernels; if a
//!    self-test shows that, use it to translate exactly.
//! 3. Otherwise match the remaining driver PIDs against local processes that
//!    hold `/dev/nvidia*` open: a unique 1:1 match is Translated; several are
//!    paired in PID order and flagged Guessed.
//! 4. Anything left is External (another container or the host).

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use crate::model::PidMapping;

#[derive(Default)]
pub struct PidMapper {
    #[cfg(target_os = "linux")]
    linux: linux::State,
}

impl PidMapper {
    pub fn new(in_container: bool) -> Self {
        let _ = in_container;
        PidMapper {
            #[cfg(target_os = "linux")]
            linux: linux::State::new(in_container),
        }
    }

    /// Resolve driver PIDs to (local pid, mapping).
    pub fn resolve(&mut self, pids: &[u32]) -> HashMap<u32, (Option<u32>, PidMapping)> {
        #[cfg(target_os = "linux")]
        {
            self.linux.resolve(pids)
        }
        #[cfg(not(target_os = "linux"))]
        {
            pids.iter().map(|&p| (p, (Some(p), PidMapping::Direct))).collect()
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::fs;

    #[derive(Default)]
    pub struct State {
        in_container: bool,
        /// `/proc/<pid>/sched` shows host PIDs (self-test result).
        sched_leaks: bool,
        cache: HashMap<u32, (Option<u32>, PidMapping)>,
        cached_set: Vec<u32>,
        last_full: Option<Instant>,
    }

    pub enum GpuFd {
        Yes,
        No,
        Unknown,
    }

    /// Does the process hold an NVIDIA (or WSL dxg) device open?
    pub fn holds_gpu(pid: u32) -> GpuFd {
        let Ok(rd) = fs::read_dir(format!("/proc/{pid}/fd")) else { return GpuFd::Unknown };
        for e in rd.flatten() {
            if let Ok(t) = fs::read_link(e.path()) {
                let s = t.to_string_lossy();
                if s.starts_with("/dev/nvidia") || s == "/dev/dxg" {
                    return GpuFd::Yes;
                }
            }
        }
        GpuFd::No
    }

    fn sched_pid(pid: &str) -> Option<u32> {
        // First line: "python3 (12345, #threads: 8)"
        let s = fs::read_to_string(format!("/proc/{pid}/sched")).ok()?;
        let line = s.lines().next()?;
        let open = line.rfind('(')?;
        let rest = &line[open + 1..];
        rest.split(',').next()?.trim().parse().ok()
    }

    fn local_pids() -> Vec<u32> {
        fs::read_dir("/proc")
            .map(|rd| rd.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect())
            .unwrap_or_default()
    }

    impl State {
        pub fn new(in_container: bool) -> Self {
            let me = std::process::id();
            let sched_leaks = sched_pid("self").is_some_and(|p| p != me);
            State { in_container, sched_leaks, ..Default::default() }
        }

        pub fn resolve(&mut self, pids: &[u32]) -> HashMap<u32, (Option<u32>, PidMapping)> {
            let mut set: Vec<u32> = pids.to_vec();
            set.sort_unstable();
            set.dedup();
            let stale = self.last_full.is_none_or(|t| t.elapsed() > Duration::from_secs(5));
            if set == self.cached_set && !stale {
                return self.cache.clone();
            }
            self.cached_set = set.clone();
            self.last_full = Some(Instant::now());

            let mut out = HashMap::new();
            let mut unresolved = Vec::new();
            for &p in &set {
                if fs::metadata(format!("/proc/{p}")).is_ok() {
                    match holds_gpu(p) {
                        GpuFd::Yes => {
                            out.insert(p, (Some(p), PidMapping::Direct));
                            continue;
                        }
                        GpuFd::Unknown if !self.in_container => {
                            out.insert(p, (Some(p), PidMapping::Direct));
                            continue;
                        }
                        _ => {}
                    }
                }
                unresolved.push(p);
            }

            if !unresolved.is_empty() {
                let locals = local_pids();
                let taken: HashSet<u32> = out.values().filter_map(|v| v.0).collect();

                if self.sched_leaks {
                    let mut host_to_local = HashMap::new();
                    for &l in &locals {
                        if let Some(h) = sched_pid(&l.to_string()) {
                            host_to_local.insert(h, l);
                        }
                    }
                    unresolved.retain(|p| match host_to_local.get(p) {
                        Some(&l) => {
                            out.insert(*p, (Some(l), PidMapping::Translated));
                            false
                        }
                        None => true,
                    });
                }

                if !unresolved.is_empty() {
                    let mut candidates: Vec<u32> = locals
                        .iter()
                        .copied()
                        .filter(|l| !taken.contains(l) && !set.contains(l) && *l != std::process::id())
                        .filter(|&l| matches!(holds_gpu(l), GpuFd::Yes))
                        .collect();
                    candidates.sort_unstable();
                    if unresolved.len() == 1 && candidates.len() == 1 {
                        out.insert(unresolved[0], (Some(candidates[0]), PidMapping::Translated));
                        unresolved.clear();
                    } else if !candidates.is_empty() && candidates.len() == unresolved.len() {
                        // PIDs are allocated in order in both namespaces, so the
                        // sorted orders usually agree. Usually.
                        for (&h, &l) in unresolved.iter().zip(&candidates) {
                            out.insert(h, (Some(l), PidMapping::Guessed));
                        }
                        unresolved.clear();
                    }
                }
                for p in unresolved {
                    out.insert(p, (None, PidMapping::External));
                }
            }
            self.cache = out.clone();
            out
        }
    }
}
