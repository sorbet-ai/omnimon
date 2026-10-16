//! MIG (Multi-Instance GPU): the partitions of an A100/H100-class GPU, each
//! with its own memory, SMs and processes. Partitions can be created and
//! destroyed while we run, so they are enumerated every tick.

use std::collections::HashMap;
use std::ffi::c_uint;

use super::ffi::*;
use super::{Gpm, RawProc, list_procs};
use crate::model::MigInstance;

pub(super) struct Mig {
    max: u32,
    /// Per-partition GPM samplers (Hopper+), by GPU instance id.
    gpm: HashMap<u32, Option<Gpm>>,
}

impl Mig {
    /// Some when MIG mode is on.
    pub(super) fn probe(api: &Api, h: nvmlDevice_t) -> Option<Mig> {
        let (mut cur, mut pending): (c_uint, c_uint) = (0, 0);
        call!(api.mig_mode, h, &mut cur, &mut pending).ok().filter(|_| cur == 1)?;
        let mut max: c_uint = 0;
        call!(api.max_mig_count, h, &mut max).ok().filter(|_| max > 0)?;
        Some(Mig { max, gpm: HashMap::new() })
    }

    pub(super) fn free(&self, api: &Api) {
        for g in self.gpm.values().flatten() {
            g.free(api);
        }
    }

    /// The partitions, and the processes each one reports (with its GPU
    /// instance id), for when the parent's process list leaves them out.
    pub(super) fn sample(&mut self, api: &Api, h: nvmlDevice_t, gpm: bool, v3_stride: usize) -> (Vec<MigInstance>, Vec<(u32, RawProc)>) {
        let mut out = Vec::new();
        let mut procs = Vec::new();
        for i in 0..self.max {
            let mut m: nvmlDevice_t = std::ptr::null_mut();
            // NOT_FOUND for unused slots.
            if call!(api.mig_handle_by_index, h, i, &mut m).is_err() {
                continue;
            }
            let mut gi: c_uint = 0;
            if call!(api.gpu_instance_id, m, &mut gi).is_err() {
                continue;
            }
            let mut ci: c_uint = 0;
            let compute_instance = call!(api.compute_instance_id, m, &mut ci).ok().map(|_| ci);
            let name = super::dev_string(api.name, m).unwrap_or_default();
            let mut mem = nvmlMemory_t::default();
            let has_mem = call!(api.memory_v1, m, &mut mem).is_ok();
            let mut attrs = nvmlDeviceAttributes_t::default();
            let sm_count = call!(api.attributes_v2, m, &mut attrs).ok().map(|_| attrs.multiprocessor_count).filter(|&n| n > 0);
            let activity = if gpm {
                let g = self.gpm.entry(gi).or_insert_with(|| Gpm::alloc(api));
                g.as_mut().and_then(|g| g.sample(api, |s| call!(api.gpm_mig_sample_get, h, gi, s)))
            } else {
                None
            };
            if let Ok(list) = list_procs(m, api.compute_procs_v3, api.compute_procs_v2, api.compute_procs_v1, v3_stride) {
                procs.extend(list.into_iter().map(|p| (gi, p)));
            }
            out.push(MigInstance {
                gpu_instance: gi,
                compute_instance,
                profile: profile_of(&name),
                uuid: super::dev_string(api.uuid, m),
                sm_count,
                mem_used: has_mem.then_some(mem.used),
                mem_total: has_mem.then_some(mem.total),
                activity,
            });
        }
        // Partitions that were destroyed.
        self.gpm.retain(|gi, g| {
            let keep = out.iter().any(|m| m.gpu_instance == *gi);
            if let (false, Some(g)) = (keep, g) {
                g.free(api);
            }
            keep
        });
        out.sort_by_key(|m| m.gpu_instance);
        (out, procs)
    }
}

/// "NVIDIA A100-SXM4-40GB MIG 3g.20gb" -> "3g.20gb".
fn profile_of(name: &str) -> String {
    name.rsplit_once("MIG ").map(|(_, p)| p.trim().to_string()).unwrap_or_else(|| name.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn profile_names() {
        assert_eq!(super::profile_of("NVIDIA A100-SXM4-40GB MIG 3g.20gb"), "3g.20gb");
        assert_eq!(super::profile_of("NVIDIA H100 80GB HBM3 MIG 1g.10gb+me"), "1g.10gb+me");
        assert_eq!(super::profile_of("odd"), "odd");
    }
}
