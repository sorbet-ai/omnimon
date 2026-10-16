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
    let host_h = if show(super::Panel::Host) { 5u16 } else { 0 };
    let procs_on = show(super::Panel::Procs);
    // The process list gets what it needs (within reason); GPU cards grow
    // into the rest, up to a cap, so a lone GPU gets a big chart.
    let want_procs = if procs_on { (app.view.len() as u16 + 4).clamp(7, 18) } else { 0 };
    let avail = area.height.saturating_sub(host_h + want_procs);
    let cap = if !procs_on || n == 1 { 13u16.max(avail / n as u16) } else { 10 };
    let per_card = (avail / n as u16).min(cap);
    let full = per_card >= 6;
    let gpu_h = if !show(super::Panel::Cards) {
        0
    } else if full {
        per_card * n as u16
    } else {
        (n as u16 + 2).min(area.height.saturating_sub(host_h + 7).max(3))
    };
    let rest = if procs_on { Constraint::Min(3) } else { Constraint::Length(0) };
    let [gpus, host, procs] = Layout::vertical([Constraint::Length(gpu_h), Constraint::Length(host_h), rest]).areas(area);

    if gpu_h > 0 {
        if full {
            let rows = Layout::vertical(vec![Constraint::Length(per_card); n]).split(gpus);
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

    let left_w = if inner.width >= 110 {
        58
    } else if inner.width >= 80 {
        50
    } else {
        inner.width
    };
    let [left, right] = Layout::horizontal([Constraint::Length(left_w), Constraint::Fill(1)]).areas(inner);

    // Bars, most important first; core ones always show (N/A is information),
    // optional ones only when the device reports them.
    let ratio = |v: Option<u32>, max: Option<u32>| match (v, max) {
        (Some(v), Some(m)) if m > 0 => Some(v as f64 / m as f64),
        _ => None,
    };
    let clk = sm.clock_sm.or(sm.clock_graphics);
    let mem_grad = if sm.spill.level == SpillLevel::Spilling { t.temp } else { t.mem };
    let core: [(&str, Option<f64>, Gradient, String); 3] = [
        ("GPU", sm.util_gpu.map(|v| v as f64 / 100.0), t.util, fmt::pct(sm.util_gpu)),
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
    let optional: [(&str, Option<f64>, Gradient, String); 7] = [
        ("TMP", ratio(sm.temp, Some(g.temp_shutdown.unwrap_or(100))), t.temp, fmt::opt(sm.temp, "°C")),
        ("CLK", ratio(clk, g.max_clock_sm.or(g.max_clock_graphics)), t.util, fmt::opt(clk, "MHz")),
        ("MBW", sm.util_mem.map(|v| v as f64 / 100.0), t.mem, fmt::pct(sm.util_mem)),
        ("FAN", sm.fan().map(|v| v as f64 / 100.0), t.temp, fmt::opt(sm.fan(), "%")),
        ("ENC", sm.enc_util.map(|v| v as f64 / 100.0), t.power, fmt::pct(sm.enc_util)),
        ("DEC", sm.dec_util.map(|v| v as f64 / 100.0), t.power, fmt::pct(sm.dec_util)),
        ("SYS", spill_frac(g, sm), t.temp, sm.mem_shared.map(fmt::bytes_short).unwrap_or_default()),
    ];
    let bars: Vec<_> = core.into_iter().chain(optional.into_iter().filter(|b| b.1.is_some())).collect();
    let nbars = (left.height as usize).saturating_sub(1).clamp(1, bars.len());
    for (row, (label, frac, grad, val)) in bars.into_iter().take(nbars).enumerate() {
        let r = Rect { y: left.y + row as u16, height: 1, ..left };
        let [lab, bar, value] = Layout::horizontal([Constraint::Length(4), Constraint::Fill(1), Constraint::Length(13)]).areas(r);
        f.render_widget(Paragraph::new(s(label, t.dim)), lab);
        f.render_widget(GradBar { frac, grad, track: t.track }, bar);
        f.render_widget(Paragraph::new(s(format!(" {val}"), t.fg)), value);
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
        f.render_widget(Paragraph::new(Line::from(info)), Rect { y: left.y + nbars as u16, height: 1, ..left });
    }

    // Mirrored history: utilization grows up, memory hangs down.
    if right.width > 6 {
        let chart = Rect { x: right.x + 1, width: right.width - 1, ..right };
        for y in chart.y..chart.y + chart.height {
            f.buffer_mut()[(right.x, y)].set_char('│').set_fg(t.border);
        }
        let ctx = app.chart_ctx();
        let n = ctx.samples(chart.width);
        let Some(h) = app.store.hist.gpus.get(i) else { return };
        let series = [
            Series { data: app.store.hist.tail(&h.util, n), grad: t.util, down: false },
            Series { data: app.store.hist.tail(&h.mem, n), grad: mem_grad, down: true },
        ];
        ctx.draw(f, chart, 1 + i as u32, &series);
        // Text over a pixel image would punch holes in it; the values are in the bars.
        if ctx.pixels() {
            return;
        }
        let up_h = chart.height.div_ceil(2);
        let label = |f: &mut Frame, text: String, y: u16| {
            let w = text.chars().count() as u16;
            if chart.width > w + 2 {
                f.render_widget(Paragraph::new(s(text, t.dim)), Rect { x: chart.x + chart.width - w, y, width: w, height: 1 });
            }
        };
        label(f, format!("util {}", fmt::pct(sm.util_gpu)), chart.y);
        if chart.height > up_h {
            label(
                f,
                format!("mem {}", sm.mem_frac().map(|m| format!("{:.0}%", m * 100.0)).unwrap_or("N/A".into())),
                chart.y + chart.height - 1,
            );
        }
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
    if inner.height < 2 {
        return;
    }
    let half = inner.width / 2;
    let bar_w = half.saturating_sub(16).max(6);
    let row0 = Rect { height: 1, ..inner };
    let row1 = Rect { y: inner.y + 1, height: 1, ..inner };

    // CPU: total bar + a per-core heat strip + recent history.
    let [cl, cbar, cval, cores] =
        Layout::horizontal([Constraint::Length(4), Constraint::Length(bar_w), Constraint::Length(7), Constraint::Fill(1)]).areas(row0);
    f.render_widget(Paragraph::new(s("CPU", t.dim)), cl);
    f.render_widget(GradBar { frac: Some(h.cpu as f64 / 100.0), grad: t.cpu, track: t.track }, cbar);
    f.render_widget(Paragraph::new(s(format!(" {:>4.0}%", h.cpu), t.fg)), cval);
    const BLK: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let end = cores.x + cores.width;
    let mut x = cores.x + 1;
    for c in &h.per_core {
        if x >= end {
            break;
        }
        let v = (*c as f64 / 100.0).clamp(0.0, 1.0);
        f.buffer_mut()[(x, cores.y)].set_char(BLK[((v * 8.0).round() as usize).max(1)]).set_fg(t.cpu.at(v)).set_bg(t.track);
        x += 1;
    }
    if x + 12 < end {
        let rest = Rect { x: x + 1, width: end - x - 1, ..cores };
        f.render_widget(Spark { data: app.store.hist.tail(&app.store.hist.cpu, rest.width as usize), grad: t.cpu }, rest);
    }

    // Memory + swap.
    let [ml, mbar, mval, sl, sbar, sval] = Layout::horizontal([
        Constraint::Length(4),
        Constraint::Length(bar_w),
        Constraint::Length(13),
        Constraint::Length(5),
        Constraint::Fill(1),
        Constraint::Length(13),
    ])
    .areas(row1);
    let frac = |u: u64, tot: u64| (tot > 0).then(|| u as f64 / tot as f64);
    f.render_widget(Paragraph::new(s("MEM", t.dim)), ml);
    f.render_widget(GradBar { frac: frac(h.mem_used, h.mem_total), grad: t.mem, track: t.track }, mbar);
    f.render_widget(Paragraph::new(s(format!(" {}/{}", fmt::bytes_short(h.mem_used), fmt::bytes_short(h.mem_total)), t.fg)), mval);
    f.render_widget(Paragraph::new(s(" SWP", t.dim)), sl);
    f.render_widget(GradBar { frac: frac(h.swap_used, h.swap_total).or(Some(0.0)), grad: t.mem, track: t.track }, sbar);
    f.render_widget(Paragraph::new(s(format!(" {}/{}", fmt::bytes_short(h.swap_used), fmt::bytes_short(h.swap_total)), t.fg)), sval);

    // Network + disk throughput with recent history (auto-scaled sparklines).
    if inner.height < 3 {
        return;
    }
    let row2 = Rect { y: inner.y + 2, height: 1, ..inner };
    let [nl, nval, nspark, dl, dval, dspark] = Layout::horizontal([
        Constraint::Length(4),
        Constraint::Length(22),
        Constraint::Length(half.saturating_sub(26).max(4)),
        Constraint::Length(5),
        Constraint::Length(22),
        Constraint::Fill(1),
    ])
    .areas(row2);
    let rate = |v: Option<u64>| v.map(fmt::rate).unwrap_or_else(|| "N/A".into());
    f.render_widget(Paragraph::new(s("NET", t.dim)), nl);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            s(" ▼", t.accent),
            s(format!("{:<9}", rate(h.net_rx)), t.fg),
            s("▲", t.accent2),
            s(rate(h.net_tx), t.fg),
        ])),
        nval,
    );
    let scaled = |r: &[Option<f64>]| {
        let max = r.iter().flatten().copied().fold(1.0f64, f64::max);
        r.iter().map(|v| v.map(|x| x / max)).collect::<Vec<_>>()
    };
    let rx = scaled(app.store.hist.tail(&app.store.hist.net_rx, nspark.width as usize));
    f.render_widget(Spark { data: &rx, grad: t.mem }, nspark);
    f.render_widget(Paragraph::new(s(" DSK", t.dim)), dl);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            s(" r ", t.dim),
            s(format!("{:<9}", rate(h.disk_read)), t.fg),
            s("w ", t.dim),
            s(rate(h.disk_write), t.fg),
        ])),
        dval,
    );
    let disk = scaled(app.store.hist.tail(&app.store.hist.disk, dspark.width as usize));
    f.render_widget(Spark { data: &disk, grad: t.power }, Rect { x: dspark.x + 1, width: dspark.width.saturating_sub(1), ..dspark });
}
