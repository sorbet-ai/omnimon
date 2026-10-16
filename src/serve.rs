//! Headless exporter: `--serve [ADDR]` exposes Prometheus metrics at
//! `/metrics` and the full JSON snapshot at `/json`. No auth: binds to
//! localhost unless told otherwise.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::brand::{APP_NAME, APP_VERSION, METRIC_NS};
use crate::collector::{Collector, Sampler};
use crate::model::{GpuSample, ProcRow, Snapshot, SpillLevel, SystemInfo};
use crate::report::JsonOut;

pub fn run(collector: Collector, info: SystemInfo, interval: Duration, addr: &str, cfg: &crate::config::Config) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr)?;
    eprintln!("{APP_NAME} {APP_VERSION}: serving http://{}/metrics and /json", listener.local_addr()?);
    let latest: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
    let sampler = Sampler::spawn(collector, interval);
    {
        let latest = latest.clone();
        let mut alerts = crate::alerts::Alerts::new(cfg.alert_command.clone(), cfg.alert_min);
        std::thread::spawn(move || {
            while let Ok(s) = sampler.rx.recv() {
                alerts.handle(&s.events);
                *latest.lock().unwrap() = Some(s);
            }
        });
    }
    let info = Arc::new(info);
    // One short-lived thread per request: fine for a localhost scrape target.
    for stream in listener.incoming().flatten() {
        let (latest, info) = (latest.clone(), info.clone());
        std::thread::spawn(move || {
            let _ = handle(stream, &latest, &info);
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, latest: &Mutex<Option<Snapshot>>, info: &SystemInfo) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let target = line.split_whitespace().nth(1).unwrap_or("/");
    let path = target.split('?').next().unwrap_or("/");
    let snap = latest.lock().unwrap().clone();
    let (status, ctype, body) = match (path, snap) {
        (_, None) => ("503 Service Unavailable", "text/plain", "warming up\n".to_string()),
        ("/metrics", Some(s)) => ("200 OK", "text/plain; version=0.0.4", prometheus(info, &s)),
        ("/json", Some(s)) => ("200 OK", "application/json", serde_json::to_string(&JsonOut::new(info, &s)).unwrap_or_default()),
        ("/", Some(_)) => ("200 OK", "text/plain", format!("{APP_NAME} {APP_VERSION}\n/metrics  Prometheus\n/json     snapshot\n")),
        _ => ("404 Not Found", "text/plain", "not found\n".to_string()),
    };
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

/// Prometheus text exposition. Metric names are prefixed with [`METRIC_NS`].
pub fn prometheus(info: &SystemInfo, s: &Snapshot) -> String {
    let ns = METRIC_NS.replace('-', "_");
    let mut o = String::new();
    let mut metric = |name: &str, help: &str, kind: &str, samples: Vec<(String, f64)>| {
        let samples: Vec<_> = samples.into_iter().filter(|(_, v)| v.is_finite()).collect();
        if samples.is_empty() {
            return;
        }
        let _ = writeln!(o, "# HELP {ns}_{name} {help}\n# TYPE {ns}_{name} {kind}");
        for (labels, v) in samples {
            let _ = writeln!(o, "{ns}_{name}{{{labels}}} {v}");
        }
    };
    let gl = |i: usize| -> Option<String> {
        let g = info.gpus.get(i)?;
        Some(format!("gpu=\"{i}\",uuid=\"{}\"", esc(&g.stable_id())))
    };
    let per = |f: &dyn Fn(&GpuSample) -> Option<f64>| -> Vec<(String, f64)> {
        s.gpus.iter().enumerate().filter_map(|(i, g)| Some((gl(i)?, f(g)?))).collect()
    };

    metric(
        "gpu_info",
        "GPU identity (always 1).",
        "gauge",
        info.gpus
            .iter()
            .filter_map(|g| Some((format!("{},name=\"{}\",vendor=\"{}\"", gl(g.index)?, esc(&g.name), g.vendor.label()), 1.0)))
            .collect(),
    );
    metric("gpu_utilization_percent", "GPU core utilization.", "gauge", per(&|g| g.util_gpu.map(f64::from)));
    metric("gpu_memory_bandwidth_utilization_percent", "Memory controller utilization.", "gauge", per(&|g| g.util_mem.map(f64::from)));
    let act = |f: fn(&crate::model::Activity) -> Option<f32>| per(&|g| g.activity.as_ref().and_then(f).map(f64::from));
    metric("gpu_sm_active_percent", "Share of time SMs had work (GPM).", "gauge", act(|a| a.sm_active));
    metric("gpu_sm_occupancy_percent", "Resident warps vs maximum (GPM).", "gauge", act(|a| a.sm_occupancy));
    metric("gpu_tensor_active_percent", "Tensor core activity, any precision (GPM).", "gauge", act(|a| a.tensor));
    metric("gpu_dram_bandwidth_percent", "DRAM bandwidth as a share of peak (GPM).", "gauge", act(|a| a.dram_bw));
    metric(
        "gpu_nvlink_rx_bytes_per_second",
        "NVLink receive rate, all links (GPM).",
        "gauge",
        per(&|g| g.activity.as_ref()?.nvlink_rx.map(|v| v as f64)),
    );
    metric(
        "gpu_nvlink_tx_bytes_per_second",
        "NVLink transmit rate, all links (GPM).",
        "gauge",
        per(&|g| g.activity.as_ref()?.nvlink_tx.map(|v| v as f64)),
    );
    metric("gpu_memory_used_bytes", "Device memory in use.", "gauge", per(&|g| g.mem_used.map(|v| v as f64)));
    metric("gpu_memory_total_bytes", "Device memory size.", "gauge", per(&|g| g.mem_total.map(|v| v as f64)));
    metric("gpu_memory_shared_bytes", "GPU allocations in system memory.", "gauge", per(&|g| g.mem_shared.map(|v| v as f64)));
    metric(
        "gpu_spilled_bytes",
        "GPU allocations measured as spilled into system memory.",
        "gauge",
        per(&|g| Some(g.spill.bytes.unwrap_or(0) as f64)),
    );
    metric(
        "gpu_spill_state",
        "0 = ok, 1 = VRAM pressure, 2 = spilling.",
        "gauge",
        per(&|g| {
            Some(match g.spill.level {
                SpillLevel::None => 0.0,
                SpillLevel::Pressure => 1.0,
                SpillLevel::Spilling => 2.0,
            })
        }),
    );
    metric("gpu_temperature_celsius", "GPU core temperature.", "gauge", per(&|g| g.temp.map(f64::from)));
    metric("gpu_memory_temperature_celsius", "Memory temperature.", "gauge", per(&|g| g.temp_mem.map(f64::from)));
    metric("gpu_fan_percent", "Mean fan speed.", "gauge", per(&|g| g.fan().map(f64::from)));
    metric("gpu_power_watts", "Power draw.", "gauge", per(&|g| g.power));
    metric("gpu_power_limit_watts", "Enforced power limit.", "gauge", per(&|g| g.power_limit));
    metric("gpu_energy_joules_total", "Energy consumed since driver load.", "counter", per(&|g| g.energy_joules));
    metric("gpu_sm_clock_mhz", "SM clock.", "gauge", per(&|g| g.clock_sm.or(g.clock_graphics).map(f64::from)));
    metric("gpu_memory_clock_mhz", "Memory clock.", "gauge", per(&|g| g.clock_mem.map(f64::from)));
    metric("gpu_pstate", "Performance state (0 = max).", "gauge", per(&|g| g.pstate.map(f64::from)));
    metric("gpu_pcie_rx_bytes_per_second", "PCIe receive throughput.", "gauge", per(&|g| g.pcie_rx.map(|v| v as f64)));
    metric("gpu_pcie_tx_bytes_per_second", "PCIe transmit throughput.", "gauge", per(&|g| g.pcie_tx.map(|v| v as f64)));
    metric("gpu_encoder_utilization_percent", "Video encoder utilization.", "gauge", per(&|g| g.enc_util.map(f64::from)));
    metric("gpu_decoder_utilization_percent", "Video decoder utilization.", "gauge", per(&|g| g.dec_util.map(f64::from)));
    metric(
        "gpu_ecc_errors",
        "Volatile ECC error counts.",
        "gauge",
        s.gpus
            .iter()
            .enumerate()
            .flat_map(|(i, g)| {
                [("corrected", g.ecc_corrected), ("uncorrected", g.ecc_uncorrected)]
                    .into_iter()
                    .filter_map(move |(k, v)| Some((format!("{},type=\"{k}\"", gl(i)?), v? as f64)))
            })
            .collect(),
    );
    let links = |f: fn(&crate::model::NvLink) -> Option<f64>| -> Vec<(String, f64)> {
        s.gpus
            .iter()
            .enumerate()
            .flat_map(|(i, g)| g.nvlink.iter().filter_map(move |l| Some((format!("{},link=\"{}\"", gl(i)?, l.link), f(l)?))))
            .collect()
    };
    metric("gpu_nvlink_up", "NVLink link state (1 = up).", "gauge", links(|l| Some(if l.up { 1.0 } else { 0.0 })));
    metric("gpu_nvlink_link_rx_bytes_per_second", "NVLink payload received, per link.", "gauge", links(|l| l.rx.map(|v| v as f64)));
    metric("gpu_nvlink_link_tx_bytes_per_second", "NVLink payload sent, per link.", "gauge", links(|l| l.tx.map(|v| v as f64)));
    metric(
        "gpu_nvlink_errors",
        "NVLink replay + recovery + CRC errors since driver load.",
        "counter",
        links(|l| l.errors.map(|v| v as f64)),
    );
    let parts = |f: fn(&crate::model::MigInstance) -> Option<f64>| -> Vec<(String, f64)> {
        s.gpus
            .iter()
            .enumerate()
            .flat_map(|(i, g)| {
                g.mig.iter().filter_map(move |m| {
                    Some((format!("{},gpu_instance=\"{}\",profile=\"{}\"", gl(i)?, m.gpu_instance, esc(&m.profile)), f(m)?))
                })
            })
            .collect()
    };
    metric("mig_memory_used_bytes", "Memory used in a MIG partition.", "gauge", parts(|m| m.mem_used.map(|v| v as f64)));
    metric("mig_memory_total_bytes", "Memory of a MIG partition.", "gauge", parts(|m| m.mem_total.map(|v| v as f64)));
    metric(
        "mig_sm_active_percent",
        "Share of time a MIG partition's SMs had work (GPM).",
        "gauge",
        parts(|m| m.activity.as_ref()?.sm_active.map(f64::from)),
    );
    metric(
        "gpu_clock_limit_reason",
        "Active clock limiting reasons (1 = active).",
        "gauge",
        s.gpus
            .iter()
            .enumerate()
            .flat_map(|(i, g)| g.throttle.iter().filter_map(move |r| Some((format!("{},reason=\"{}\"", gl(i)?, esc(r.label())), 1.0))))
            .collect(),
    );
    let pl = |p: &ProcRow| -> Option<String> {
        Some(format!("{},pid=\"{}\",name=\"{}\",user=\"{}\"", gl(p.gpu)?, p.pid(), esc(&p.name), esc(p.user.as_deref().unwrap_or(""))))
    };
    let per_proc =
        |f: &dyn Fn(&ProcRow) -> Option<f64>| -> Vec<(String, f64)> { s.processes.iter().filter_map(|p| Some((pl(p)?, f(p)?))).collect() };
    metric("process_gpu_memory_bytes", "GPU memory used by a process.", "gauge", per_proc(&|p| p.usage.gpu_mem.map(|m| m as f64)));
    metric(
        "process_gpu_shared_memory_bytes",
        "A process's GPU allocations in system memory.",
        "gauge",
        per_proc(&|p| p.usage.gpu_mem_shared.map(|m| m as f64)),
    );
    metric(
        "process_gpu_utilization_percent",
        "SM utilization attributed to a process.",
        "gauge",
        per_proc(&|p| p.usage.sm_util.map(f64::from)),
    );
    metric("host_cpu_utilization_percent", "Host CPU utilization.", "gauge", vec![(String::new(), s.host.cpu as f64)]);
    metric("host_memory_used_bytes", "Host memory in use.", "gauge", vec![(String::new(), s.host.mem_used as f64)]);
    metric("host_memory_total_bytes", "Host memory size.", "gauge", vec![(String::new(), s.host.mem_total as f64)]);
    let host = |v: Option<u64>| v.map(|v| vec![(String::new(), v as f64)]).unwrap_or_default();
    metric("host_network_receive_bytes_per_second", "Network receive throughput.", "gauge", host(s.host.net_rx));
    metric("host_network_transmit_bytes_per_second", "Network transmit throughput.", "gauge", host(s.host.net_tx));
    metric("host_disk_read_bytes_per_second", "Disk read throughput.", "gauge", host(s.host.disk_read));
    metric("host_disk_write_bytes_per_second", "Disk write throughput.", "gauge", host(s.host.disk_write));
    metric(
        "host_cpu_temperature_celsius",
        "Hottest CPU sensor.",
        "gauge",
        s.host.cpu_temp.map(|v| vec![(String::new(), v as f64)]).unwrap_or_default(),
    );
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GpuInfo, GpuProcess, PidMapping, Throttle};

    fn fixture() -> (SystemInfo, Snapshot) {
        let mut info = SystemInfo::default();
        info.gpus.push(GpuInfo { name: "Weird \"GPU\"\nname".into(), uuid: Some("GPU-1".into()), ..Default::default() });
        let snap = Snapshot {
            gpus: vec![
                GpuSample { util_gpu: Some(50.0), power: Some(f64::NAN), throttle: vec![Throttle::PowerCap], ..Default::default() },
                // A sample without a matching GpuInfo must not panic.
                GpuSample { util_gpu: Some(1.0), ..Default::default() },
            ],
            processes: vec![ProcRow {
                gpu: 0,
                usage: GpuProcess { pid: 7, gpu_mem: Some(1024), ..Default::default() },
                local_pid: Some(7),
                mapping: PidMapping::Direct,
                name: "back\\slash".into(),
                cmdline: String::new(),
                user: None,
                cpu: None,
                rss: None,
                runtime: None,
                spilling: false,
                container: None,
            }],
            ..Default::default()
        };
        (info, snap)
    }

    #[test]
    fn exposition_is_well_formed() {
        let (info, snap) = fixture();
        let out = prometheus(&info, &snap);
        assert!(out.contains(r#"name="Weird \"GPU\"\nname""#));
        assert!(out.contains(r#"name="back\\slash""#));
        assert!(out.contains(r#"reason="power cap""#));
        assert!(!out.lines().any(|l| l.ends_with("NaN") || l.ends_with("inf")));
        assert!(!out.contains("gpu=\"1\""), "sample without GpuInfo leaked");
        // Every sample line belongs to the most recent TYPE line.
        let mut current = String::new();
        for line in out.lines() {
            if let Some(rest) = line.strip_prefix("# TYPE ") {
                current = rest.split(' ').next().unwrap().to_string();
            } else if !line.starts_with('#') {
                assert!(line.starts_with(&current), "{line} outside {current}");
            }
        }
    }

    #[test]
    fn json_shape() {
        let (info, snap) = fixture();
        let v: serde_json::Value = serde_json::to_value(JsonOut::new(&info, &snap)).unwrap();
        assert_eq!(v["schema"], crate::report::SCHEMA);
        assert!(v["system"]["gpus"].is_array());
        // Processes are flattened: driver fields sit beside host fields.
        let p = &v["snapshot"]["processes"][0];
        assert_eq!(p["pid"], 7);
        assert_eq!(p["gpu_mem"], 1024);
        assert_eq!(p["mapping"], "direct");
        // Backend-internal per-GPU lists are not part of the output.
        assert!(v["snapshot"]["gpus"][0].get("processes").is_none());
    }
}
