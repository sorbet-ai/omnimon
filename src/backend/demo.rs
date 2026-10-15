//! Simulated GPUs, for screenshots, UI work and machines without a GPU.

use std::time::Instant;

use super::Backend;
use crate::model::{BackendInfo, GpuInfo, GpuProcess, GpuSample, ProcKind, Vendor};

const GIB: u64 = 1 << 30;

struct Model {
    name: &'static str,
    mem_gib: u64,
    tdp: f64,
    clk: u32,
    mem_clk: u32,
    arch: &'static str,
}

const MODELS: &[Model] = &[
    Model { name: "NVIDIA H100 80GB HBM3", mem_gib: 80, tdp: 700.0, clk: 1980, mem_clk: 2619, arch: "Hopper" },
    Model { name: "NVIDIA GeForce RTX 4090", mem_gib: 24, tdp: 450.0, clk: 3105, mem_clk: 10501, arch: "Ada Lovelace" },
    Model { name: "NVIDIA A100-SXM4-40GB", mem_gib: 40, tdp: 400.0, clk: 1410, mem_clk: 1215, arch: "Ampere" },
    Model { name: "NVIDIA RTX 6000 Ada Generation", mem_gib: 48, tdp: 300.0, clk: 2505, mem_clk: 10001, arch: "Ada Lovelace" },
];

const JOBS: &[&str] = &["python train.py", "vllm serve", "ollama", "torchrun", "blender", "jupyter-kernel", "llama-server", "ffmpeg"];

pub struct Demo {
    gpus: Vec<GpuInfo>,
    start: Instant,
    rng: u64,
    load: Vec<f64>,
}

impl Demo {
    pub fn new(n: usize) -> Self {
        let n = n.clamp(1, 64);
        let gpus = (0..n)
            .map(|i| {
                let m = &MODELS[if n >= 4 { 0 } else { i % MODELS.len() }];
                let mut g = GpuInfo::new(i, Vendor::Demo, m.name.to_string());
                g.uuid = Some(format!("GPU-demo-{i:04}-0000-0000-000000000000"));
                g.pci_bus_id = Some(format!("00000000:{:02X}:00.0", 0x18 + i * 0x10));
                g.memory_total = Some(m.mem_gib * GIB);
                g.power_limit_default = Some(m.tdp);
                g.power_limit_max = Some(m.tdp);
                g.max_clock_graphics = Some(m.clk);
                g.max_clock_sm = Some(m.clk);
                g.max_clock_mem = Some(m.mem_clk);
                g.pcie_max_gen = Some(if m.arch == "Hopper" { 5 } else { 4 });
                g.pcie_max_width = Some(16);
                g.temp_slowdown = Some(87);
                g.temp_shutdown = Some(92);
                g.architecture = Some(m.arch.into());
                g.num_fans = if m.arch == "Hopper" || m.name.contains("A100") { 0 } else { 1 };
                g.compute_mode = Some("Default".into());
                g
            })
            .collect();
        Demo { gpus, start: Instant::now(), rng: 0x9E3779B97F4A7C15, load: vec![0.5; n] }
    }

    fn rand(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }
}

impl Backend for Demo {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            name: "Demo".into(),
            library: None,
            driver_version: Some("999.99".into()),
            api_version: None,
            runtime_version: Some("13.0".into()),
        }
    }

    fn gpus(&self) -> Vec<GpuInfo> {
        self.gpus.clone()
    }

    fn sample(&mut self) -> Vec<GpuSample> {
        let t = self.start.elapsed().as_secs_f64();
        let mut out = Vec::with_capacity(self.gpus.len());
        for i in 0..self.gpus.len() {
            let g = self.gpus[i].clone();
            let phase = i as f64 * 1.7;
            // Training-like: mostly busy with periodic eval dips; one GPU idles.
            let target = if i == 1 && self.gpus.len() > 2 {
                0.02
            } else {
                let wave = 0.75 + 0.2 * (t / 7.0 + phase).sin();
                if ((t + phase * 5.0) % 40.0) < 5.0 { 0.15 } else { wave }
            };
            let r = self.rand();
            self.load[i] += (target - self.load[i]) * 0.35 + (r - 0.5) * 0.12;
            self.load[i] = self.load[i].clamp(0.0, 1.0);
            let l = self.load[i];

            let total = g.memory_total.unwrap();
            let mem_frac = if i == 1 && self.gpus.len() > 2 { 0.01 } else { 0.55 + 0.25 * (t / 60.0 + phase).sin().abs() };
            let tdp = g.power_limit_default.unwrap();
            let temp = (34.0 + 48.0 * l + 2.0 * self.rand()) as u32;

            let mut procs = Vec::new();
            if !(i == 1 && self.gpus.len() > 2) {
                let n = 1 + (i % 3);
                for k in 0..n {
                    let share = if k == 0 { 0.7 } else { 0.3 / (n - 1) as f64 };
                    procs.push(GpuProcess {
                        pid: 41000 + (i * 97 + k * 13) as u32,
                        pid_is_local: false,
                        kind: if k == 2 { ProcKind::Graphics } else { ProcKind::Compute },
                        gpu_mem: Some((total as f64 * mem_frac * share) as u64),
                        sm_util: Some((l * 100.0 * share).round() as f32),
                        mem_util: Some((l * 60.0 * share).round() as f32),
                        enc_util: Some(0.0),
                        dec_util: Some(if JOBS[(i + k) % JOBS.len()] == "ffmpeg" { 35.0 } else { 0.0 }),
                        gpu_instance: None,
                        name: Some(JOBS[(i * 3 + k) % JOBS.len()].to_string()),
                    });
                }
            }

            let mut throttle = Vec::new();
            if l < 0.05 {
                throttle.push("idle".into());
            } else if l > 0.85 {
                throttle.push("power cap".into());
            }
            out.push(GpuSample {
                util_gpu: Some((l * 100.0).round() as f32),
                util_mem: Some((l * 55.0).round() as f32),
                mem_used: Some((total as f64 * mem_frac) as u64),
                mem_total: Some(total),
                mem_reserved: Some(total / 100),
                temp: Some(temp),
                temp_mem: if g.num_fans == 0 { Some(temp + 6) } else { None },
                fans: if g.num_fans > 0 { vec![(30.0 + 50.0 * l) as u32] } else { vec![] },
                power: Some(tdp * (0.12 + 0.85 * l)),
                power_limit: Some(tdp),
                energy_joules: Some(t * tdp * 0.6),
                clock_graphics: Some((g.max_clock_graphics.unwrap() as f64 * (0.3 + 0.65 * l)) as u32),
                clock_sm: Some((g.max_clock_sm.unwrap() as f64 * (0.3 + 0.65 * l)) as u32),
                clock_mem: g.max_clock_mem,
                clock_video: Some(1200),
                pstate: Some(if l < 0.05 { 8 } else { 0 }),
                pcie_gen: Some(if l < 0.05 { 1 } else { g.pcie_max_gen.unwrap() }),
                pcie_width: Some(16),
                pcie_tx: Some((l * 900e6 * self.rand()) as u64),
                pcie_rx: Some((l * 4e9 * self.rand()) as u64),
                enc_util: Some(0.0),
                dec_util: Some(0.0),
                jpg_util: None,
                ofa_util: None,
                throttle,
                ecc_corrected: Some(0),
                ecc_uncorrected: Some(0),
                bar1_used: Some(GIB / 8),
                bar1_total: Some(total),
                processes: procs,
                error: None,
            });
        }
        out
    }
}
