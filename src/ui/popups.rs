//! Help, kill and process-detail popups.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use super::draw::{block, s};
use super::gfx::Series;
use super::{App, Popup};
use crate::brand::{APP_NAME, APP_VERSION};
use crate::fmt;
use crate::host::KillSignal;
use crate::model::{PidMapping, ProcKind};

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

pub(super) fn help(f: &mut Frame, app: &App) {
    let t = app.theme();
    let mut lines: Vec<Line> = app
        .keymap
        .help_rows()
        .into_iter()
        .map(|(k, d)| Line::from(vec![s(format!("{:>22}  ", fmt::truncate(&k, 22)), t.accent).bold(), s(d, t.fg)]))
        .collect();
    lines.push(Line::from(vec![s(format!("{:>22}  ", "esc"), t.accent).bold(), s("clear filter / tags, then quit", t.fg)]));
    lines.push(Line::default());
    let legend =
        |k: &str, v: &str, c, d: &str| Line::from(vec![s(format!("{k:>16} "), t.dim), s(format!("{v:<5}"), c), s(format!("  {d}"), t.dim)]);
    lines.push(legend("PID", "h123", t.faint, "driver PID outside this namespace"));
    lines.push(legend("", "⇄", t.accent, "translated from a host PID"));
    lines.push(legend("", "≈ ~", t.warn, "matched heuristically"));
    lines.push(legend("SYS-MEM", "!1.2G", t.crit, "GPU memory spilled into system RAM"));
    lines.push(Line::default());
    let charts =
        if app.chart_ctx().pixels() { format!("pixels ({})", app.gfx.protocol_name()) } else { app.chart_style.label().to_string() };
    let colors = if app.truecolor { "truecolor" } else { "256 colors" };
    lines.push(Line::from(s(format!("{APP_NAME} {APP_VERSION} · theme {} · {colors} · charts {charts}", t.name), t.faint)).centered());
    let r = centered(f.area(), 84, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(block(t, true).title_top(Line::from(s(" keys ", t.title).bold()))), r);
}

pub(super) fn kill(f: &mut Frame, app: &App) {
    let t = app.theme();
    let Some(Popup::Kill { targets, signal }) = &app.popup else { return };
    let plural = if targets.len() == 1 { "" } else { "es" };
    let mut lines = vec![Line::from(s(format!("Send a signal to {} process{plural}:", targets.len()), t.fg)), Line::default()];
    for (gpu, _, local, name) in targets.iter().take(8) {
        lines.push(Line::from(vec![
            s(format!("  gpu{gpu} "), t.accent),
            s(format!("{local:>7} "), t.fg).bold(),
            s(fmt::truncate(name, 40), t.dim),
        ]));
    }
    if targets.len() > 8 {
        lines.push(Line::from(s(format!("  … and {} more", targets.len() - 8), t.dim)));
    }
    lines.push(Line::default());
    let mut sigs = vec![Span::raw("  ")];
    for (k, sg) in KillSignal::available().iter().enumerate() {
        let hot = if *sg == KillSignal::Kill { t.crit } else { t.warn };
        let st = if k == *signal { Style::new().fg(Color::Black).bg(hot).bold() } else { Style::new().fg(t.dim) };
        sigs.push(Span::styled(format!(" {} ", sg.label()), st));
        sigs.push(Span::raw(" "));
    }
    lines.push(Line::from(sigs));
    lines.push(Line::default());
    let mut hint = vec![s("  enter ", t.accent), s("send  ", t.dim), s("esc ", t.accent), s("cancel", t.dim)];
    if KillSignal::available().len() > 1 {
        hint.splice(0..0, [s("  ←→ ", t.accent), s("choose", t.dim)]);
    }
    lines.push(Line::from(hint));
    let r = centered(f.area(), 60, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(block(t, true).border_style(Style::new().fg(t.crit)).title_top(Line::from(s(" kill ", t.crit).bold()))),
        r,
    );
}

pub(super) fn process(f: &mut Frame, app: &App) {
    let t = app.theme();
    let Some(Popup::Process { key, details, full_env }) = &app.popup else { return };
    let Some(p) = app.row_by_key(*key) else { return };
    let u = &p.usage;
    let area = f.area();
    let r = centered(area, area.width.saturating_sub(8).min(120), area.height.saturating_sub(2).min(36));
    f.render_widget(Clear, r);
    let b = block(t, true)
        .title_top(Line::from(vec![
            s(format!(" {} ", p.name), t.title).bold(),
            s(format!("pid {} · gpu {} ", p.local_pid.unwrap_or(p.pid()), p.gpu), t.dim),
        ]))
        .title_bottom(
            Line::from(vec![
                s(" e ", t.accent).bold(),
                s("environment  ", t.dim),
                s("K ", t.accent).bold(),
                s("kill  ", t.dim),
                s("esc ", t.accent).bold(),
                s("close ", t.dim),
            ])
            .right_aligned(),
        );
    let inner = b.inner(r).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(b, r);
    let w = inner.width as usize;

    let mapping = match p.mapping {
        PidMapping::Direct => "same PID namespace".to_string(),
        PidMapping::Translated => format!("driver PID {} → local PID {} (container)", p.pid(), p.local_pid.unwrap_or(0)),
        PidMapping::Guessed => format!("driver PID {} → local PID {} (heuristic match)", p.pid(), p.local_pid.unwrap_or(0)),
        PidMapping::External => format!("driver PID {} is outside this PID namespace", p.pid()),
    };
    let kv = |k: &str, v: String| Line::from(vec![s(format!("{k:<10}"), t.dim), s(fmt::truncate(&v, w.saturating_sub(10)), t.fg)]);
    let kind = match u.kind {
        ProcKind::Compute => "compute",
        ProcKind::Graphics => "graphics",
        ProcKind::Both => "compute + graphics",
    };
    let mut lines = vec![kv("user", p.user.clone().unwrap_or_else(|| "-".into())), kv("type", kind.into()), kv("pid", mapping)];
    if let Some(c) = &p.container {
        lines.push(kv("container", c.clone()));
    }
    if let Some(h) = app.hist.procs.get(key).filter(|h| h.energy > 0.0) {
        let cost = app.price_per_kwh.map(|pr| format!(" · ≈{:.3}", h.energy / 3.6e6 * pr)).unwrap_or_default();
        lines.push(kv("energy", format!("≈{:.1} Wh while watched (share of GPU power by SM use){cost}", h.energy / 3600.0)));
    }
    lines.push(kv("gpu mem", u.gpu_mem.map(fmt::bytes).unwrap_or_else(|| "N/A".into())));
    if let Some(sh) = u.gpu_mem_shared {
        lines.push(if p.spilling {
            Line::from(vec![
                s(format!("{:<10}", "sys mem"), t.dim),
                s(format!("{} spilled out of VRAM, reached over PCIe", fmt::bytes(sh)), t.crit).bold(),
            ])
        } else {
            kv("sys mem", fmt::bytes(sh))
        });
    }
    lines.push(kv(
        "gpu util",
        format!("sm {} · mem {} · enc {} · dec {}", fmt::pct(u.sm_util), fmt::pct(u.mem_util), fmt::pct(u.enc_util), fmt::pct(u.dec_util)),
    ));
    lines.push(kv(
        "host",
        format!(
            "cpu {} · ram {} · running {}",
            p.cpu.map(|c| format!("{c:.1}%")).unwrap_or("-".into()),
            p.rss.map(fmt::bytes).unwrap_or("-".into()),
            p.runtime.map(fmt::duration).unwrap_or("-".into())
        ),
    ));
    if let Some(d) = details {
        // Ancestry, trimmed from the root end when it doesn't fit.
        let chain: Vec<String> = d.ancestors.iter().map(|(pid, n)| format!("{n}({pid})")).collect();
        let mut tree = chain.join(" › ");
        let room = w.saturating_sub(10);
        if tree.chars().count() > room {
            let tail: String = tree.chars().rev().take(room.saturating_sub(1)).collect::<Vec<_>>().into_iter().rev().collect();
            tree = format!("…{tail}");
        }
        lines.push(kv("tree", tree));
        if !d.children.is_empty() {
            let names: Vec<String> = d.children.iter().take(6).map(|(pid, n)| format!("{n}({pid})")).collect();
            let more = d.children.len().saturating_sub(6);
            lines.push(kv("children", format!("{}{}", names.join(", "), if more > 0 { format!(" +{more}") } else { String::new() })));
        }
        if let Some(e) = &d.exe {
            lines.push(kv("exe", e.clone()));
        }
        if let Some(c) = &d.cwd {
            lines.push(kv("cwd", c.clone()));
        }
    }
    lines.push(Line::from(s("command", t.dim)));
    let cmd_rows = (p.cmdline.chars().count()).div_ceil(w.max(1)).clamp(1, 3);
    let wrapped: Vec<String> = p.cmdline.chars().collect::<Vec<_>>().chunks(w.max(1)).take(cmd_rows).map(|c| c.iter().collect()).collect();
    for l in wrapped {
        lines.push(Line::from(s(l, t.accent)));
    }

    // Environment: GPU-relevant variables, or everything with `e`.
    if let Some(d) = details {
        let gpu_vars = d.env.iter().filter(|(k, _)| crate::host::is_gpu_env(k)).count();
        let header = if d.env.is_empty() {
            "environment (not readable)".to_string()
        } else if *full_env {
            format!("environment · all {} variables", d.env.len())
        } else {
            format!("environment · {gpu_vars} GPU-related of {}", d.env.len())
        };
        lines.push(Line::default());
        lines.push(Line::from(s(header, t.dim)));
        for (k, v) in d.env.iter().filter(|(k, _)| *full_env || crate::host::is_gpu_env(k)) {
            let c = if crate::host::is_gpu_env(k) { t.accent2 } else { t.fg };
            let text = fmt::truncate(&format!("{k}={v}"), w);
            lines.push(Line::from(s(text, c)));
        }
    }

    // History charts below, if there's room.
    let text_h = (lines.len() as u16).min(inner.height);
    f.render_widget(Paragraph::new(lines), Rect { height: text_h, ..inner });
    let top = inner.y + text_h + 1;
    let bottom = inner.y + inner.height;
    let Some(h) = app.hist.procs.get(key) else { return };
    if top + 4 > bottom {
        return;
    }
    let chart = Rect { y: top, height: bottom - top, ..inner };
    let [c1, c2] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).spacing(2).areas(chart);
    let ctx = app.chart_ctx();
    let mem = app.hist.tail(&h.mem, ctx.samples(c1.width));
    let peak = mem.iter().flatten().copied().fold(0.0f64, f64::max);
    let scale = app.sample(p.gpu).and_then(|g| g.mem_total).map(|v| v as f64).unwrap_or(peak).max(1.0);
    let memn: Vec<Option<f64>> = mem.iter().map(|v| v.map(|x| x / scale)).collect();
    let label = |f: &mut Frame, a: Rect, txt: String| f.render_widget(Paragraph::new(s(txt, t.dim)), Rect { height: 1, ..a });
    label(f, c1, format!("gpu memory  peak {}", if peak > 0.0 { fmt::bytes(peak as u64) } else { "N/A".into() }));
    label(f, c2, "gpu utilization".into());
    let body = |a: Rect| Rect { y: a.y + 1, height: a.height.saturating_sub(1), ..a };
    ctx.draw(f, body(c1), 900, &[Series { data: &memn, grad: t.mem, down: false }]);
    ctx.draw(f, body(c2), 901, &[Series { data: app.hist.tail(&h.sm, ctx.samples(c2.width)), grad: t.util, down: false }]);
}

pub(super) fn tune(f: &mut Frame, app: &App) {
    let t = app.theme();
    let Some(Popup::Tune { gpu, watts }) = &app.popup else { return };
    let Some(g) = app.gpu(*gpu) else { return };
    let (lo, hi) = (g.power_limit_min.unwrap_or(0.0), g.power_limit_max.unwrap_or(*watts));
    let current = app.sample(*gpu).and_then(|s| s.power_limit);
    let mut lines = vec![
        Line::from(vec![s(format!("GPU {} · ", gpu), t.dim), s(g.short_name().to_string(), t.fg).bold()]),
        Line::default(),
        Line::from(vec![
            s("power limit  ", t.dim),
            s(format!("{watts:.0} W",), t.accent).bold(),
            s(
                format!(
                    "   now {} · default {} · range {lo:.0}-{hi:.0} W",
                    fmt::opt(current.map(|c| c.round()), " W"),
                    fmt::opt(g.power_limit_default.map(|d| d.round()), " W")
                ),
                t.dim,
            ),
        ]),
    ];
    let bar_w = 50usize;
    let frac = if hi > lo { ((watts - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 1.0 };
    let filled = (frac * bar_w as f64).round() as usize;
    lines.push(Line::from(vec![
        s("             ", t.dim),
        s("━".repeat(filled), t.power.at(frac)),
        s("╌".repeat(bar_w - filled), t.faint),
    ]));
    lines.push(Line::default());
    lines.push(Line::from(vec![
        s("  ←→ ", t.accent),
        s("±5 W  ", t.dim),
        s("shift ", t.accent),
        s("±25 W  ", t.dim),
        s("enter ", t.accent),
        s("apply  ", t.dim),
        s("d ", t.accent),
        s("reset to default  ", t.dim),
        s("esc ", t.accent),
        s("cancel", t.dim),
    ]));
    lines.push(Line::from(s("  Needs administrator / root. Resets at reboot.", t.faint)));
    let r = centered(f.area(), 78, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(block(t, true).title_top(Line::from(s(" tune ", t.title).bold()))), r);
}
