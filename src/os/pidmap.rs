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
//!
//! The decision logic is generic over [`ProcFs`] so it can be tested without
//! a real /proc; only the Linux implementation touches the filesystem.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::model::PidMapping;

pub type Resolution = HashMap<u32, (Option<u32>, PidMapping)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuFd {
    Yes,
    No,
    /// Can't tell (no permission to read the process's fds).
    Unknown,
}

/// What the mapper needs to know about processes.
pub trait ProcFs {
    fn exists(&self, pid: u32) -> bool;
    fn holds_gpu(&self, pid: u32) -> GpuFd;
    fn pids(&self) -> Vec<u32>;
    /// PID shown in the first line of /proc/<pid>/sched.
    fn sched_pid(&self, pid: u32) -> Option<u32>;
    fn self_pid(&self) -> u32;
    fn self_sched_pid(&self) -> Option<u32>;
}

pub struct PidMapper<F: ProcFs> {
    fs: F,
    in_container: bool,
    /// `/proc/<pid>/sched` shows host PIDs (self-test result).
    sched_leaks: bool,
    cache: Resolution,
    cached_set: Vec<u32>,
    last_full: Option<Instant>,
}

impl<F: ProcFs> PidMapper<F> {
    pub fn with_fs(fs: F, in_container: bool) -> Self {
        let sched_leaks = fs.self_sched_pid().is_some_and(|p| p != fs.self_pid());
        PidMapper { fs, in_container, sched_leaks, cache: HashMap::new(), cached_set: Vec::new(), last_full: None }
    }

    pub fn resolve(&mut self, pids: &[u32]) -> Resolution {
        let mut set: Vec<u32> = pids.to_vec();
        set.sort_unstable();
        set.dedup();
        let stale = self.last_full.is_none_or(|t| t.elapsed() > Duration::from_secs(5));
        if set == self.cached_set && !stale {
            return self.cache.clone();
        }
        self.last_full = Some(Instant::now());

        let mut out = HashMap::new();
        let mut unresolved = Vec::new();
        for &p in &set {
            if self.fs.exists(p) {
                match self.fs.holds_gpu(p) {
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
            let locals = self.fs.pids();
            let mut taken: HashSet<u32> = out.values().filter_map(|v| v.0).collect();

            if self.sched_leaks {
                let host_to_local: HashMap<u32, u32> = locals.iter().filter_map(|&l| Some((self.fs.sched_pid(l)?, l))).collect();
                unresolved.retain(|p| match host_to_local.get(p) {
                    Some(&l) if !taken.contains(&l) => {
                        out.insert(*p, (Some(l), PidMapping::Translated));
                        taken.insert(l);
                        false
                    }
                    _ => true,
                });
            }

            if !unresolved.is_empty() {
                let me = self.fs.self_pid();
                let mut candidates: Vec<u32> = locals
                    .iter()
                    .copied()
                    .filter(|l| !taken.contains(l) && !set.contains(l) && *l != me)
                    .filter(|&l| self.fs.holds_gpu(l) == GpuFd::Yes)
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
        self.cached_set = set;
        self.cache = out.clone();
        out
    }
}

/// The real /proc.
#[cfg(target_os = "linux")]
pub struct LinuxProc;

#[cfg(target_os = "linux")]
impl LinuxProc {
    fn sched(pid: &str) -> Option<u32> {
        // First line: "python3 (12345, #threads: 8)"
        let s = std::fs::read_to_string(format!("/proc/{pid}/sched")).ok()?;
        let line = s.lines().next()?;
        let rest = &line[line.rfind('(')? + 1..];
        rest.split(',').next()?.trim().parse().ok()
    }
}

#[cfg(target_os = "linux")]
impl ProcFs for LinuxProc {
    fn exists(&self, pid: u32) -> bool {
        std::fs::metadata(format!("/proc/{pid}")).is_ok()
    }
    fn holds_gpu(&self, pid: u32) -> GpuFd {
        let Ok(rd) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { return GpuFd::Unknown };
        for e in rd.flatten() {
            if let Ok(t) = std::fs::read_link(e.path()) {
                let s = t.to_string_lossy();
                if s.starts_with("/dev/nvidia") || s == "/dev/dxg" {
                    return GpuFd::Yes;
                }
            }
        }
        GpuFd::No
    }
    fn pids(&self) -> Vec<u32> {
        std::fs::read_dir("/proc").map(|rd| rd.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect()).unwrap_or_default()
    }
    fn sched_pid(&self, pid: u32) -> Option<u32> {
        Self::sched(&pid.to_string())
    }
    fn self_pid(&self) -> u32 {
        std::process::id()
    }
    fn self_sched_pid(&self) -> Option<u32> {
        Self::sched("self")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake process table: pid -> (holds gpu, sched pid).
    #[derive(Default)]
    struct Fake {
        procs: HashMap<u32, (GpuFd, Option<u32>)>,
        me: u32,
        me_sched: Option<u32>,
    }

    impl ProcFs for Fake {
        fn exists(&self, pid: u32) -> bool {
            self.procs.contains_key(&pid)
        }
        fn holds_gpu(&self, pid: u32) -> GpuFd {
            self.procs.get(&pid).map(|p| p.0).unwrap_or(GpuFd::No)
        }
        fn pids(&self) -> Vec<u32> {
            self.procs.keys().copied().collect()
        }
        fn sched_pid(&self, pid: u32) -> Option<u32> {
            self.procs.get(&pid).and_then(|p| p.1)
        }
        fn self_pid(&self) -> u32 {
            self.me
        }
        fn self_sched_pid(&self) -> Option<u32> {
            self.me_sched
        }
    }

    fn fake(procs: &[(u32, GpuFd, Option<u32>)]) -> Fake {
        Fake { procs: procs.iter().map(|&(p, g, s)| (p, (g, s))).collect(), me: 1, me_sched: None }
    }

    #[test]
    fn same_namespace_is_direct() {
        let mut m = PidMapper::with_fs(fake(&[(100, GpuFd::Yes, None)]), false);
        assert_eq!(m.resolve(&[100])[&100], (Some(100), PidMapping::Direct));
    }

    #[test]
    fn unreadable_fds_trusted_only_outside_containers() {
        let f = || fake(&[(100, GpuFd::Unknown, None)]);
        assert_eq!(PidMapper::with_fs(f(), false).resolve(&[100])[&100].1, PidMapping::Direct);
        assert_eq!(PidMapper::with_fs(f(), true).resolve(&[100])[&100].1, PidMapping::External);
    }

    #[test]
    fn pid_collision_is_not_trusted() {
        // Host pid 100 exists locally but is an unrelated process; local 7 is the GPU user.
        let mut m = PidMapper::with_fs(fake(&[(100, GpuFd::No, None), (7, GpuFd::Yes, None)]), true);
        assert_eq!(m.resolve(&[100])[&100], (Some(7), PidMapping::Translated));
    }

    #[test]
    fn sched_leak_translates_exactly() {
        let mut f = fake(&[(7, GpuFd::Yes, Some(5007)), (8, GpuFd::Yes, Some(5008))]);
        f.me_sched = Some(4001); // differs from self pid 1 -> leak detected
        let r = PidMapper::with_fs(f, true).resolve(&[5008, 5007]);
        assert_eq!(r[&5007], (Some(7), PidMapping::Translated));
        assert_eq!(r[&5008], (Some(8), PidMapping::Translated));
    }

    #[test]
    fn sched_match_is_not_reused_by_heuristics() {
        // 5007 maps via sched to 7; 5009 must not also be paired with 7.
        let mut f = fake(&[(7, GpuFd::Yes, Some(5007))]);
        f.me_sched = Some(4001);
        let r = PidMapper::with_fs(f, true).resolve(&[5007, 5009]);
        assert_eq!(r[&5007], (Some(7), PidMapping::Translated));
        assert_eq!(r[&5009], (None, PidMapping::External));
    }

    #[test]
    fn several_candidates_are_guessed_in_order() {
        let r = PidMapper::with_fs(fake(&[(7, GpuFd::Yes, None), (9, GpuFd::Yes, None)]), true).resolve(&[5009, 5007]);
        assert_eq!(r[&5007], (Some(7), PidMapping::Guessed));
        assert_eq!(r[&5009], (Some(9), PidMapping::Guessed));
    }

    #[test]
    fn mismatched_counts_are_external() {
        let r = PidMapper::with_fs(fake(&[(7, GpuFd::Yes, None)]), true).resolve(&[5007, 5008]);
        assert!(r.values().all(|v| v.1 == PidMapping::External));
    }
}
