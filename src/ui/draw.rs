//! Frame layout, header/footer, and helpers shared by the views.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use super::theme::Theme;
use super::{App, Hit, Popup, Tab};
use crate::brand::APP_NAME;
use crate::model::{GpuInfo, GpuSample, SpillLevel};

pub(super) fn draw(f: &mut Frame, app: &App) -> Hit {
    let mut hit = Hit::default();
    let area = f.area();
    let [header, body, footer] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    draw_header(f, app, header, &mut hit);
    match app.tab {
        Tab::Overview => super::overview::draw(f, app, body, &mut hit),
        Tab::Charts => super::charts::draw(f, app, body, &mut hit),
        Tab::Details => super::details::draw(f, app, body),
        Tab::Events => super::timeline::draw(f, app, body),
    }
    draw_footer(f, app, footer);
    if let Some((msg, _, err)) = &app.toast {
        let t = app.theme();
        let w = (msg.chars().count() as u16 + 4).min(area.width);
        let r = Rect { x: area.x + area.width - w, y: footer.y.saturating_sub(1), width: w, height: 1 };
        f.render_widget(Clear, r);
        let bg = if *err { t.crit } else { t.ok };
        f.render_widget(Paragraph::new(format!("  {msg}  ")).style(Style::new().fg(Color::Black).bg(bg).bold()), r);
    }
    match &app.popup {
        Some(Popup::Help) => super::popups::help(f, app),
        Some(Popup::Kill { .. }) => super::popups::kill(f, app),
        Some(Popup::Process { .. }) => super::popups::process(f, app),
        Some(Popup::Tune { .. }) => super::popups::tune(f, app),
        None => {}
    }
    hit
}

// ---------------------------------------------------------------- helpers

pub(super) fn block<'a>(t: &Theme, focused: bool) -> Block<'a> {
    Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(if focused { t.border_hi } else { t.border }))
}

pub(super) fn s<'a>(text: impl Into<std::borrow::Cow<'a, str>>, fg: Color) -> Span<'a> {
    Span::styled(text, Style::new().fg(fg))
}

pub(super) fn temp_color(t: &Theme, temp: u32, info: &GpuInfo) -> Color {
    let slow = info.temp_slowdown.or(info.temp_max_operating).unwrap_or(90) as f64;
    t.temp.at(((temp as f64 - 30.0) / (slow - 30.0)).clamp(0.0, 1.0))
}

/// 0 = fine, 1 = watch, 2 = something is wrong.
pub(super) fn severity(info: &GpuInfo, s: &GpuSample) -> u8 {
    if s.error.is_some() || s.ecc_uncorrected.is_some_and(|e| e > 0) || s.spill.level == SpillLevel::Spilling {
        return 2;
    }
    let slow = info.temp_slowdown.or(info.temp_max_operating).unwrap_or(90);
    if s.temp.is_some_and(|t| t + 3 >= slow) || s.throttle_severity() >= 2 {
        return 2;
    }
    if s.temp.is_some_and(|t| t + 10 >= slow) {
        return 1;
    }
    0
}

pub(super) fn severity_color(t: &Theme, sev: u8, normal: Color) -> Color {
    match sev {
        2 => t.crit,
        1 => t.warn,
        _ => normal,
    }
}

/// One-line spill summary for a GPU, if any.
pub(super) fn spill_text(s: &GpuSample) -> Option<String> {
    match s.spill.level {
        SpillLevel::Spilling => {
            let slow = s.spill.slowdown.filter(|x| *x >= 1.5).map(|x| format!(" · ≈{x:.0}× slower")).unwrap_or_default();
            Some(format!("SPILL {}{slow}", s.spill.reason))
        }
        SpillLevel::Pressure => Some(s.spill.reason.clone()),
        SpillLevel::None => None,
    }
}

/// Program basename + args, without the long path.
pub(super) fn short_cmd(cmd: &str, name: &str) -> String {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return name.to_string();
    }
    // Windows paths contain spaces: find the executable by name. ASCII
    // lowercasing keeps byte offsets valid for slicing `cmd`.
    if let Some(pos) = Some(name).filter(|n| !n.is_empty()).and_then(|n| cmd.to_ascii_lowercase().find(&n.to_ascii_lowercase())) {
        return cmd[pos..].to_string();
    }
    let (exe, args) = cmd.split_once(' ').unwrap_or((cmd, ""));
    let base = exe.rsplit(['/', '\\']).next().unwrap_or(exe);
    if args.is_empty() { base.to_string() } else { format!("{base} {args}") }
}

pub(super) fn draw_no_gpus(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let mut lines = vec![Line::from(s("No GPUs detected", t.warn).bold()), Line::default()];
    for w in &app.info.warnings {
        lines.push(Line::from(s(format!("• {w}"), t.dim)));
    }
    lines.push(Line::default());
    lines.push(Line::from(s(format!("Try `{APP_NAME} --diagnose`, or `{APP_NAME} --demo` to see the UI with simulated GPUs."), t.dim)));
    if app.info.host.container.is_some() {
        lines.push(Line::from(s("In Docker, run with `--gpus all` (and NVIDIA_DRIVER_CAPABILITIES including `utility`).", t.dim)));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block(t, false).title_top(" GPUs ")), area);
}

// ---------------------------------------------------------------- header / footer

fn draw_header(f: &mut Frame, app: &App, area: Rect, hit: &mut Hit) {
    let t = app.theme();
    let mut left = vec![
        Span::styled(format!(" {APP_NAME} "), Style::new().fg(Color::Black).bg(t.title).bold()),
        Span::raw(" "),
        s(app.info.host.hostname.as_str(), t.fg).bold(),
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
            left.push(s(d.as_str(), t.fg));
        }
        if let (Some(n), Some(v)) = (&b.runtime_name, &b.runtime_version) {
            left.push(s(format!("  {n} "), t.dim));
            left.push(s(v.as_str(), t.fg));
        }
    }
    if app.rewind > 0
        && let Some(snap) = &app.snap
    {
        let ago = app.past.back().map(|n| (n.timestamp - snap.timestamp).max(0.0) as u64).unwrap_or(0);
        let when = chrono::DateTime::from_timestamp(snap.timestamp as i64, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
            .unwrap_or_default();
        left.push(Span::raw("  "));
        left.push(Span::styled(
            format!(" ◀ REWIND -{} · {when} ", crate::fmt::duration(ago)),
            Style::new().fg(Color::Black).bg(t.accent2).bold(),
        ));
    }
    let lw: usize = left.iter().map(|s| s.width()).sum();

    let now = chrono::Local::now().format("%H:%M:%S").to_string();
    let rate = format!("{:.2}s", app.interval.as_secs_f64()).replace(".00s", "s");
    let right = if app.paused {
        vec![
            Span::styled(" PAUSED ", Style::new().fg(Color::Black).bg(t.warn).bold()),
            Span::raw("  "),
            s(now, t.fg).bold(),
            Span::raw(" "),
        ]
    } else {
        vec![s("every ", t.faint), s(rate, t.dim), Span::raw("  "), s(now, t.fg).bold(), Span::raw(" ")]
    };
    let rw: usize = right.iter().map(|s| s.width()).sum();

    f.render_widget(Paragraph::new(Line::from(left)), area);
    f.render_widget(Paragraph::new(Line::from(right).right_aligned()), area);

    // Tabs centered in the remaining space.
    let labels: Vec<(String, Tab)> = Tab::ALL.iter().enumerate().map(|(i, tab)| (format!(" {} {} ", i + 1, tab.label()), *tab)).collect();
    let tw: usize = labels.iter().map(|(l, _)| l.chars().count() + 1).sum();
    let free = (area.width as usize).saturating_sub(lw + rw);
    if free >= tw + 2 {
        let mut x = area.x + (lw + (free - tw) / 2) as u16;
        for (label, tab) in labels {
            let w = label.chars().count() as u16;
            let r = Rect { x, y: area.y, width: w, height: 1 };
            let st = if tab == app.tab { Style::new().fg(Color::Black).bg(t.accent).bold() } else { Style::new().fg(t.dim) };
            f.render_widget(Paragraph::new(label).style(st), r);
            hit.tabs.push((r, tab));
            x += w + 1;
        }
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    if let Some(line) = &app.cmd_edit {
        let hint = "theme · sort · filter · hide/show gpus|host|procs · charts · interval · gpu · view · quit";
        f.render_widget(Paragraph::new(Line::from(vec![s(" :", t.accent).bold(), s(line.as_str(), t.fg), s("▏", t.accent)])), area);
        f.render_widget(Paragraph::new(Line::from(s(format!("{hint} "), t.faint)).right_aligned()), area);
        return;
    }
    use super::Action as A;
    let actions: &[(A, &str)] = match app.tab {
        Tab::Overview => &[
            (A::Quit, "quit"),
            (A::Help, "help"),
            (A::Command, "command"),
            (A::NextTab, "view"),
            (A::SortNext, "sort"),
            (A::Filter, "filter"),
            (A::GpuOnly, "this gpu"),
            (A::ProcessDetails, "details"),
            (A::Kill, "kill"),
            (A::Theme, "theme"),
            (A::ChartStyle, "charts"),
            (A::Pause, "pause"),
        ],
        _ => &[
            (A::Quit, "quit"),
            (A::Help, "help"),
            (A::Command, "command"),
            (A::NextTab, "view"),
            (A::Theme, "theme"),
            (A::ChartStyle, "charts"),
            (A::Pause, "pause"),
        ],
    };
    let keys: Vec<(&str, &str)> = actions.iter().map(|(a, d)| (app.keymap.label(*a), *d)).filter(|(k, _)| !k.is_empty()).collect();
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in &keys {
        spans.push(Span::styled(*k, Style::new().fg(t.accent).bold()));
        spans.push(s(format!(" {d}  "), t.dim));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    if !app.info.warnings.is_empty() {
        let line = Line::from(vec![s("! ", t.warn).bold(), s(app.info.warnings.join(" · "), t.warn), Span::raw(" ")]).right_aligned();
        let used: u16 = 1 + keys.iter().map(|(k, d)| (k.chars().count() + d.chars().count() + 3) as u16).sum::<u16>();
        if used + (line.width() as u16) < area.width {
            f.render_widget(Paragraph::new(line), area);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_cmd_handles_paths_and_unicode() {
        assert_eq!(short_cmd(r"C:\Program Files\App\app.exe --x", "app.exe"), "app.exe --x");
        assert_eq!(short_cmd("/usr/bin/python3 train.py", "python3"), "python3 train.py");
        assert_eq!(short_cmd("/opt/x/run --flag", ""), "run --flag");
        // Non-ASCII before the name must not break slicing.
        assert_eq!(short_cmd("/home/İlker/bin/ollama serve", "ollama"), "ollama serve");
        assert_eq!(short_cmd("", "name"), "name");
    }
}
