//! Headless exporter: `--serve [ADDR]` exposes Prometheus metrics at
//! `/metrics` and the full JSON snapshot at `/json`. No auth: binds to
//! localhost unless told otherwise.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::brand::{APP_NAME, APP_VERSION};
use crate::collector::{Collector, Sampler};
use crate::model::{Snapshot, SystemInfo};

pub fn run(collector: Collector, info: SystemInfo, interval: Duration, addr: &str) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr)?;
    eprintln!("{APP_NAME} {APP_VERSION}: serving http://{}/metrics and /json", listener.local_addr()?);
    let latest: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
    let sampler = Sampler::spawn(collector, interval);
    {
        let latest = latest.clone();
        std::thread::spawn(move || {
            while let Ok(s) = sampler.rx.recv() {
                *latest.lock().unwrap() = Some(s);
            }
        });
    }
    let info = Arc::new(info);
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
    let path = line.split_whitespace().nth(1).unwrap_or("/");
    let snap = latest.lock().unwrap().clone();
    let (status, ctype, body) = match (path, snap) {
        (_, None) => ("503 Service Unavailable", "text/plain", "warming up\n".to_string()),
        ("/metrics", Some(s)) => ("200 OK", "text/plain; version=0.0.4", prometheus(info, &s)),
        ("/json", Some(s)) => (
            "200 OK",
            "application/json",
            serde_json::to_string(&serde_json::json!({ "schema": 1, "system": info, "snapshot": s })).unwrap_or_default(),
        ),
        ("/", Some(_)) => ("200 OK", "text/plain", format!("{APP_NAME} {APP_VERSION}\n/metrics  Prometheus\n/json     snapshot\n")),
        _ => ("404 Not Found", "text/plain", "not found\n".to_string()),
    };
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

/// Prometheus text exposition. Metric names are namespaced by the app name.
pub fn prometheus(info: &SystemInfo, s: &Snapshot) -> String {
    let ns = APP_NAME.replace('-', "_");
    let mut o = String::new();
    let mut metric = |name: &str, help: &str, kind: &str, samples: Vec<(String, f64)>| {
        if samples.is_empty() {
            return;
        }
        let _ = writeln!(o, "# HELP {ns}_{name} {help}\n# TYPE {ns}_{name} {kind}");
        for (labels, v) in samples {
            let _ = writeln!(o, "{ns}_{name}{{{labels}}} {v}");
        }
    };
    let gl = |i: usize| {
        let g = &info.gpus[i];
        format!("gpu=\"{i}\",uuid=\"{}\"", esc(g.uuid.as_deref().unwrap_or("")))
    };
    let per = |f: &dyn Fn(&crate::model::GpuSample) -> Option<f64>| -> Vec<(String, f64)> {
        s.gpus.iter().enumerate().filter_map(|(i, g)| f(g).map(|v| (gl(i), v))).collect()
    };

    metric(
        "gpu_info",
        "GPU identity (always 1).",
        "gauge",
        info.gpus
            .iter()
            .map(|g| {
                (format!("{},name=\"{}\",vendor=\"{}\"", gl(g.index), esc(&g.name), g.vendor.label()), 1.0)
            })
            .collect(),
    );
    metric("gpu_utilization_percent", "GPU core utilization.", "gauge", per(&|g| g.util_gpu.map(f64::from)));
    metric("gpu_memory_bandwidth_utilization_percent", "Memory controller utilization.", "gauge", per(&|g| g.util_mem.map(f64::from)));
    metric("gpu_memory_used_bytes", "Device memory in use.", "gauge", per(&|g| g.mem_used.map(|v| v as f64)));
    metric("gpu_memory_total_bytes", "Device memory size.", "gauge", per(&|g| g.mem_total.map(|v| v as f64)));
    metric("gpu_temperature_celsius", "GPU core temperature.", "gauge", per(&|g| g.temp.map(f64::from)));
    metric("gpu_memory_temperature_celsius", "Memory temperature.", "gauge", per(&|g| g.temp_mem.map(f64::from)));
    metric("gpu_fan_percent", "Mean fan speed.", "gauge", per(&|g| g.fan().map(f64::from)));
    metric("gpu_power_watts", "Power draw.", "gauge", per(&|g| g.power));
    metric("gpu_power_limit_watts", "Enforced power limit.", "gauge", per(&|g| g.power_limit));
    metric("gpu_energy_joules_total", "Energy consumed since driver load.", "counter", per(&|g| g.energy_joules));
    metric("gpu_sm_clock_mhz", "SM clock.", "gauge", per(&|g| g.clock_sm.map(f64::from)));
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
                    .filter_map(move |(k, v)| v.map(|v| (format!("{},type=\"{k}\"", gl(i)), v as f64)))
            })
            .collect(),
    );
    metric(
        "gpu_clock_limit_reason",
        "Active clock limiting reasons (1 = active).",
        "gauge",
        s.gpus
            .iter()
            .enumerate()
            .flat_map(|(i, g)| g.throttle.iter().map(move |r| (format!("{},reason=\"{}\"", gl(i), esc(r)), 1.0)))
            .collect(),
    );
    let pl = |p: &crate::model::ProcRow| {
        format!(
            "{},pid=\"{}\",name=\"{}\",user=\"{}\"",
            gl(p.gpu),
            p.pid,
            esc(&p.name),
            esc(p.user.as_deref().unwrap_or(""))
        )
    };
    metric(
        "process_gpu_memory_bytes",
        "GPU memory used by a process.",
        "gauge",
        s.processes.iter().filter_map(|p| p.gpu_mem.map(|m| (pl(p), m as f64))).collect(),
    );
    metric(
        "process_gpu_utilization_percent",
        "SM utilization attributed to a process.",
        "gauge",
        s.processes.iter().filter_map(|p| p.sm_util.map(|v| (pl(p), v as f64))).collect(),
    );
    metric("host_cpu_utilization_percent", "Host CPU utilization.", "gauge", vec![(String::new(), s.host.cpu as f64)]);
    metric("host_memory_used_bytes", "Host memory in use.", "gauge", vec![(String::new(), s.host.mem_used as f64)]);
    metric("host_memory_total_bytes", "Host memory size.", "gauge", vec![(String::new(), s.host.mem_total as f64)]);
    o
}
