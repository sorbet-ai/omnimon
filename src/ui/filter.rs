//! Process filter syntax: free text plus `key:value` terms, all ANDed.
//!
//!   python gpu:0 user:alice      text match + GPU + user
//!   pid:4242                     driver or local PID
//!   type:c / type:g              compute / graphics
//!   spill                        only processes spilling into system memory

use crate::model::{ProcKind, ProcRow};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    words: Vec<String>,
    gpu: Option<usize>,
    user: Option<String>,
    pid: Option<u32>,
    kind: Option<ProcKind>,
    spill: bool,
}

impl Filter {
    pub fn parse(s: &str) -> Filter {
        let mut f = Filter::default();
        for term in s.split_whitespace() {
            let lower = term.to_lowercase();
            match lower.split_once(':') {
                Some(("gpu", v)) if v.parse::<usize>().is_ok() => f.gpu = v.parse().ok(),
                Some(("user", v)) => f.user = Some(v.to_string()),
                Some(("pid", v)) if v.parse::<u32>().is_ok() => f.pid = v.parse().ok(),
                Some(("type", v)) => {
                    f.kind = match v {
                        "c" | "compute" => Some(ProcKind::Compute),
                        "g" | "graphics" => Some(ProcKind::Graphics),
                        "cg" | "c+g" | "both" => Some(ProcKind::Both),
                        _ => None,
                    }
                }
                _ if lower == "spill" || lower == "spill:yes" => f.spill = true,
                _ => f.words.push(lower),
            }
        }
        f
    }

    pub fn matches(&self, p: &ProcRow) -> bool {
        let cmd = || p.cmdline.to_lowercase();
        self.gpu.is_none_or(|g| p.gpu == g)
            && self.user.as_ref().is_none_or(|u| p.user.as_deref().is_some_and(|pu| pu.to_lowercase().contains(u)))
            && self.pid.is_none_or(|pid| p.pid() == pid || p.local_pid == Some(pid))
            && self.kind.is_none_or(|k| p.usage.kind == k || p.usage.kind == ProcKind::Both)
            && (!self.spill || p.spilling)
            && self.words.iter().all(|w| {
                cmd().contains(w)
                    || p.user.as_deref().is_some_and(|u| u.to_lowercase().contains(w))
                    || p.pid().to_string() == *w
                    || p.local_pid.is_some_and(|l| l.to_string() == *w)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GpuProcess, PidMapping};

    fn row(gpu: usize, pid: u32, user: &str, cmd: &str, kind: ProcKind, spilling: bool) -> ProcRow {
        ProcRow {
            gpu,
            usage: GpuProcess { pid, kind, ..Default::default() },
            local_pid: Some(pid + 1000),
            mapping: PidMapping::Translated,
            name: cmd.into(),
            cmdline: cmd.into(),
            user: Some(user.into()),
            cpu: None,
            rss: None,
            runtime: None,
            spilling,
            container: None,
        }
    }

    #[test]
    fn terms() {
        let a = row(0, 1, "alice", "python train.py", ProcKind::Compute, false);
        let b = row(1, 2, "bob", "blender", ProcKind::Graphics, true);
        let m = |q: &str| [&a, &b].iter().filter(|p| Filter::parse(q).matches(p)).map(|p| p.pid()).collect::<Vec<_>>();
        assert_eq!(m(""), [1, 2]);
        assert_eq!(m("python"), [1]);
        assert_eq!(m("gpu:1"), [2]);
        assert_eq!(m("user:ALI"), [1]);
        assert_eq!(m("pid:1002"), [2], "local pid");
        assert_eq!(m("type:c"), [1]);
        assert_eq!(m("spill"), [2]);
        assert_eq!(m("python gpu:1"), Vec::<u32>::new());
        assert_eq!(Filter::parse("  "), Filter::default());
    }
}
