//! NVLink: which links are up, what is on the other end, payload traffic per
//! link and link-level errors. Constants and layouts checked against NVIDIA's
//! own bindings (nvidia-ml-py).

use std::ffi::{c_uint, c_ulonglong};

use std::time::Instant;

use super::ffi::*;
use crate::model::NvLink;

struct Port {
    link: u32,
    peer_bus_id: Option<String>,
    peer_switch: bool,
    /// Previous payload counters (rx KiB, tx KiB) and when they were read.
    prev: Option<(u64, u64, Instant)>,
}

pub(super) struct Links {
    ports: Vec<Port>,
    /// Error counters answer on this device (stop asking once they don't).
    errors: bool,
    /// Throughput counters answer on this device.
    counters: bool,
}

impl Links {
    /// The device's NVLink ports, or None if it has none.
    pub(super) fn probe(api: &Api, h: nvmlDevice_t) -> Option<Links> {
        let mut ports = Vec::new();
        for link in 0..NVML_NVLINK_MAX_LINKS {
            let mut active: c_uint = 0;
            if call!(api.nvlink_state, h, link, &mut active).is_err() {
                continue;
            }
            let mut kind: c_uint = 0;
            let peer_switch = call!(api.nvlink_remote_type, h, link, &mut kind).is_ok() && kind == NVML_NVLINK_DEVICE_TYPE_SWITCH;
            let mut pci: nvmlPciInfo_t = unsafe { std::mem::zeroed() };
            let peer_bus_id = (!peer_switch && call!(api.nvlink_remote_pci_v2, h, link, &mut pci).is_ok())
                .then(|| cstr(&pci.bus_id))
                .filter(|s| !s.is_empty());
            ports.push(Port { link, peer_bus_id, peer_switch, prev: None });
        }
        if ports.is_empty() {
            return None;
        }
        let mut version: c_uint = 0;
        if call!(api.nvlink_version, h, ports[0].link, &mut version).is_ok() {
            log::debug!("NVLink: {} links, nvmlDeviceGetNvLinkVersion = {version}", ports.len());
        }
        Some(Links { ports, errors: true, counters: true })
    }

    pub(super) fn sample(&mut self, api: &Api, h: nvmlDevice_t) -> Vec<NvLink> {
        let now = Instant::now();
        // Payload counters for every link in one call: (rx, tx) per port.
        let mut fv: Vec<nvmlFieldValue_t> = Vec::new();
        if self.counters {
            for p in &self.ports {
                for id in [NVML_FI_DEV_NVLINK_THROUGHPUT_DATA_RX, NVML_FI_DEV_NVLINK_THROUGHPUT_DATA_TX] {
                    let mut v: nvmlFieldValue_t = unsafe { std::mem::zeroed() };
                    v.field_id = id;
                    v.scope_id = p.link;
                    fv.push(v);
                }
            }
            if call!(api.field_values, h, fv.len() as i32, fv.as_mut_ptr()).is_err() {
                fv.clear();
            }
            if !fv.is_empty() && fv.iter().all(|v| v.nvml_return != NVML_SUCCESS) {
                log::debug!("NVLink throughput counters not available (first: {})", fv[0].nvml_return);
                self.counters = false;
                fv.clear();
            }
        }
        let counter = |i: usize| fv.get(i).filter(|v| v.nvml_return == NVML_SUCCESS).map(field_u64);

        let mut out = Vec::with_capacity(self.ports.len());
        let mut any_errors = false;
        for (k, p) in self.ports.iter_mut().enumerate() {
            let mut active: c_uint = 0;
            let up = call!(api.nvlink_state, h, p.link, &mut active).is_ok() && active == 1;
            let (mut rx, mut tx) = (None, None);
            if let (Some(r), Some(t)) = (counter(2 * k), counter(2 * k + 1)) {
                if let Some((pr, pt, when)) = p.prev {
                    let dt = now.duration_since(when).as_secs_f64().max(1e-3);
                    rx = Some((r.saturating_sub(pr) as f64 * 1024.0 / dt) as u64);
                    tx = Some((t.saturating_sub(pt) as f64 * 1024.0 / dt) as u64);
                }
                p.prev = Some((r, t, now));
            }
            let errors = if self.errors {
                let mut total: Option<u64> = None;
                for c in [NVML_NVLINK_ERROR_DL_REPLAY, NVML_NVLINK_ERROR_DL_RECOVERY, NVML_NVLINK_ERROR_DL_CRC_FLIT] {
                    let mut v: c_ulonglong = 0;
                    if call!(api.nvlink_error_counter, h, p.link, c, &mut v).is_ok() {
                        total = Some(total.unwrap_or(0) + v);
                    }
                }
                any_errors |= total.is_some();
                total
            } else {
                None
            };
            out.push(NvLink { link: p.link, up, rx, tx, errors, peer_bus_id: p.peer_bus_id.clone(), peer_switch: p.peer_switch });
        }
        if !any_errors {
            self.errors = false;
        }
        out
    }
}

fn field_u64(v: &nvmlFieldValue_t) -> u64 {
    if v.value_type == NVML_VALUE_TYPE_DOUBLE { f64::from_bits(v.value) as u64 } else { v.value }
}

/// NVLink bandwidth per link and direction (bytes/s), from the common link speed.
pub(super) fn link_bandwidth(api: &Api, h: nvmlDevice_t) -> Option<f64> {
    let mut v: nvmlFieldValue_t = unsafe { std::mem::zeroed() };
    v.field_id = NVML_FI_DEV_NVLINK_SPEED_MBPS_COMMON;
    call!(api.field_values, h, 1, &mut v).ok()?;
    (v.nvml_return == NVML_SUCCESS).then(|| field_u64(&v)).filter(|&mbps| mbps > 0).map(|mbps| mbps as f64 * 1e6)
}

/// NVLink generation by GPU architecture (the version query's numbering
/// changed between drivers, so it is only logged).
pub(super) fn version_for(arch: Option<&str>) -> Option<u32> {
    Some(match arch? {
        "Pascal" => 1,
        "Volta" => 2,
        "Ampere" => 3,
        "Hopper" => 4,
        "Blackwell" => 5,
        _ => return None,
    })
}
