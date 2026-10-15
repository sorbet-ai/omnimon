//! All rendering. Layout adapts to terminal size and GPU count.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, Wrap};

use super::theme::{Gradient, Theme};
use super::widgets::{AreaChart, GradBar, Spark};
use super::{App, Popup, SIGNALS, SortKey, Tab};
use crate::brand::{APP_NAME, APP_VERSION};
use crate::fmt;
use crate::model::{GpuInfo, GpuSample, PidMapping, ProcKind};

pub fn draw(f: &mut Frame, app: &mut App) {
    app.hit = Default::default();
    let area = f.area();
    let [header, body, footer] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    draw_header(f, app, header);
    match app.tab {
        Tab::Overview => draw_overview(f, app, body),
        Tab::Charts => draw_charts(f, app, body),
        Tab::Details => draw_details(f, app, body),
    }
    draw_footer(f, app, footer);
    if let Some((msg, _, err)) = &app.toast {
        let t = app.theme();
        let w = (msg.chars().count() as u16 + 4).min(area.width);
        let r = Rect { x: area.x + area.width - w, y: footer.y.saturating_sub(1), width: w, height: 1 };
        f.render_widget(Clear, r);
        let st = if *err { Style::new().fg(Color::Black).bg(t.crit) } else { Style::new().fg(Color::Black).bg(t.accent) };
        f.render_widget(Paragraph::new(format!("  {msg}  ")).style(st.bold()), r);
    }
    match &app.popup {
        Some(Popup::Help) => draw_help(f, app),
        Some(Popup::Kill { .. }) => draw_kill(f, app),
        Some(Popup::Process { .. }) => draw_process(f, app),
        None => {}
    }
}

// ---------------------------------------------------------------- helpers

fn block<'a>(t: &Theme, focused: bool) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { t.border_hi } else { t.border }))
}

fn s<'a>(text: impl Into<std::borrow::Cow<'a, str>>, fg: Color) -> Span<'a> {
    Span::styled(text, Style::new().fg(fg))
}

fn pct(v: Option<f32>) -> String {
    v.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "N/A".into())
}

fn temp_color(t: &Theme, temp: u32, info: &GpuInfo) -> Color {
    let slow = info.temp_slowdown.or(info.temp_max_operating).unwrap_or(90) as f64;
    t.temp.at(((temp as f64 - 30.0) / (slow - 30.0)).clamp(0.0, 1.0))
}

fn truncate(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n - 1).collect::<String>() + "…" }
}

/// Throttle reasons worth shouting about (idle is normal).
fn alerts(s: &GpuSample) -> Vec<&str> {
    s.throttle.iter().map(|x| x.as_str()).filter(|r| !matches!(*r, "idle" | "app clocks" | "display clocks")).collect()
}

fn severity(app: &App, i: usize) -> u8 {
    let Some(snap) = &app.snap else { return 0 };
    let Some(s) = snap.gpus.get(i) else { return 0 };
    let info = &app.info.gpus[i];
    if s.error.is_some() || s.ecc_uncorrected.is_some_and(|e| e > 0) {
        return 2;
    }
    let slow = info.temp_slowdown.or(info.temp_max_operating).unwrap_or(90);
    if s.temp.is_some_and(|t| t + 3 >= slow) || s.throttle.iter().any(|r| r.contains("thermal") || r == "hw slowdown" || r == "power brake") {
        return 2;
    }
    if s.temp.is_some_and(|t| t + 10 >= slow) {
        return 1;
    }
    0
}

// ---------------------------------------------------------------- header / footer

fn draw_header(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme();
    let mut left = vec![
        Span::styled(format!(" {APP_NAME} "), Style::new().fg(Color::Black).bg(t.title).bold()),
        Span::raw(" "),
        s(app.info.host.hostname.clone(), t.fg).bold(),
    ];
    if let Some(c) = &app.info.host.container {
        left.push(s(format!(" [{c}]"), t.accent2));
    }
    if app.info.host.wsl {
        left.push(s(" [wsl]", t.accent2));
    }
    if let Some(b) = app.info.backends.first() {
        if let Some(d) = &b.driver_version {
            left.push(s("  driver ", t.dim));
            left.push(s(d.clone(), t.fg));
        }
        if let Some(c) = &b.runtime_version {
            left.push(s("  CUDA ", t.dim));
            left.push(s(c.clone(), t.fg));
        }
    }
    let lw: usize = left.iter().map(|s| s.width()).sum();

    let now = chrono::Local::now().format("%H:%M:%S").to_string();
    let rate = format!("{:.2}s", app.interval.as_secs_f64()).replace(".00s", "s");
    let right = if app.paused {
        vec![Span::styled(" PAUSED ", Style::new().fg(Color::Black).bg(t.warn).bold()), Span::raw("  "), s(now, t.fg).bold(), Span::raw(" ")]
    } else {
        vec![s("every ", t.faint), s(rate, t.dim), Span::raw("  "), s(now, t.fg).bold(), Span::raw(" ")]
    };
    let rw: usize = right.iter().map(|s| s.width()).sum();

    // Tabs centered in the remaining space.
    let mut tabs = Vec::new();
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let sel = *tab == app.tab;
        let label = format!(" {} {} ", i + 1, tab.label());
        tabs.push((label, sel, *tab));
    }
    let tw: usize = tabs.iter().map(|(l, _, _)| l.chars().count() + 1).sum();
    f.render_widget(Paragraph::new(Line::from(left)), area);
    f.render_widget(Paragraph::new(Line::from(right).right_aligned()), area);
    let free = (area.width as usize).saturating_sub(lw + rw);
    if free >= tw + 2 {
        let mut x = area.x + (lw + (free - tw) / 2) as u16;
        for (label, sel, tab) in tabs {
            let w = label.chars().count() as u16;
            let r = Rect { x, y: area.y, width: w, height: 1 };
            let st = if sel { Style::new().fg(Color::Black).bg(t.accent).bold() } else { Style::new().fg(t.dim) };
            f.render_widget(Paragraph::new(label).style(st), r);
            app.hit.tabs.push((r, tab));
            x += w + 1;
        }
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let keys: &[(&str, &str)] = match app.tab {
        Tab::Overview => &[
            ("q", "quit"),
            ("?", "help"),
            ("tab", "view"),
            ("↑↓", "select"),
            ("←→", "gpu"),
            ("s", "sort"),
            ("/", "filter"),
            ("g", "this gpu"),
            ("space", "tag"),
            ("K", "kill"),
            ("t", "theme"),
            ("+-", "rate"),
            ("p", "pause"),
        ],
        _ => &[("q", "quit"), ("?", "help"), ("tab", "view"), ("←→", "gpu"), ("t", "theme"), ("+-", "rate"), ("p", "pause")],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in keys {
        spans.push(Span::styled(*k, Style::new().fg(t.accent).bold()));
        spans.push(s(format!(" {d}  "), t.dim));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    if !app.info.warnings.is_empty() {
        let w = app.info.warnings.join(" · ");
        let line = Line::from(vec![s("! ", t.warn).bold(), s(w, t.warn), Span::raw(" ")]).right_aligned();
        let wlen = line.width() as u16;
        let used: u16 = 1 + keys.iter().map(|(k, d)| (k.chars().count() + d.chars().count() + 3) as u16).sum::<u16>();
        if used + wlen < area.width {
            f.render_widget(Paragraph::new(line), area);
        }
    }
}

// ---------------------------------------------------------------- overview

fn draw_overview(f: &mut Frame, app: &mut App, area: Rect) {
    let n = app.info.gpus.len();
    if n == 0 {
        draw_no_gpus(f, app, area);
        return;
    }
    let host_h = 4u16;
    // The process list gets what it needs (within reason); GPU cards grow
    // into the rest, up to a cap, so a lone GPU gets a big chart.
    let nprocs = app.rows().len() as u16;
    let want_procs = (nprocs + 4).clamp(7, 18);
    let avail = area.height.saturating_sub(host_h + want_procs);
    let per_card = (avail / n as u16).min(if n == 1 { 13 } else { 10 });
    let full = per_card >= 6;
    let gpu_h = if full { per_card * n as u16 } else { (n as u16 + 2).min(area.height.saturating_sub(host_h + 7).max(3)) };
    let [gpus, host, procs] =
        Layout::vertical([Constraint::Length(gpu_h), Constraint::Length(host_h), Constraint::Min(3)]).areas(area);

    if full {
        let rows = Layout::vertical(vec![Constraint::Length(per_card); n]).split(gpus);
        for i in 0..n {
            draw_gpu_card(f, app, i, rows[i]);
        }
    } else {
        draw_gpu_compact(f, app, gpus);
    }
    draw_host(f, app, host);
    draw_procs(f, app, procs);
}

fn draw_no_gpus(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let mut lines = vec![Line::from(s("No GPUs detected", t.warn).bold()), Line::default()];
    for w in &app.info.warnings {
        lines.push(Line::from(s(format!("• {w}"), t.dim)));
    }
    lines.push(Line::default());
    lines.push(Line::from(s(format!("Try `{APP_NAME} --diagnose`, or `{APP_NAME} --demo` to see the UI with simulated GPUs."), t.dim)));
    if app.info.host.container.is_some() {
        lines.push(Line::from(s(
            "In Docker, run with `--gpus all` (and NVIDIA_DRIVER_CAPABILITIES including `utility`).",
            t.dim,
        )));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block(t, false).title_top(" GPUs ")), area);
}

fn gpu_title<'a>(app: &App, i: usize, compact: bool) -> Line<'a> {
    let t = app.theme();
    let g = &app.info.gpus[i];
    let mut v = vec![
        Span::raw(" "),
        Span::styled(format!("{}", g.index), Style::new().fg(Color::Black).bg(if i == app.sel_gpu { t.accent } else { t.dim }).bold()),
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
        if let Some(snap) = &app.snap {
            if let Some(sm) = snap.gpus.get(i) {
                if let (Some(gn), Some(w)) = (sm.pcie_gen, sm.pcie_width) {
                    meta.push(format!("PCIe {gn}×{w}"));
                }
            }
        }
        if !meta.is_empty() {
            v.push(s(format!("  {}", meta.join(" · ")), t.dim));
        }
    }
    v.push(Span::raw(" "));
    Line::from(v)
}

fn gpu_status<'a>(app: &App, i: usize) -> Line<'a> {
    let t = app.theme();
    let g = &app.info.gpus[i];
    let Some(sm) = app.snap.as_ref().and_then(|s| s.gpus.get(i)) else { return Line::default() };
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

fn draw_gpu_card(f: &mut Frame, app: &mut App, i: usize, area: Rect) {
    let t = app.theme();
    let sev = severity(app, i);
    let focused = i == app.sel_gpu;
    let mut b = block(t, focused).title_top(gpu_title(app, i, area.width < 70)).title_top(gpu_status(app, i));
    if sev == 2 {
        b = b.border_style(Style::new().fg(t.crit));
    } else if sev == 1 {
        b = b.border_style(Style::new().fg(t.warn));
    }
    let sm = app.snap.as_ref().and_then(|s| s.gpus.get(i)).cloned().unwrap_or_default();
    let al = alerts(&sm);
    if !al.is_empty() {
        b = b.title_bottom(Line::from(s(format!(" ▲ {} ", al.join(", ")), t.warn)).right_aligned());
    }
    if let Some(e) = &sm.error {
        b = b.title_bottom(Line::from(s(format!(" ✗ {e} "), t.crit)).right_aligned());
    }
    let nprocs = app.snap.as_ref().map(|s| s.processes.iter().filter(|p| p.gpu == i).count()).unwrap_or(0);
    if nprocs > 0 {
        b = b.title_bottom(Line::from(s(format!(" {nprocs} proc{} ", if nprocs == 1 { "" } else { "s" }), t.dim)));
    }
    let inner = b.inner(area).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(b, area);
    app.hit.gpus.push((area, i));
    if inner.height == 0 {
        return;
    }

    let g = app.info.gpus[i].clone();
    let left_w = if inner.width >= 110 { 58 } else if inner.width >= 80 { 50 } else { inner.width };
    let [left, right] = Layout::horizontal([Constraint::Length(left_w), Constraint::Fill(1)]).areas(inner);

    // Bars, most important first; the last line is the info line.
    let mut bars: Vec<(&str, Option<f64>, Gradient, String)> = vec![
        ("GPU", sm.util_gpu.map(|v| v as f64 / 100.0), t.util, pct(sm.util_gpu)),
        (
            "MEM",
            sm.mem_frac(),
            t.mem,
            match (sm.mem_used, sm.mem_total) {
                (Some(u), Some(tot)) => format!("{}/{}", fmt::bytes_short(u), fmt::bytes_short(tot)),
                _ => "N/A".into(),
            },
        ),
        (
            "PWR",
            app.power_frac(i, &sm),
            t.power,
            match (sm.power, sm.power_limit) {
                (Some(p), Some(l)) => format!("{p:.0}/{l:.0}W"),
                (Some(p), None) => format!("{p:.0}W"),
                _ => "N/A".into(),
            },
        ),
    ];
    {
        let tmax = g.temp_shutdown.unwrap_or(100) as f64;
        bars.push(("TMP", sm.temp.map(|v| v as f64 / tmax), t.temp, sm.temp.map(|v| format!("{v}°C")).unwrap_or("N/A".into())));
        let clk_max = g.max_clock_sm.or(g.max_clock_graphics);
        let clk = sm.clock_sm.or(sm.clock_graphics);
        bars.push((
            "CLK",
            match (clk, clk_max) {
                (Some(c), Some(m)) if m > 0 => Some(c as f64 / m as f64),
                _ => None,
            },
            t.util,
            clk.map(|c| format!("{c}MHz")).unwrap_or("N/A".into()),
        ));
        bars.push(("MBW", sm.util_mem.map(|v| v as f64 / 100.0), t.mem, pct(sm.util_mem)));
        if let Some(fan) = sm.fan() {
            bars.push(("FAN", Some(fan as f64 / 100.0), t.temp, format!("{fan}%")));
        }
        if sm.enc_util.is_some() {
            bars.push(("ENC", sm.enc_util.map(|v| v as f64 / 100.0), t.power, pct(sm.enc_util)));
            bars.push(("DEC", sm.dec_util.map(|v| v as f64 / 100.0), t.power, pct(sm.dec_util)));
        }
    }
    // Core bars always show (N/A is information); optional ones only when the
    // device reports them.
    let mut bars: Vec<_> = bars.into_iter().enumerate().filter(|(k, b)| *k < 3 || b.1.is_some()).map(|(_, b)| b).collect();
    let nbars = (left.height as usize).saturating_sub(1).min(bars.len()).max(1);
    bars.truncate(nbars);
    let val_w = 12u16;
    for (row, (label, frac, grad, val)) in bars.into_iter().take(nbars).enumerate() {
        let y = left.y + row as u16;
        let r = Rect { x: left.x, y, width: left.width, height: 1 };
        let [lab, bar, value] = Layout::horizontal([Constraint::Length(4), Constraint::Fill(1), Constraint::Length(val_w)]).areas(r);
        f.render_widget(Paragraph::new(s(label, t.dim)), lab);
        f.render_widget(GradBar { frac, grad, track: t.track }, bar);
        f.render_widget(Paragraph::new(Line::from(s(format!(" {val}"), t.fg)).left_aligned()), value);
    }
    if left.height as usize > nbars {
        let y = left.y + nbars as u16;
        let mut info = Vec::new();
        if nbars < 5 {
            if let Some(c) = sm.clock_sm.or(sm.clock_graphics) {
                info.push(s("SM ", t.dim));
                info.push(s(format!("{c}"), t.fg));
            }
        }
        if let Some(c) = sm.clock_mem {
            info.push(s(" MEM ", t.dim));
            info.push(s(format!("{c}"), t.fg));
            info.push(s("MHz", t.dim));
        }
        if let (Some(rx), Some(tx)) = (sm.pcie_rx, sm.pcie_tx) {
            info.push(s("  ▼", t.accent));
            info.push(s(fmt::rate(rx), t.fg));
            info.push(s(" ▲", t.accent2));
            info.push(s(fmt::rate(tx), t.fg));
        }
        if (sm.enc_util.is_some() || sm.dec_util.is_some()) && nbars < 8 {
            info.push(s("  ENC ", t.dim));
            info.push(s(pct(sm.enc_util), t.fg));
            info.push(s(" DEC ", t.dim));
            info.push(s(pct(sm.dec_util), t.fg));
        }
        f.render_widget(Paragraph::new(Line::from(info)), Rect { x: left.x, y, width: left.width, height: 1 });
    }

    // Mirrored history: utilization grows up, memory hangs down.
    if right.width > 6 {
        let chart = Rect { x: right.x + 1, width: right.width - 1, ..right };
        for y in chart.y..chart.y + chart.height {
            f.buffer_mut()[(right.x, y)].set_char('│').set_fg(t.border);
        }
        let n = chart.width as usize * 2;
        let h = &app.hist[i];
        let up_h = chart.height.div_ceil(2);
        let up = Rect { height: up_h, ..chart };
        let down = Rect { y: chart.y + up_h, height: chart.height - up_h, ..chart };
        f.render_widget(AreaChart { data: h.util.tail(n), grad: t.util, inverted: false }, up);
        f.render_widget(AreaChart { data: h.mem.tail(n), grad: t.mem, inverted: true }, down);
        let lab = |f: &mut Frame, text: String, y: u16, c: Color| {
            let w = text.chars().count() as u16;
            if chart.width > w + 2 {
                f.render_widget(Paragraph::new(s(text, c)), Rect { x: chart.x + chart.width - w, y, width: w, height: 1 });
            }
        };
        lab(f, format!("util {}", pct(sm.util_gpu)), up.y, t.dim);
        if down.height > 0 {
            lab(f, format!("mem {}", sm.mem_frac().map(|m| format!("{:.0}%", m * 100.0)).unwrap_or("N/A".into())), down.y + down.height - 1, t.dim);
        }
    }
}

fn draw_gpu_compact(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme();
    let b = block(t, false).title_top(Line::from(vec![s(" GPUs ", t.title).bold(), s(format!("{} ", app.info.gpus.len()), t.dim)]));
    let inner = b.inner(area).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(b, area);
    let n = app.info.gpus.len();
    for i in 0..n.min(inner.height as usize) {
        let r = Rect { x: inner.x, y: inner.y + i as u16, width: inner.width, height: 1 };
        app.hit.gpus.push((r, i));
        let g = &app.info.gpus[i];
        let sm = app.snap.as_ref().and_then(|s| s.gpus.get(i)).cloned().unwrap_or_default();
        let sel = i == app.sel_gpu;
        let name_w = if r.width > 140 { 26 } else if r.width > 110 { 20 } else { 14 };
        let cols = Layout::horizontal([
            Constraint::Length(3),
            Constraint::Length(name_w),
            Constraint::Length(4),
            Constraint::Fill(2),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Fill(2),
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Fill(1),
        ])
        .split(r);
        let idx_style = if sel { Style::new().fg(Color::Black).bg(t.accent).bold() } else { Style::new().fg(t.accent).bold() };
        f.render_widget(Paragraph::new(Span::styled(format!("{:>2} ", g.index), idx_style)), cols[0]);
        f.render_widget(Paragraph::new(s(truncate(g.short_name(), name_w as usize - 1), t.fg)), cols[1]);
        f.render_widget(Paragraph::new(s("GPU", t.dim)), cols[2]);
        f.render_widget(GradBar { frac: sm.util_gpu.map(|v| v as f64 / 100.0), grad: t.util, track: t.track }, cols[3]);
        f.render_widget(Paragraph::new(s(format!("{:>4}", pct(sm.util_gpu)), t.fg)), cols[4]);
        f.render_widget(Paragraph::new(s(" MEM", t.dim)), Rect { width: cols[5].width.saturating_sub(1), ..cols[5] });
        f.render_widget(GradBar { frac: sm.mem_frac(), grad: t.mem, track: t.track }, cols[6]);
        let mem = match (sm.mem_used, sm.mem_total) {
            (Some(u), Some(tot)) => format!(" {}/{}", fmt::bytes_short(u), fmt::bytes_short(tot)),
            _ => " N/A".into(),
        };
        f.render_widget(Paragraph::new(s(mem, t.fg)), cols[7]);
        let temp = sm.temp.map(|v| s(format!("{v:>3}°C"), temp_color(t, v, g))).unwrap_or(s(" N/A", t.dim));
        f.render_widget(Paragraph::new(temp), cols[8]);
        let pw = sm.power.map(|p| format!(" {p:>4.0}W")).unwrap_or(" N/A".into());
        let pc = app.power_frac(i, &sm).map(|v| t.power.at(v)).unwrap_or(t.dim);
        f.render_widget(Paragraph::new(s(pw, pc)), cols[9]);
        let n = cols[10].width as usize;
        let spark = Rect { x: cols[10].x + 1, width: cols[10].width.saturating_sub(1), ..cols[10] };
        f.render_widget(Spark { data: app.hist[i].util.tail(n), grad: t.util }, spark);
    }
}

fn draw_host(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let hi = &app.info.host;
    let mut title = vec![s(" host ", t.title).bold(), s(format!("{} · {} cores ", hi.cpu_brand, hi.cpu_cores), t.dim)];
    let Some(snap) = &app.snap else {
        f.render_widget(block(t, false).title_top(Line::from(title)), area);
        return;
    };
    let h = &snap.host;
    title.push(s(format!("· up {} ", fmt::duration(h.uptime)), t.dim));
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
    let row0 = Rect { height: 1, ..inner };
    let row1 = Rect { y: inner.y + 1, height: 1, ..inner };

    // CPU: total bar + a per-core heat strip.
    let [cl, cbar, cval, cores] =
        Layout::horizontal([Constraint::Length(4), Constraint::Length(half.saturating_sub(16).max(6)), Constraint::Length(7), Constraint::Fill(1)]).areas(row0);
    f.render_widget(Paragraph::new(s("CPU", t.dim)), cl);
    f.render_widget(GradBar { frac: Some(h.cpu as f64 / 100.0), grad: t.cpu, track: t.track }, cbar);
    f.render_widget(Paragraph::new(s(format!(" {:>4.0}%", h.cpu), t.fg)), cval);
    const BLK: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let mut x = cores.x + 1;
    for c in &h.per_core {
        if x >= cores.x + cores.width {
            break;
        }
        let v = (*c as f64 / 100.0).clamp(0.0, 1.0);
        let idx = ((v * 8.0).round() as usize).max(1);
        f.buffer_mut()[(x, cores.y)].set_char(BLK[idx]).set_fg(t.cpu.at(v)).set_bg(t.track);
        x += 1;
    }
    if x + 12 < cores.x + cores.width {
        let rest = Rect { x: x + 1, width: cores.x + cores.width - x - 1, ..cores };
        f.render_widget(Spark { data: app.host_hist.cpu.tail(rest.width as usize), grad: t.cpu }, rest);
    }

    // Memory + swap.
    let [ml, mbar, mval, sl, sbar, sval] = Layout::horizontal([
        Constraint::Length(4),
        Constraint::Length(half.saturating_sub(16).max(6)),
        Constraint::Length(13),
        Constraint::Length(5),
        Constraint::Fill(1),
        Constraint::Length(13),
    ])
    .areas(row1);
    f.render_widget(Paragraph::new(s("MEM", t.dim)), ml);
    let mf = if h.mem_total > 0 { Some(h.mem_used as f64 / h.mem_total as f64) } else { None };
    f.render_widget(GradBar { frac: mf, grad: t.mem, track: t.track }, mbar);
    f.render_widget(Paragraph::new(s(format!(" {}/{}", fmt::bytes_short(h.mem_used), fmt::bytes_short(h.mem_total)), t.fg)), mval);
    f.render_widget(Paragraph::new(s(" SWP", t.dim)), sl);
    let sf = if h.swap_total > 0 { Some(h.swap_used as f64 / h.swap_total as f64) } else { Some(0.0) };
    f.render_widget(GradBar { frac: sf, grad: t.mem, track: t.track }, sbar);
    f.render_widget(Paragraph::new(s(format!(" {}/{}", fmt::bytes_short(h.swap_used), fmt::bytes_short(h.swap_total)), t.fg)), sval);
}

fn draw_procs(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme();
    let rows = app.rows();
    let total = app.snap.as_ref().map(|s| s.processes.len()).unwrap_or(0);
    let mut title = vec![s(" Processes ", t.title).bold(), s(format!("{} ", rows.len()), t.dim)];
    if rows.len() != total {
        title.push(s(format!("of {total} "), t.dim));
    }
    if app.gpu_only {
        title.push(s(format!("· GPU {} only ", app.sel_gpu), t.accent));
    }
    if !app.tagged.is_empty() {
        title.push(s(format!("· {} tagged ", app.tagged.len()), t.accent2));
    }
    let mut b = block(t, true).title_top(Line::from(title));
    let filter_line = if app.filter_edit {
        Some(Line::from(vec![s(" / ", t.accent).bold(), s(app.filter.clone(), t.fg), s("▏ ", t.accent)]))
    } else if !app.filter.is_empty() {
        Some(Line::from(vec![s(" filter: ", t.dim), s(app.filter.clone(), t.accent), s(" ", t.dim)]))
    } else {
        None
    };
    if let Some(l) = filter_line {
        b = b.title_top(l.right_aligned());
    }
    let inner = b.inner(area);

    let wide = inner.width >= 100;
    let mid = inner.width >= 76;
    // (header, width, sort key, right-align)
    let mut cols: Vec<(&str, u16, Option<SortKey>, bool)> = vec![("GPU", 3, Some(SortKey::Gpu), true), ("PID", 8, Some(SortKey::Pid), true)];
    if mid {
        cols.push(("USER", 10, Some(SortKey::User), false));
    }
    cols.push(("TYPE", 4, None, false));
    cols.push(("GPU-MEM", 9, Some(SortKey::GpuMem), true));
    cols.push(("SM", 4, Some(SortKey::Sm), true));
    if wide {
        cols.push(("ENC/DEC", 7, None, true));
    }
    cols.push(("CPU", 6, Some(SortKey::Cpu), true));
    if mid {
        cols.push(("RAM", 6, Some(SortKey::Rss), true));
    }
    if wide {
        cols.push(("TIME", 9, Some(SortKey::Time), true));
    }
    cols.push(("COMMAND", 0, Some(SortKey::Name), false));

    let fixed: u16 = cols.iter().map(|c| c.1 + 1).sum::<u16>() + 2;
    let cmd_w = inner.width.saturating_sub(fixed).max(10);
    let widths: Vec<Constraint> = cols.iter().map(|c| Constraint::Length(if c.1 == 0 { cmd_w } else { c.1 })).collect();

    // Header with sort indicator; record x ranges for mouse sorting.
    let arrow = if app.sort_desc { "▼" } else { "▲" };
    let header = Row::new(cols.iter().map(|(h, _, k, right)| {
        let active = *k == Some(app.sort);
        let label = if active { format!("{h}{arrow}") } else { h.to_string() };
        let st = if active { Style::new().fg(t.accent).bold() } else { Style::new().fg(t.dim).bold() };
        let line = Line::from(label).style(st);
        Cell::from(if *right { line.right_aligned() } else { line })
    }));
    let mut hit_header = Vec::new();
    let mut x = inner.x + 2;
    for c in &cols {
        let w = if c.1 == 0 { cmd_w } else { c.1 };
        if let Some(k) = c.2 {
            hit_header.push((x, x + w, k));
        }
        x += w + 1;
    }

    let me = app.me.clone();
    let snap_gpus = app.snap.as_ref().map(|s| s.gpus.clone()).unwrap_or_default();
    let body: Vec<Row> = rows
        .iter()
        .map(|p| {
            let mine = match (&p.user, &me) {
                (Some(u), Some(m)) => u.eq_ignore_ascii_case(m) || u.ends_with(&format!("\\{m}")),
                _ => false,
            };
            let ext = p.mapping == PidMapping::External;
            let base = if ext {
                Style::new().fg(t.faint).add_modifier(Modifier::ITALIC)
            } else if mine || p.user.is_none() {
                Style::new().fg(t.fg)
            } else {
                Style::new().fg(t.dim)
            };
            let tagged = app.tagged.contains(&(p.gpu, p.pid));
            let mut cells: Vec<Cell> = Vec::new();
            for (h, _, _, _) in &cols {
                let c: Cell = match *h {
                    "GPU" => Cell::from(Line::from(format!("{}", p.gpu)).right_aligned()).style(Style::new().fg(t.accent)),
                    "PID" => {
                        let (txt, st) = match p.mapping {
                            PidMapping::Direct => (format!("{}", p.pid), base),
                            PidMapping::Translated => (format!("{}", p.local_pid.unwrap_or(p.pid)), base),
                            PidMapping::Guessed => (format!("~{}", p.local_pid.unwrap_or(p.pid)), base.fg(t.warn)),
                            PidMapping::External => (format!("h{}", p.pid), base),
                        };
                        let txt = if tagged { format!("●{txt}") } else { txt };
                        Cell::from(Line::from(txt).right_aligned()).style(if tagged { st.fg(t.accent2).bold() } else { st })
                    }
                    "USER" => Cell::from(truncate(p.user.as_deref().unwrap_or("-").rsplit('\\').next().unwrap_or("-"), 10)),
                    "TYPE" => Cell::from(p.kind.short()).style(Style::new().fg(match p.kind {
                        ProcKind::Compute => t.accent,
                        ProcKind::Graphics => t.accent2,
                        ProcKind::Both => t.title,
                    })),
                    "GPU-MEM" => {
                        let total = snap_gpus.get(p.gpu).and_then(|g| g.mem_total).unwrap_or(0);
                        match p.gpu_mem {
                            Some(m) => {
                                let c = if total > 0 { t.mem.at(m as f64 / total as f64 * 2.0) } else { t.fg };
                                Cell::from(Line::from(fmt::bytes_short(m)).right_aligned()).style(Style::new().fg(c))
                            }
                            None => Cell::from(Line::from("N/A").right_aligned()).style(Style::new().fg(t.faint)),
                        }
                    }
                    "SM" => match p.sm_util {
                        Some(v) => Cell::from(Line::from(format!("{v:.0}%")).right_aligned()).style(Style::new().fg(t.util.at(v as f64 / 100.0))),
                        None => Cell::from(Line::from("-").right_aligned()).style(Style::new().fg(t.faint)),
                    },
                    "ENC/DEC" => match (p.enc_util, p.dec_util) {
                        (Some(e), Some(d)) => Cell::from(Line::from(format!("{e:.0}/{d:.0}")).right_aligned()),
                        _ => Cell::from(Line::from("-").right_aligned()).style(Style::new().fg(t.faint)),
                    },
                    "CPU" => match p.cpu {
                        Some(v) => Cell::from(Line::from(format!("{v:.1}")).right_aligned()).style(Style::new().fg(t.cpu.at(v as f64 / 100.0))),
                        None => Cell::from(Line::from("-").right_aligned()),
                    },
                    "RAM" => Cell::from(Line::from(p.rss.map(fmt::bytes_short).unwrap_or("-".into())).right_aligned()),
                    "TIME" => Cell::from(Line::from(p.runtime.map(fmt::duration).unwrap_or("-".into())).right_aligned()),
                    "COMMAND" => {
                        let txt = if app.full_cmd { p.cmdline.clone() } else { short_cmd(&p.cmdline, &p.name) };
                        let mut spans = Vec::new();
                        match p.mapping {
                            PidMapping::Translated => spans.push(s("⇄ ", t.accent)),
                            PidMapping::Guessed => spans.push(s("≈ ", t.warn)),
                            _ => {}
                        }
                        spans.push(Span::raw(truncate(&txt, cmd_w as usize)));
                        Cell::from(Line::from(spans))
                    }
                    _ => Cell::from(""),
                };
                cells.push(c);
            }
            Row::new(cells).style(base)
        })
        .collect();

    let empty = body.is_empty();
    drop(rows);
    app.hit.proc_header = hit_header;
    app.hit.proc_header_y = inner.y;
    app.hit.proc_body = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
    let table = Table::new(body, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(t.sel_bg).add_modifier(Modifier::BOLD))
        .highlight_symbol("▌ ")
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always)
        .block(b);
    f.render_stateful_widget(table, area, &mut app.table);
    if empty {
        let msg = if app.info.host.wsl {
            "WSL doesn't expose GPU processes (driver limitation); run on Windows to see them"
        } else if !app.filter.is_empty() {
            "no processes match the filter"
        } else {
            "no GPU processes"
        };
        let r = Rect { y: inner.y + 2, height: 1, ..inner };
        f.render_widget(Paragraph::new(s(msg, t.faint)).centered(), r);
    }
}

/// Program basename + args, without the long path.
fn short_cmd(cmd: &str, name: &str) -> String {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return name.to_string();
    }
    // Windows paths may contain spaces; find the executable by name.
    let lower = cmd.to_lowercase();
    if let Some(pos) = lower.find(&name.to_lowercase()) {
        let rest = &cmd[pos..];
        return rest.to_string();
    }
    let (exe, args) = cmd.split_once(' ').unwrap_or((cmd, ""));
    let base = exe.rsplit(['/', '\\']).next().unwrap_or(exe);
    if args.is_empty() { base.to_string() } else { format!("{base} {args}") }
}

// ---------------------------------------------------------------- charts view

fn draw_charts(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme();
    let n = app.info.gpus.len();
    if n == 0 {
        draw_no_gpus(f, app, area);
        return;
    }
    let i = app.sel_gpu.min(n - 1);
    let g = app.info.gpus[i].clone();
    let sm = app.snap.as_ref().and_then(|s| s.gpus.get(i)).cloned().unwrap_or_default();

    // GPU selector strip when there are several.
    let area = if n > 1 {
        let [sel, rest] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        let mut x = sel.x + 1;
        for j in 0..n {
            let label = format!(" {} {} ", j, truncate(app.info.gpus[j].short_name(), 18));
            let w = label.chars().count() as u16;
            if x + w > sel.x + sel.width {
                break;
            }
            let r = Rect { x, y: sel.y, width: w, height: 1 };
            let st = if j == i { Style::new().fg(Color::Black).bg(t.accent).bold() } else { Style::new().fg(t.dim) };
            f.render_widget(Paragraph::new(label).style(st), r);
            app.hit.gpus.push((r, j));
            x += w + 1;
        }
        rest
    } else {
        area
    };

    let h = &app.hist[i];
    struct C<'a> {
        title: String,
        value: String,
        data: &'a super::Ring,
        mirror: Option<&'a super::Ring>,
        grad: Gradient,
        grad2: Gradient,
        scale: Scale,
    }
    #[derive(Clone, Copy)]
    enum Scale {
        Unit,
        Auto,
    }
    let charts = vec![
        C { title: "GPU utilization".into(), value: pct(sm.util_gpu), data: &h.util, mirror: None, grad: t.util, grad2: t.util, scale: Scale::Unit },
        C {
            title: "Memory".into(),
            value: match (sm.mem_used, sm.mem_total) {
                (Some(u), Some(tot)) => format!("{} / {}", fmt::bytes(u), fmt::bytes(tot)),
                _ => "N/A".into(),
            },
            data: &h.mem,
            mirror: None,
            grad: t.mem,
            grad2: t.mem,
            scale: Scale::Unit,
        },
        C {
            title: "Power".into(),
            value: match (sm.power, sm.power_limit) {
                (Some(p), Some(l)) => format!("{p:.0} W / {l:.0} W"),
                (Some(p), None) => format!("{p:.0} W"),
                _ => "N/A".into(),
            },
            data: &h.power,
            mirror: None,
            grad: t.power,
            grad2: t.power,
            scale: Scale::Unit,
        },
        C {
            title: "Temperature".into(),
            value: sm.temp.map(|v| format!("{v}°C")).unwrap_or("N/A".into()),
            data: &h.temp,
            mirror: None,
            grad: t.temp,
            grad2: t.temp,
            scale: Scale::Unit,
        },
        C {
            title: "SM clock".into(),
            value: match (sm.clock_sm.or(sm.clock_graphics), g.max_clock_sm.or(g.max_clock_graphics)) {
                (Some(c), Some(m)) => format!("{c} / {m} MHz"),
                (Some(c), None) => format!("{c} MHz"),
                _ => "N/A".into(),
            },
            data: &h.clock,
            mirror: None,
            grad: t.util,
            grad2: t.util,
            scale: Scale::Unit,
        },
        C {
            title: "PCIe ▼rx ▲tx".into(),
            value: match (sm.pcie_rx, sm.pcie_tx) {
                (Some(r), Some(x)) => format!("▼{} ▲{}", fmt::rate(r), fmt::rate(x)),
                _ => "N/A".into(),
            },
            data: &h.rx,
            mirror: Some(&h.tx),
            grad: t.mem,
            grad2: t.power,
            scale: Scale::Auto,
        },
    ];
    let (cols, rows) = if area.width >= 150 { (3, 2) } else if area.width >= 90 { (2, 3) } else { (1, 6) };
    let row_areas = Layout::vertical(vec![Constraint::Fill(1); rows]).split(area);
    for (k, c) in charts.iter().enumerate() {
        let (r, col) = (k / cols, k % cols);
        if r >= rows {
            break;
        }
        let cell = Layout::horizontal(vec![Constraint::Fill(1); cols]).split(row_areas[r])[col];
        let b = block(t, false)
            .title_top(Line::from(vec![s(format!(" {} ", c.title), t.title).bold()]))
            .title_top(Line::from(s(format!(" {} ", c.value), t.fg).bold()).right_aligned());
        let inner = b.inner(cell);
        let w = inner.width as usize * 2;
        let raw = c.data.tail(w);
        let stats = {
            let vals: Vec<f64> = raw.iter().flatten().copied().collect();
            if vals.is_empty() {
                None
            } else {
                Some((vals.iter().sum::<f64>() / vals.len() as f64, vals.iter().cloned().fold(f64::MIN, f64::max)))
            }
        };
        let secs = (w as f64 * app.interval.as_secs_f64()) as u64;
        let b = b.title_bottom(Line::from(s(format!(" {} ", fmt::duration(secs)), t.faint)));
        let b = match (stats, c.scale) {
            (Some((avg, max)), Scale::Unit) => {
                b.title_bottom(Line::from(s(format!(" avg {:.0}%  max {:.0}% ", avg * 100.0, max * 100.0), t.dim)).right_aligned())
            }
            _ => b,
        };
        f.render_widget(b, cell);
        match (c.mirror, c.scale) {
            (Some(m), _) => {
                let mraw = m.tail(w);
                let max = raw.iter().chain(mraw).flatten().cloned().fold(1.0f64, f64::max);
                let norm = |v: &[Option<f64>]| v.iter().map(|x| x.map(|x| x / max)).collect::<Vec<_>>();
                let (a, bb) = (norm(raw), norm(mraw));
                let up_h = inner.height.div_ceil(2);
                f.render_widget(AreaChart { data: &a, grad: c.grad, inverted: false }, Rect { height: up_h, ..inner });
                f.render_widget(
                    AreaChart { data: &bb, grad: c.grad2, inverted: true },
                    Rect { y: inner.y + up_h, height: inner.height - up_h, ..inner },
                );
                let lbl = format!("peak {}", fmt::rate(max as u64));
                let lw = lbl.chars().count() as u16;
                if inner.width > lw {
                    f.render_widget(Paragraph::new(s(lbl, t.faint)), Rect { x: inner.x + inner.width - lw, width: lw, height: 1, ..inner });
                }
            }
            (None, _) => f.render_widget(AreaChart { data: raw, grad: c.grad, inverted: false }, inner),
        }
    }
}

// ---------------------------------------------------------------- details view

fn draw_details(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme();
    let n = app.info.gpus.len();
    if n == 0 {
        draw_no_gpus(f, app, area);
        return;
    }
    let i = app.sel_gpu.min(n - 1);
    let g = &app.info.gpus[i];
    let sm = app.snap.as_ref().and_then(|s| s.gpus.get(i)).cloned().unwrap_or_default();
    let na = || "N/A".to_string();
    let o = |v: Option<String>| v.unwrap_or_else(na);
    let b = |v: Option<bool>| v.map(|x| if x { "enabled" } else { "disabled" }.to_string()).unwrap_or_else(na);
    let backend = app.info.backends.first();

    let left: Vec<(&str, String)> = vec![
        ("Name", g.name.clone()),
        ("Vendor", g.vendor.label().into()),
        ("Architecture", o(g.architecture.clone())),
        ("Compute capability", o(g.compute_capability.clone())),
        ("CUDA cores", o(g.cores.map(|c| c.to_string()))),
        ("UUID", o(g.uuid.clone())),
        ("PCI bus", o(g.pci_bus_id.clone())),
        ("Serial", o(g.serial.clone())),
        ("VBIOS", o(g.vbios.clone())),
        ("Driver", o(backend.and_then(|b| b.driver_version.clone()))),
        ("CUDA (driver)", o(backend.and_then(|b| b.runtime_version.clone()))),
        ("NVML", o(backend.and_then(|b| b.api_version.clone()))),
        ("Library", o(backend.and_then(|b| b.library.clone()))),
        ("Persistence mode", b(g.persistence_mode)),
        ("Display active", b(g.display_active)),
        ("Compute mode", o(g.compute_mode.clone())),
        ("MIG", b(g.mig_enabled)),
        ("Fans", g.num_fans.to_string()),
        ("PCIe max", match (g.pcie_max_gen, g.pcie_max_width) {
            (Some(a), Some(w)) => format!("Gen{a} ×{w}"),
            _ => na(),
        }),
        ("Power limits", match (g.power_limit_min, g.power_limit_default, g.power_limit_max) {
            (Some(a), Some(d), Some(m)) => format!("{a:.0} W min · {d:.0} W default · {m:.0} W max"),
            _ => o(g.power_limit_default.map(|d| format!("{d:.0} W default"))),
        }),
        ("Temp thresholds", format!(
            "slowdown {} · shutdown {} · max op {}",
            fmt::opt(g.temp_slowdown, "°C"),
            fmt::opt(g.temp_shutdown, "°C"),
            fmt::opt(g.temp_max_operating, "°C")
        )),
        ("Max clocks", format!(
            "gfx {} · sm {} · mem {} · video {}",
            fmt::opt(g.max_clock_graphics, ""),
            fmt::opt(g.max_clock_sm, ""),
            fmt::opt(g.max_clock_mem, ""),
            fmt::opt(g.max_clock_video, "")
        )),
    ];
    let right: Vec<(&str, String)> = vec![
        ("Utilization", format!("gpu {} · memory bw {}", pct(sm.util_gpu), pct(sm.util_mem))),
        ("Memory", match (sm.mem_used, sm.mem_total) {
            (Some(u), Some(tot)) => format!(
                "{} used / {} total{}",
                fmt::bytes(u),
                fmt::bytes(tot),
                sm.mem_reserved.map(|r| format!(" ({} reserved)", fmt::bytes(r))).unwrap_or_default()
            ),
            _ => na(),
        }),
        ("BAR1", match (sm.bar1_used, sm.bar1_total) {
            (Some(u), Some(tot)) => format!("{} / {}", fmt::bytes(u), fmt::bytes(tot)),
            _ => na(),
        }),
        ("Temperature", format!("{}{}", fmt::opt(sm.temp, "°C"), sm.temp_mem.map(|m| format!(" · memory {m}°C")).unwrap_or_default())),
        ("Fan", if sm.fans.is_empty() { na() } else { sm.fans.iter().map(|f| format!("{f}%")).collect::<Vec<_>>().join(" · ") }),
        ("Power", match (sm.power, sm.power_limit) {
            (Some(p), Some(l)) => format!("{p:.1} W / {l:.0} W limit"),
            (Some(p), None) => format!("{p:.1} W"),
            _ => na(),
        }),
        ("Energy", sm.energy_joules.map(|j| format!("{:.3} kWh since driver load", j / 3.6e6)).unwrap_or_else(na)),
        ("Performance state", sm.pstate.map(|p| format!("P{p}")).unwrap_or_else(na)),
        ("Clocks", format!(
            "gfx {} · sm {} · mem {} · video {} MHz",
            fmt::opt(sm.clock_graphics, ""),
            fmt::opt(sm.clock_sm, ""),
            fmt::opt(sm.clock_mem, ""),
            fmt::opt(sm.clock_video, "")
        )),
        ("PCIe link", match (sm.pcie_gen, sm.pcie_width) {
            (Some(a), Some(w)) => format!("Gen{a} ×{w}"),
            _ => na(),
        }),
        ("PCIe throughput", match (sm.pcie_rx, sm.pcie_tx) {
            (Some(r), Some(x)) => format!("rx {} · tx {}", fmt::rate(r), fmt::rate(x)),
            _ => na(),
        }),
        ("Encoder / decoder", format!("enc {} · dec {}", pct(sm.enc_util), pct(sm.dec_util))),
        ("JPEG / OFA", format!("jpg {} · ofa {}", pct(sm.jpg_util), pct(sm.ofa_util))),
        ("Clock limiters", if sm.throttle.is_empty() { "none".into() } else { sm.throttle.join(", ") }),
        ("ECC errors (volatile)", match (sm.ecc_corrected, sm.ecc_uncorrected) {
            (Some(c), Some(u)) => format!("{c} corrected · {u} uncorrected"),
            _ => na(),
        }),
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
    f.render_widget(Paragraph::new(kv(left)).wrap(Wrap { trim: false }).block(block(t, false).title_top(title("Device"))), l);
    f.render_widget(Paragraph::new(kv(right)).wrap(Wrap { trim: false }).block(block(t, false).title_top(title("Live"))), r);
}

// ---------------------------------------------------------------- popups

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn draw_help(f: &mut Frame, app: &App) {
    let t = app.theme();
    let keys: &[(&str, &str)] = &[
        ("1 2 3 / Tab", "overview · charts · details"),
        ("← → [ ]", "select GPU (or click a card)"),
        ("↑ ↓ j k PgUp PgDn", "move in process list (or wheel / click)"),
        ("s / S", "next / previous sort column (or click header)"),
        ("r", "reverse sort"),
        ("/", "filter by command, user or PID (Esc clears)"),
        ("g", "only show processes on the selected GPU"),
        ("c", "toggle full command line"),
        ("enter", "process details"),
        ("space", "tag process (for multi-kill)"),
        ("K  Del  F9", "signal tagged / selected process"),
        ("t", "cycle theme"),
        ("+ / -", "slower / faster refresh"),
        ("p", "pause sampling"),
        ("q  Esc  Ctrl-C", "quit"),
    ];
    let mut lines: Vec<Line> = keys.iter().map(|(k, d)| Line::from(vec![s(format!("{k:>18}  "), t.accent).bold(), s(*d, t.fg)])).collect();
    lines.push(Line::default());
    lines.push(Line::from(vec![s("               PID ", t.dim), s("h123", t.faint), s("  driver PID outside this namespace", t.dim)]));
    lines.push(Line::from(vec![s("                   ", t.dim), s("⇄", t.accent), s("  translated from a host PID", t.dim)]));
    lines.push(Line::from(vec![s("                   ", t.dim), s("≈ ~", t.warn), s("  matched heuristically", t.dim)]));
    lines.push(Line::default());
    lines.push(Line::from(s(format!("{APP_NAME} {APP_VERSION} · theme {} · {}", t.name, if app.truecolor { "truecolor" } else { "256 colors" }), t.faint)).centered());
    let r = centered(f.area(), 72, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(block(t, true).title_top(Line::from(s(" keys ", t.title).bold()))), r);
}

fn draw_process(f: &mut Frame, app: &App) {
    let t = app.theme();
    let Some(Popup::Process { gpu, pid }) = &app.popup else { return };
    let Some(p) = app.snap.as_ref().and_then(|s| s.processes.iter().find(|p| p.gpu == *gpu && p.pid == *pid)) else {
        return;
    };
    let area = f.area();
    let w = area.width.saturating_sub(8).min(110);
    let r = centered(area, w, 22.min(area.height));
    f.render_widget(Clear, r);
    let b = block(t, true).title_top(Line::from(vec![
        s(format!(" {} ", p.name), t.title).bold(),
        s(format!("pid {} · gpu {} ", p.local_pid.unwrap_or(p.pid), p.gpu), t.dim),
    ]));
    let inner = b.inner(r).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(b, r);

    let mapping = match p.mapping {
        PidMapping::Direct => "same PID namespace".to_string(),
        PidMapping::Translated => format!("driver PID {} → local PID {} (container)", p.pid, p.local_pid.unwrap_or(0)),
        PidMapping::Guessed => format!("driver PID {} → local PID {} (heuristic match)", p.pid, p.local_pid.unwrap_or(0)),
        PidMapping::External => format!("driver PID {} is outside this PID namespace", p.pid),
    };
    let kv = |k: &str, v: String| Line::from(vec![s(format!("{k:<10}"), t.dim), s(v, t.fg)]);
    let mut lines = vec![
        kv("user", p.user.clone().unwrap_or_else(|| "-".into())),
        kv("type", match p.kind {
            ProcKind::Compute => "compute",
            ProcKind::Graphics => "graphics",
            ProcKind::Both => "compute + graphics",
        }
        .into()),
        kv("pid", mapping),
        kv("gpu mem", p.gpu_mem.map(fmt::bytes).unwrap_or_else(|| "N/A".into())),
        kv("gpu util", format!("sm {} · mem {} · enc {} · dec {}", pct(p.sm_util), pct(p.mem_util), pct(p.enc_util), pct(p.dec_util))),
        kv("host", format!(
            "cpu {} · ram {} · running {}",
            p.cpu.map(|c| format!("{c:.1}%")).unwrap_or("-".into()),
            p.rss.map(fmt::bytes).unwrap_or("-".into()),
            p.runtime.map(fmt::duration).unwrap_or("-".into())
        )),
        Line::default(),
    ];
    lines.push(Line::from(s("command", t.dim)));
    let cmd_lines = (p.cmdline.chars().count() as u16).div_ceil(inner.width.max(1)).clamp(1, 4);
    let text_h = lines.len() as u16;
    f.render_widget(Paragraph::new(lines), Rect { height: text_h, ..inner });
    let cmd_area = Rect { y: inner.y + text_h, height: cmd_lines, ..inner };
    f.render_widget(Paragraph::new(s(p.cmdline.clone(), t.accent)).wrap(Wrap { trim: false }), cmd_area);

    // History sparklines.
    let top = cmd_area.y + cmd_area.height + 1;
    if top + 3 > inner.y + inner.height {
        return;
    }
    let chart = Rect { y: top, height: inner.y + inner.height - top, ..inner };
    let [c1, c2] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).spacing(2).areas(chart);
    let Some(h) = app.proc_hist.get(&(*gpu, *pid)) else { return };
    let n = c1.width as usize * 2;
    let mem = h.mem.tail(n);
    let peak = mem.iter().flatten().cloned().fold(0.0f64, f64::max);
    let total = app.snap.as_ref().and_then(|s| s.gpus.get(*gpu)).and_then(|g| g.mem_total).map(|v| v as f64);
    let scale = total.unwrap_or(peak).max(1.0);
    let memn: Vec<Option<f64>> = mem.iter().map(|v| v.map(|x| x / scale)).collect();
    let label = |f: &mut Frame, a: Rect, txt: String| f.render_widget(Paragraph::new(s(txt, t.dim)), Rect { height: 1, ..a });
    label(f, c1, format!("gpu memory  peak {}", if peak > 0.0 { fmt::bytes(peak as u64) } else { "N/A".into() }));
    label(f, c2, "gpu utilization".into());
    let body = |a: Rect| Rect { y: a.y + 1, height: a.height.saturating_sub(1), ..a };
    f.render_widget(AreaChart { data: &memn, grad: t.mem, inverted: false }, body(c1));
    f.render_widget(AreaChart { data: h.sm.tail(c2.width as usize * 2), grad: t.util, inverted: false }, body(c2));
}

fn draw_kill(f: &mut Frame, app: &App) {
    let t = app.theme();
    let Some(Popup::Kill { targets, signal }) = &app.popup else { return };
    let mut lines = vec![Line::from(s(format!("Send a signal to {} process{}:", targets.len(), if targets.len() == 1 { "" } else { "es" }), t.fg)), Line::default()];
    for (gpu, _, local, name) in targets.iter().take(8) {
        lines.push(Line::from(vec![s(format!("  gpu{gpu} "), t.accent), s(format!("{local:>7} "), t.fg).bold(), s(truncate(name, 40), t.dim)]));
    }
    if targets.len() > 8 {
        lines.push(Line::from(s(format!("  … and {} more", targets.len() - 8), t.dim)));
    }
    lines.push(Line::default());
    let mut sigs = vec![Span::raw("  ")];
    for (k, sg) in SIGNALS.iter().enumerate() {
        let label = if cfg!(windows) && k > 0 { "terminate".to_string() } else { sg.label().to_string() };
        let label = if cfg!(windows) && k == 0 { "ctrl-c".to_string() } else { label };
        let st = if k == *signal { Style::new().fg(Color::Black).bg(if k == 2 { t.crit } else { t.warn }).bold() } else { Style::new().fg(t.dim) };
        sigs.push(Span::styled(format!(" {label} "), st));
        sigs.push(Span::raw(" "));
    }
    lines.push(Line::from(sigs));
    lines.push(Line::default());
    lines.push(Line::from(vec![s("  ←→ ", t.accent), s("choose  ", t.dim), s("enter ", t.accent), s("send  ", t.dim), s("esc ", t.accent), s("cancel", t.dim)]));
    let r = centered(f.area(), 60, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(block(t, true).border_style(Style::new().fg(t.crit)).title_top(Line::from(s(" kill ", t.crit).bold()))), r);
}

