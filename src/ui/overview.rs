//! Overview: GPU cards (or compact rows), host strip, process table.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::draw::{block, draw_no_gpus, s, severity, severity_color, spill_text, temp_color};
use super::gfx::Series;
use super::theme::Gradient;
use super::widgets::{GradBar, Spark};
use super::{App, Hit};
use crate::fmt;
use crate::model::{GpuInfo, GpuSample, SpillLevel};

pub(super) fn draw(f: &mut Frame, app: &App, area: Rect, hit: &mut Hit) {
    let n = app.info.gpus.len();
    if n == 0 {
        draw_no_gpus(f, app, area);
        return;
    }
    let show = |p: super::Panel| !app.hidden.contains(&p);
    let host_h = if show(super::Panel::Host) { 8u16 } else { 0 };
    let procs_on = show(super::Panel::Procs);
    // The process list gets what it needs (within reason); GPU cards grow
    // into the rest, up to a cap, so a lone GPU gets a big chart.
    // Cards are as tall as their bars; the process list gets the rest.
    let needs: Vec<u16> = (0..n).map(|i| card_height(app, i)).collect();
    let avail = area.height.saturating_sub(host_h + if procs_on { 7 } else { 0 });
    let fair = avail / n as u16;
    let heights: Vec<u16> = if needs.iter().sum::<u16>() <= avail { needs.clone() } else { needs.iter().map(|&h| h.min(fair)).collect() };
    let full = heights.iter().all(|&h| h >= 6);
    let gpu_h = if !show(super::Panel::Cards) {
        0
    } else if full {
        heights.iter().sum()
    } else {
        (n as u16 + 2).min(area.height.saturating_sub(host_h + 7).max(3))
    };
    let rest = if procs_on { Constraint::Min(3) } else { Constraint::Length(0) };
    let [gpus, host, procs] = Layout::vertical([Constraint::Length(gpu_h), Constraint::Length(host_h), rest]).areas(area);

    if gpu_h > 0 {
        if full {
            let rows = Layout::vertical(heights.iter().map(|&h| Constraint::Length(h))).split(gpus);
            for (i, r) in rows.iter().enumerate() {
                card(f, app, i, *r, hit);
            }
        } else {
            compact(f, app, gpus, hit);
        }
    }
    if host_h > 0 {
        host_panel(f, app, host);
    }
    if procs_on {
        super::procs::draw(f, app, procs, hit);
    }
}

fn title<'a>(app: &App, g: &GpuInfo, sm: &GpuSample, compact: bool) -> Line<'a> {
    let t = app.theme();
    let mut v = vec![
        Span::raw(" "),
        Span::styled(
            format!("{}", g.index),
            Style::new().fg(Color::Black).bg(if g.index == app.sel_gpu { t.accent } else { t.dim }).bold(),
        ),
        Span::raw(" "),
        s(g.short_name().to_string(), t.fg).bold(),
    ];
    if !compact {
        let mut meta = Vec::new();
        if let Some(a) = &g.architecture {
            meta.push(a.clone());
        }
        if let Some(m) = g.memory_total {
            meta.push(fmt::bytes(m));
        }
        if let (Some(gn), Some(w)) = (sm.pcie_gen, sm.pcie_width) {
            meta.push(format!("PCIe {gn}×{w}"));
        }
        if !meta.is_empty() {
            v.push(s(format!("  {}", meta.join(" · ")), t.dim));
        }
    }
    v.push(Span::raw(" "));
    Line::from(v)
}

fn status<'a>(app: &App, g: &GpuInfo, sm: &GpuSample) -> Line<'a> {
    let t = app.theme();
    let mut v = vec![Span::raw(" ")];
    if let Some(temp) = sm.temp {
        v.push(s(format!("{temp}°C"), temp_color(t, temp, g)).bold());
        if let Some(mt) = sm.temp_mem {
            v.push(s(format!(" mem {mt}°C"), t.dim));
        }
    }
    if let Some(fan) = sm.fan() {
        v.push(s(format!("  fan {fan}%"), t.dim));
    }
    if let Some(p) = sm.pstate {
        v.push(s(format!("  P{p}"), t.dim));
    }
    v.push(Span::raw(" "));
    Line::from(v).right_aligned()
}

fn mem_text(sm: &GpuSample) -> String {
    let base = match (sm.mem_used, sm.mem_total) {
        (Some(u), Some(tot)) => format!("{}/{}", fmt::bytes_short(u), fmt::bytes_short(tot)),
        _ => "N/A".into(),
    };
    match sm.spill.bytes {
        Some(b) if sm.spill.level == SpillLevel::Spilling => format!("{base} +{}", fmt::bytes_short(b)),
        _ => base,
    }
}

fn card(f: &mut Frame, app: &App, i: usize, area: Rect, hit: &mut Hit) {
    let t = app.theme();
    let (Some(g), Some(sm)) = (app.gpu(i), app.sample(i)) else { return };
    let sev = severity(g, sm);
    let mut b = block(t, i == app.sel_gpu).title_top(title(app, g, sm, area.width < 70)).title_top(status(app, g, sm));
    if sev > 0 {
        b = b.border_style(Style::new().fg(severity_color(t, sev, t.border)));
    }
    let alerts: Vec<&str> = sm.throttle.iter().filter(|r| r.severity() > 0).map(|r| r.label()).collect();
    if !alerts.is_empty() {
        b = b.title_bottom(Line::from(s(format!(" ▲ {} ", alerts.join(", ")), t.warn)).right_aligned());
    }
    if let Some(txt) = spill_text(sm) {
        let c = if sm.spill.level == SpillLevel::Spilling { t.crit } else { t.dim };
        b = b.title_bottom(Line::from(s(format!(" ▼ {txt} "), c).bold()).right_aligned());
    }
    if let Some(e) = &sm.error {
        b = b.title_bottom(Line::from(s(format!(" ✗ {e} "), t.crit)).right_aligned());
    }
    let nprocs = app.procs().iter().filter(|p| p.gpu == i).count();
    if nprocs > 0 {
        b = b.title_bottom(Line::from(s(format!(" {nprocs} proc{} ", if nprocs == 1 { "" } else { "s" }), t.dim)));
    }
    let inner = b.inner(area).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(b, area);
    hit.gpus.push((area, i));
    if inner.height == 0 {
        return;
    }

    let [left, right] = split(inner);

    let bars = card_bars(app, i, g, sm);
    let clk = sm.clock_sm.or(sm.clock_graphics);
    let mem_grad = mem_gradient(app, sm);
    let nbars = (left.height as usize).saturating_sub(1).clamp(1, bars.len());
    for (row, (label, frac, grad, val)) in bars.into_iter().take(nbars).enumerate() {
        bar_row(f, app, Rect { y: left.y + row as u16, height: 1, ..left }, label, frac, grad, &val);
    }
    if left.height as usize > nbars {
        let mut info = Vec::new();
        if let Some(c) = clk.filter(|_| nbars < 5) {
            info.push(s("SM ", t.dim));
            info.push(s(c.to_string(), t.fg));
        }
        if let Some(c) = sm.clock_mem {
            info.push(s(" MEM ", t.dim));
            info.push(s(c.to_string(), t.fg));
            info.push(s("MHz", t.dim));
        }
        if let (Some(rx), Some(tx)) = (sm.pcie_rx, sm.pcie_tx) {
            info.push(s("  ▼", t.accent));
            info.push(s(fmt::rate(rx), t.fg));
            info.push(s(" ▲", t.accent2));
            info.push(s(fmt::rate(tx), t.fg));
        }
        if !sm.nvlink.is_empty() || sm.nvlink_rate().is_some() {
            info.push(s("  NVL", t.dim));
            if !sm.nvlink.is_empty() {
                let up = sm.nvlink.iter().filter(|l| l.up).count();
                let col = if up == sm.nvlink.len() { t.ok } else { t.crit };
                info.push(s(format!(" {up}/{}", sm.nvlink.len()), col));
            }
            if let Some((rx, tx)) = sm.nvlink_rate() {
                info.push(s(" ▼", t.dim));
                info.push(s(fmt::rate(rx), t.fg));
                info.push(s(" ▲", t.dim));
                info.push(s(fmt::rate(tx), t.fg));
            }
        }
        if !sm.mig.is_empty() {
            info.push(s("  MIG ", t.dim));
            info.push(s(format!("{}×", sm.mig.len()), t.accent));
        }
        f.render_widget(Paragraph::new(Line::from(info)), Rect { y: left.y + nbars as u16, height: 1, ..left });
    }

    // History as sparkline strips, one per metric, labelled like the bars.
    let Some(h) = app.store.hist.gpus.get(i) else { return };
    let all = [
        Strip { label: "GPU", ring: &h.util, grad: t.util, auto_scale: false, reported: sm.util_gpu.is_some() },
        Strip { label: "MEM", ring: &h.mem, grad: mem_grad, auto_scale: false, reported: sm.mem_frac().is_some() },
        Strip { label: "PWR", ring: &h.power, grad: t.power, auto_scale: false, reported: sm.power.is_some() },
        Strip { label: "TMP", ring: &h.temp, grad: t.temp, auto_scale: false, reported: sm.temp.is_some() },
    ];
    strips(f, app, right, 1 + i as u32, &all);
}

type Bar = (&'static str, Option<f64>, Gradient, String);

/// Memory turns hot-colored while it spills.
fn mem_gradient(app: &App, sm: &GpuSample) -> Gradient {
    let t = app.theme();
    if sm.spill.level == SpillLevel::Spilling { t.temp } else { t.mem }
}

/// A GPU card's bars, most important first: core ones always (N/A is
/// information), optional ones only when the device reports them.
fn card_bars(app: &App, i: usize, g: &GpuInfo, sm: &GpuSample) -> Vec<Bar> {
    let t = app.theme();
    let ratio = |v: Option<u32>, max: Option<u32>| match (v, max) {
        (Some(v), Some(m)) if m > 0 => Some(v as f64 / m as f64),
        _ => None,
    };
    let clk = sm.clock_sm.or(sm.clock_graphics);
    let mem_grad = mem_gradient(app, sm);
    let core: [Bar; 3] = [
        (
            "GPU",
            sm.util_gpu.map(|v| v as f64 / 100.0),
            t.util,
            // MIG GPUs only report utilization per partition (see Details).
            if sm.util_gpu.is_none() && !sm.mig.is_empty() { "N/A (MIG)".into() } else { fmt::pct(sm.util_gpu) },
        ),
        ("MEM", sm.mem_frac(), mem_grad, mem_text(sm)),
        (
            "PWR",
            app.power_frac(i, sm),
            t.power,
            match (sm.power, sm.power_limit) {
                (Some(p), Some(l)) => format!("{p:.0}/{l:.0}W"),
                (Some(p), None) => format!("{p:.0}W"),
                _ => "N/A".into(),
            },
        ),
    ];
    let act = sm.activity.as_ref();
    // Measured DRAM bandwidth (GPM) beats the memory controller's busy time.
    let (mbw, mbw_text) = match (act.and_then(|a| a.dram_bw), g.memory_bandwidth) {
        (Some(p), Some(peak)) => (Some(p), fmt::bandwidth(peak * p as f64 / 100.0).replace(' ', "")),
        (Some(p), None) => (Some(p), fmt::pct(Some(p))),
        _ => (sm.util_mem, fmt::pct(sm.util_mem)),
    };
    let tensor = act.and_then(|a| a.tensor);
    let optional: [Bar; 8] = [
        ("TMP", ratio(sm.temp, Some(g.temp_shutdown.unwrap_or(100))), t.temp, fmt::opt(sm.temp, "°C")),
        ("CLK", ratio(clk, g.max_clock_sm.or(g.max_clock_graphics)), t.util, fmt::opt(clk, "MHz")),
        ("MBW", mbw.map(|v| v as f64 / 100.0), t.mem, mbw_text),
        ("TNS", tensor.map(|v| v as f64 / 100.0), t.util, fmt::pct(tensor)),
        ("FAN", sm.fan().map(|v| v as f64 / 100.0), t.temp, fmt::opt(sm.fan(), "%")),
        ("ENC", sm.enc_util.map(|v| v as f64 / 100.0), t.power, fmt::pct(sm.enc_util)),
        ("DEC", sm.dec_util.map(|v| v as f64 / 100.0), t.power, fmt::pct(sm.dec_util)),
        ("SYS", spill_frac(g, sm), t.temp, sm.mem_shared.map(fmt::bytes_short).unwrap_or_default()),
    ];
    core.into_iter().chain(optional.into_iter().filter(|b| b.1.is_some())).collect()
}

/// Rows a full card needs: its bars, the info line and the border.
fn card_height(app: &App, i: usize) -> u16 {
    match (app.gpu(i), app.sample(i)) {
        (Some(g), Some(sm)) => card_bars(app, i, g, sm).len() as u16 + 3,
        _ => 6,
    }
}

/// Left column for labelled bars, right column for history: the same split
/// for GPU cards and the host panel.
fn split(inner: Rect) -> [Rect; 2] {
    let left_w = if inner.width >= 110 {
        58
    } else if inner.width >= 80 {
        50
    } else {
        inner.width
    };
    Layout::horizontal([Constraint::Length(left_w), Constraint::Fill(1)]).areas(inner)
}

/// "LBL ████████░░░░  value": one bar row.
fn bar_row(f: &mut Frame, app: &App, r: Rect, label: &str, frac: Option<f64>, grad: Gradient, val: &str) {
    let t = app.theme();
    let [lab, bar, value] = Layout::horizontal([Constraint::Length(4), Constraint::Fill(1), Constraint::Length(13)]).areas(r);
    f.render_widget(Paragraph::new(s(label.to_string(), t.dim)), lab);
    f.render_widget(GradBar { frac, grad, track: t.track }, bar);
    f.render_widget(Paragraph::new(s(format!(" {val}"), t.fg)), value);
}

/// One sparkline strip: a history ring, normalized to [0,1] unless
/// `auto_scale` (rates: scaled to the window's peak).
struct Strip<'a> {
    label: &'static str,
    ring: &'a super::history::Ring,
    grad: Gradient,
    auto_scale: bool,
    /// The device reports this metric (strips for missing ones are skipped).
    reported: bool,
}

/// History as stacked strips with a label column, as many as fit at two
/// rows each (at least two).
fn strips(f: &mut Frame, app: &App, area: Rect, slot: u32, all: &[Strip]) {
    let t = app.theme();
    if area.width <= 10 || area.height == 0 {
        return;
    }
    for y in area.y..area.y + area.height {
        f.buffer_mut()[(area.x, y)].set_char('│').set_fg(t.border);
    }
    let fit = (area.height as usize / 2).max(2);
    let shown: Vec<&Strip> = all.iter().filter(|m| m.reported).take(fit).collect();
    if shown.is_empty() {
        return;
    }
    let labels = Rect { x: area.x + 2, width: 4, ..area };
    let chart = Rect { x: area.x + 6, width: area.width - 6, ..area };
    let ctx = app.chart_ctx();
    let n = ctx.samples(chart.width);
    let data: Vec<Vec<Option<f64>>> = shown
        .iter()
        .map(|m| {
            let raw = app.store.hist.tail(m.ring, n);
            if !m.auto_scale {
                return raw.to_vec();
            }
            let max = raw.iter().flatten().copied().fold(1.0f64, f64::max);
            raw.iter().map(|v| v.map(|x| x / max)).collect()
        })
        .collect();
    let series: Vec<Series> = shown.iter().zip(&data).map(|(m, d)| Series { data: d, grad: m.grad }).collect();
    ctx.draw(f, chart, slot, &series);
    for (m, lane) in shown.iter().zip(super::gfx::lanes(labels, shown.len())) {
        let y = lane.y + lane.height.saturating_sub(1) / 2;
        f.render_widget(Paragraph::new(s(m.label, t.dim)), Rect { y, height: 1, ..lane });
    }
}

/// System-memory share relative to VRAM size (only shown when it matters).
fn spill_frac(g: &GpuInfo, sm: &GpuSample) -> Option<f64> {
    if sm.spill.level != SpillLevel::Spilling {
        return None;
    }
    Some(sm.mem_shared? as f64 / g.memory_total? as f64)
}

fn compact(f: &mut Frame, app: &App, area: Rect, hit: &mut Hit) {
    let t = app.theme();
    let b = block(t, false).title_top(Line::from(vec![s(" GPUs ", t.title).bold(), s(format!("{} ", app.info.gpus.len()), t.dim)]));
    let inner = b.inner(area).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(b, area);
    for (i, g) in app.info.gpus.iter().enumerate().take(inner.height as usize) {
        let Some(sm) = app.sample(i) else { continue };
        let r = Rect { y: inner.y + i as u16, height: 1, ..inner };
        hit.gpus.push((r, i));
        let name_w = if r.width > 140 {
            26
        } else if r.width > 110 {
            20
        } else {
            14
        };
        let cols = Layout::horizontal([
            Constraint::Length(3),
            Constraint::Length(name_w),
            Constraint::Length(4),
            Constraint::Fill(2),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Fill(2),
            Constraint::Length(17),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Fill(1),
        ])
        .split(r);
        let sel = i == app.sel_gpu;
        let idx_style = if sel { Style::new().fg(Color::Black).bg(t.accent).bold() } else { Style::new().fg(t.accent).bold() };
        f.render_widget(Paragraph::new(Span::styled(format!("{:>2} ", g.index), idx_style)), cols[0]);
        f.render_widget(Paragraph::new(s(fmt::truncate(g.short_name(), name_w as usize - 1), t.fg)), cols[1]);
        f.render_widget(Paragraph::new(s("GPU", t.dim)), cols[2]);
        f.render_widget(GradBar { frac: sm.util_gpu.map(|v| v as f64 / 100.0), grad: t.util, track: t.track }, cols[3]);
        f.render_widget(Paragraph::new(s(format!("{:>4}", fmt::pct(sm.util_gpu)), t.fg)), cols[4]);
        f.render_widget(Paragraph::new(s(" MEM", t.dim)), Rect { width: cols[5].width.saturating_sub(1), ..cols[5] });
        f.render_widget(GradBar { frac: sm.mem_frac(), grad: t.mem, track: t.track }, cols[6]);
        let mem_c = severity_color(t, if sm.spill.level == SpillLevel::Spilling { 2 } else { 0 }, t.fg);
        f.render_widget(Paragraph::new(s(format!(" {}", mem_text(sm)), mem_c)), cols[7]);
        let temp = sm.temp.map(|v| s(format!("{v:>3}°C"), temp_color(t, v, g))).unwrap_or(s(" N/A", t.dim));
        f.render_widget(Paragraph::new(temp), cols[8]);
        let pc = app.power_frac(i, sm).map(|v| t.power.at(v)).unwrap_or(t.dim);
        f.render_widget(Paragraph::new(s(sm.power.map(|p| format!(" {p:>4.0}W")).unwrap_or(" N/A".into()), pc)), cols[9]);
        if let Some(h) = app.store.hist.gpus.get(i) {
            let spark = Rect { x: cols[10].x + 1, width: cols[10].width.saturating_sub(1), ..cols[10] };
            f.render_widget(Spark { data: app.store.hist.tail(&h.util, spark.width as usize), grad: t.util }, spark);
        }
    }
}

fn host_panel(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let hi = &app.info.host;
    let mut title = vec![s(" host ", t.title).bold(), s(format!("{} · {} cores ", hi.cpu_brand, hi.cpu_cores), t.dim)];
    let Some(snap) = app.snap() else {
        f.render_widget(block(t, false).title_top(Line::from(title)), area);
        return;
    };
    let h = &snap.host;
    title.push(s(format!("· up {} ", fmt::duration(h.uptime)), t.dim));
    if let Some(c) = h.cpu_temp {
        title.push(s(format!("· {c:.0}°C "), t.temp.at(((c as f64 - 35.0) / 60.0).clamp(0.0, 1.0))));
    }
    let mut b = block(t, false).title_top(Line::from(title));
    if let Some(l) = h.load {
        b = b.title_top(Line::from(vec![s(" load ", t.dim), s(format!("{:.2} {:.2} {:.2} ", l[0], l[1], l[2]), t.fg)]).right_aligned());
    }
    let inner = b.inner(area).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(b, area);
    if inner.height == 0 {
        return;
    }
    let [left, right] = split(inner);
    let row = |k: u16| (k < left.height).then(|| Rect { y: left.y + k, height: 1, ..left });
    let frac = |u: u64, tot: u64| (tot > 0).then(|| u as f64 / tot as f64);
    let rate = |v: Option<u64>| v.map(fmt::rate).unwrap_or_else(|| "N/A".into());

    if let Some(r) = row(0) {
        bar_row(f, app, r, "CPU", Some(h.cpu as f64 / 100.0), t.cpu, &format!("{:.0}%", h.cpu));
    }
    // Per-core load, one cell per core (averaged when there are more cores
    // than cells).
    if let Some(r) = row(1).filter(|_| !h.per_core.is_empty()) {
        const BLK: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        f.render_widget(Paragraph::new(s("CORE", t.dim)), Rect { width: 4, ..r });
        let cells = r.width.saturating_sub(19) as usize;
        let per = h.per_core.len().div_ceil(cells.max(1));
        for (k, chunk) in h.per_core.chunks(per.max(1)).take(cells).enumerate() {
            let v = (chunk.iter().sum::<f32>() / chunk.len() as f32 / 100.0).clamp(0.0, 1.0) as f64;
            f.buffer_mut()[(r.x + 5 + k as u16, r.y)]
                .set_char(BLK[((v * 8.0).round() as usize).max(1)])
                .set_fg(super::theme::paint(t.cpu, v, v, true))
                .set_bg(t.track);
        }
    }
    if let Some(r) = row(2) {
        let txt = format!("{}/{}", fmt::bytes_short(h.mem_used), fmt::bytes_short(h.mem_total));
        bar_row(f, app, r, "MEM", frac(h.mem_used, h.mem_total), t.mem, &txt);
    }
    if let Some(r) = row(3) {
        let txt =
            if h.swap_total == 0 { "none".into() } else { format!("{}/{}", fmt::bytes_short(h.swap_used), fmt::bytes_short(h.swap_total)) };
        bar_row(f, app, r, "SWP", frac(h.swap_used, h.swap_total).or(Some(0.0)), t.mem, &txt);
    }
    if let Some(r) = row(4) {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                s("NET ", t.dim),
                s("▼", t.accent),
                s(format!("{:<10}", rate(h.net_rx)), t.fg),
                s("▲", t.accent2),
                s(rate(h.net_tx), t.fg),
            ])),
            r,
        );
    }
    if let Some(r) = row(5) {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                s("DSK ", t.dim),
                s("r ", t.dim),
                s(format!("{:<9}", rate(h.disk_read)), t.fg),
                s("w ", t.dim),
                s(rate(h.disk_write), t.fg),
            ])),
            r,
        );
    }

    let hist = &app.store.hist;
    let all = [
        Strip { label: "CPU", ring: &hist.cpu, grad: t.cpu, auto_scale: false, reported: true },
        Strip { label: "MEM", ring: &hist.mem, grad: t.mem, auto_scale: false, reported: true },
        Strip { label: "NET", ring: &hist.net_rx, grad: t.power, auto_scale: true, reported: h.net_rx.is_some() },
        Strip { label: "DSK", ring: &hist.disk, grad: t.temp, auto_scale: true, reported: h.disk_read.is_some() },
    ];
    strips(f, app, right, 50, &all);
}
