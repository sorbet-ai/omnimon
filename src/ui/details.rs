//! Details view: everything known about the selected GPU.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::App;
use super::draw::{block, draw_no_gpus, s};
use crate::fmt;
use crate::model::SpillLevel;

pub(super) fn draw(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let n = app.info.gpus.len();
    if n == 0 {
        draw_no_gpus(f, app, area);
        return;
    }
    let i = app.sel_gpu.min(n - 1);
    let (Some(g), Some(sm)) = (app.gpu(i), app.sample(i)) else { return };
    let na = || "N/A".to_string();
    let o = |v: Option<String>| v.unwrap_or_else(na);
    let b = |v: Option<bool>| v.map(|x| if x { "enabled" } else { "disabled" }.to_string()).unwrap_or_else(na);
    let backend = app.info.backend_of(i);

    let device: Vec<(&str, String)> = vec![
        ("Name", g.name.clone()),
        ("Vendor", g.vendor.label().into()),
        ("Architecture", o(g.architecture.clone())),
        ("Chip", o(g.chip.clone())),
        ("Compute capability", o(g.compute_capability.clone())),
        ("Cores", o(g.cores.map(|c| c.to_string()))),
        ("UUID", o(g.uuid.clone())),
        ("PCI bus", o(g.pci_bus_id.clone())),
        ("Serial", o(g.serial.clone())),
        ("VBIOS", o(g.vbios.clone())),
        ("Data source", o(backend.map(|b| b.name.clone()))),
        ("Driver", o(backend.and_then(|b| b.driver_version.clone()))),
        (
            "Runtime",
            o(backend.and_then(|b| Some(format!("{} {}", b.runtime_name.as_deref().unwrap_or(""), b.runtime_version.as_deref()?)))),
        ),
        ("API version", o(backend.and_then(|b| b.api_version.clone()))),
        ("Library", o(backend.and_then(|b| b.library.clone()))),
        ("Memory bandwidth", o(g.memory_bandwidth.map(|b| format!("{:.0} GB/s", b / 1e9)))),
        ("Host link", o(g.pcie_bandwidth().map(|b| format!("{:.0} GB/s", b / 1e9)))),
        ("Persistence mode", b(g.persistence_mode)),
        ("Display active", b(g.display_active)),
        ("Compute mode", o(g.compute_mode.clone())),
        ("MIG", b(g.mig_enabled)),
        ("Fans", g.num_fans.to_string()),
        (
            "PCIe max",
            match (g.pcie_max_gen, g.pcie_max_width) {
                (Some(a), Some(w)) => format!("Gen{a} ×{w}"),
                _ => na(),
            },
        ),
        (
            "Power limits",
            match (g.power_limit_min, g.power_limit_default, g.power_limit_max) {
                (Some(a), Some(d), Some(m)) => format!("{a:.0} W min · {d:.0} W default · {m:.0} W max"),
                _ => o(g.power_limit_default.map(|d| format!("{d:.0} W default"))),
            },
        ),
        (
            "Temp thresholds",
            format!(
                "slowdown {} · shutdown {} · max op {}",
                fmt::opt(g.temp_slowdown, "°C"),
                fmt::opt(g.temp_shutdown, "°C"),
                fmt::opt(g.temp_max_operating, "°C")
            ),
        ),
        (
            "Max clocks",
            format!(
                "gfx {} · sm {} · mem {} · video {}",
                fmt::opt(g.max_clock_graphics, ""),
                fmt::opt(g.max_clock_sm, ""),
                fmt::opt(g.max_clock_mem, ""),
                fmt::opt(g.max_clock_video, "")
            ),
        ),
    ];
    let live: Vec<(&str, String)> = vec![
        ("Utilization", format!("gpu {} · memory bw {}", fmt::pct(sm.util_gpu), fmt::pct(sm.util_mem))),
        (
            "Memory",
            match (sm.mem_used, sm.mem_total) {
                (Some(u), Some(tot)) => format!(
                    "{} used / {} total{}",
                    fmt::bytes(u),
                    fmt::bytes(tot),
                    sm.mem_reserved.map(|r| format!(" ({} reserved)", fmt::bytes(r))).unwrap_or_default()
                ),
                _ => na(),
            },
        ),
        ("In system memory", o(sm.mem_shared.map(fmt::bytes))),
        (
            "Spill",
            match sm.spill.level {
                SpillLevel::None => "none".into(),
                _ => format!(
                    "{}{}",
                    sm.spill.reason,
                    sm.spill.slowdown.map(|x| format!(" · ≈{x:.1}× slower when bandwidth-bound")).unwrap_or_default()
                ),
            },
        ),
        (
            "BAR1",
            match (sm.bar1_used, sm.bar1_total) {
                (Some(u), Some(tot)) => format!("{} / {}", fmt::bytes(u), fmt::bytes(tot)),
                _ => na(),
            },
        ),
        ("Temperature", format!("{}{}", fmt::opt(sm.temp, "°C"), sm.temp_mem.map(|m| format!(" · memory {m}°C")).unwrap_or_default())),
        ("Fan", if sm.fans.is_empty() { na() } else { sm.fans.iter().map(|f| format!("{f}%")).collect::<Vec<_>>().join(" · ") }),
        (
            "Power",
            match (sm.power, sm.power_limit) {
                (Some(p), Some(l)) => format!("{p:.1} W / {l:.0} W limit"),
                (Some(p), None) => format!("{p:.1} W"),
                _ => na(),
            },
        ),
        ("Energy", o(sm.energy_joules.map(|j| format!("{:.3} kWh since driver load", j / 3.6e6)))),
        ("This session", {
            let j = app.hist.gpus.get(i).map(|h| h.energy).unwrap_or(0.0);
            format!("{:.1} Wh{}", j / 3600.0, app.price_per_kwh.map(|p| format!(" · ≈{:.3} at {p}/kWh", j / 3.6e6 * p)).unwrap_or_default())
        }),
        ("Performance state", o(sm.pstate.map(|p| format!("P{p}")))),
        (
            "Clocks",
            format!(
                "gfx {} · sm {} · mem {} · video {} MHz",
                fmt::opt(sm.clock_graphics, ""),
                fmt::opt(sm.clock_sm, ""),
                fmt::opt(sm.clock_mem, ""),
                fmt::opt(sm.clock_video, "")
            ),
        ),
        (
            "PCIe link",
            match (sm.pcie_gen, sm.pcie_width) {
                (Some(a), Some(w)) => format!("Gen{a} ×{w}"),
                _ => na(),
            },
        ),
        (
            "PCIe throughput",
            match (sm.pcie_rx, sm.pcie_tx) {
                (Some(r), Some(x)) => format!("rx {} · tx {}", fmt::rate(r), fmt::rate(x)),
                _ => na(),
            },
        ),
        ("Encoder / decoder", format!("enc {} · dec {}", fmt::pct(sm.enc_util), fmt::pct(sm.dec_util))),
        ("JPEG / OFA", format!("jpg {} · ofa {}", fmt::pct(sm.jpg_util), fmt::pct(sm.ofa_util))),
        (
            "Clock limiters",
            if sm.throttle.is_empty() { "none".into() } else { sm.throttle.iter().map(|r| r.label()).collect::<Vec<_>>().join(", ") },
        ),
        (
            "ECC errors (volatile)",
            match (sm.ecc_corrected, sm.ecc_uncorrected) {
                (Some(c), Some(u)) => format!("{c} corrected · {u} uncorrected"),
                _ => na(),
            },
        ),
        ("Status", sm.error.clone().unwrap_or_else(|| "ok".into())),
    ];

    let [l, r] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(area);
    let kv = |items: Vec<(&str, String)>| -> Vec<Line<'static>> {
        items
            .into_iter()
            .map(|(k, v)| {
                let vc = if v == "N/A" { t.faint } else { t.fg };
                Line::from(vec![s(format!("{k:<22}"), t.dim), s(v, vc)])
            })
            .collect()
    };
    let title = |x: &str| Line::from(vec![s(format!(" {x} "), t.title).bold(), s(format!("GPU {i} "), t.dim)]);
    f.render_widget(Paragraph::new(kv(device)).wrap(Wrap { trim: false }).block(block(t, false).title_top(title("Device"))), l);
    // Diagnosis above the live values.
    let recent = app.hist.gpus.get(i).map(|h| {
        let w: Vec<f64> = h.util.window(60, app.hist.back).iter().flatten().map(|v| v * 100.0).collect();
        crate::diagnose::Recent { util_avg: (!w.is_empty()).then(|| w.iter().sum::<f64>() / w.len() as f64), samples: w.len() }
    });
    let procs: Vec<&crate::model::ProcRow> = app.procs().iter().filter(|p| p.gpu == i).collect();
    let findings = crate::diagnose::diagnose(g, sm, &procs, recent.unwrap_or_default());
    let diag: Vec<Line> = if findings.is_empty() {
        vec![Line::from(s("nothing wrong found", t.ok))]
    } else {
        findings
            .iter()
            .map(|f| {
                let c = match f.severity {
                    crate::events::Severity::Crit => t.crit,
                    crate::events::Severity::Warn => t.warn,
                    crate::events::Severity::Info => t.accent,
                };
                Line::from(vec![s("■ ", c), s(format!("{}: ", f.title), c).bold(), s(f.detail.as_str(), t.fg)])
            })
            .collect()
    };
    let diag_h = (diag.len() as u16 * 2 + 2).min(r.height / 2).max(3);
    let [dr, lr] = Layout::vertical([Constraint::Length(diag_h), Constraint::Fill(1)]).areas(r);
    f.render_widget(
        Paragraph::new(diag).wrap(Wrap { trim: false }).block(block(t, !findings.is_empty()).title_top(title("Diagnosis"))),
        dr,
    );
    f.render_widget(Paragraph::new(kv(live)).wrap(Wrap { trim: false }).block(block(t, false).title_top(title("Live"))), lr);
}
