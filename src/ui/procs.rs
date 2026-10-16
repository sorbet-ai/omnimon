//! The process table.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, HighlightSpacing, Paragraph, Row, Table};

use super::draw::{block, s, short_cmd};
use super::theme::Theme;
use super::{App, Hit, SortKey};
use crate::fmt;
use crate::model::{PidMapping, ProcKind, ProcRow};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
    Gpu,
    Pid,
    User,
    Kind,
    GpuMem,
    SysMem,
    Sm,
    EncDec,
    Cpu,
    Rss,
    Time,
    Command,
}

impl Col {
    fn header(self) -> &'static str {
        match self {
            Col::Gpu => "GPU",
            Col::Pid => "PID",
            Col::User => "USER",
            Col::Kind => "TYPE",
            Col::GpuMem => "GPU-MEM",
            Col::SysMem => "SYS-MEM",
            Col::Sm => "SM",
            Col::EncDec => "ENC/DEC",
            Col::Cpu => "CPU",
            Col::Rss => "RAM",
            Col::Time => "TIME",
            Col::Command => "COMMAND",
        }
    }
    /// 0 = takes the remaining width.
    fn width(self) -> u16 {
        match self {
            Col::Gpu => 3,
            Col::Pid => 8,
            Col::User => 10,
            Col::Kind => 4,
            Col::GpuMem | Col::SysMem => 8,
            Col::Sm => 4,
            Col::EncDec => 7,
            Col::Cpu | Col::Rss => 6,
            Col::Time => 9,
            Col::Command => 0,
        }
    }
    fn sort(self) -> Option<SortKey> {
        Some(match self {
            Col::Gpu => SortKey::Gpu,
            Col::Pid => SortKey::Pid,
            Col::User => SortKey::User,
            Col::GpuMem => SortKey::GpuMem,
            Col::SysMem => SortKey::SharedMem,
            Col::Sm => SortKey::Sm,
            Col::Cpu => SortKey::Cpu,
            Col::Rss => SortKey::Rss,
            Col::Time => SortKey::Time,
            Col::Command => SortKey::Name,
            Col::Kind | Col::EncDec => return None,
        })
    }
    fn right(self) -> bool {
        !matches!(self, Col::User | Col::Kind | Col::Command)
    }
}

/// Per-frame facts the cell renderer needs.
struct Ctx<'a> {
    t: &'a Theme,
    base: Style,
    tagged: bool,
    gpu_total: Option<u64>,
    full_cmd: bool,
    cmd_w: u16,
}

fn rcell<'a>(text: String, fg: Color) -> Cell<'a> {
    Cell::from(Line::from(text).right_aligned()).style(Style::new().fg(fg))
}

fn cell<'a>(col: Col, p: &ProcRow, c: &Ctx) -> Cell<'a> {
    let t = c.t;
    let u = &p.usage;
    let dash = || rcell("-".into(), t.faint);
    match col {
        Col::Gpu => rcell(p.gpu.to_string(), t.accent),
        Col::Pid => {
            let (txt, st) = match p.mapping {
                PidMapping::Direct => (p.pid().to_string(), c.base),
                PidMapping::Translated => (p.local_pid.unwrap_or(p.pid()).to_string(), c.base),
                PidMapping::Guessed => (format!("~{}", p.local_pid.unwrap_or(p.pid())), c.base.fg(t.warn)),
                PidMapping::External => (format!("h{}", p.pid()), c.base),
            };
            let (txt, st) = if c.tagged { (format!("●{txt}"), st.fg(t.accent2).bold()) } else { (txt, st) };
            Cell::from(Line::from(txt).right_aligned()).style(st)
        }
        Col::User => Cell::from(fmt::truncate(p.user.as_deref().unwrap_or("-").rsplit('\\').next().unwrap_or("-"), 10)),
        Col::Kind => Cell::from(u.kind.short()).style(Style::new().fg(match u.kind {
            ProcKind::Compute => t.accent,
            ProcKind::Graphics => t.accent2,
            ProcKind::Both => t.title,
        })),
        Col::GpuMem => match u.gpu_mem {
            Some(m) => {
                let fg = c.gpu_total.filter(|&tot| tot > 0).map(|tot| t.mem.at(m as f64 / tot as f64 * 2.0)).unwrap_or(t.fg);
                rcell(fmt::bytes_short(m), fg)
            }
            None => rcell("N/A".into(), t.faint),
        },
        Col::SysMem => match u.gpu_mem_shared {
            Some(m) if p.spilling => {
                Cell::from(Line::from(format!("!{}", fmt::bytes_short(m))).right_aligned()).style(Style::new().fg(t.crit).bold())
            }
            Some(m) if m >= 1 << 20 => rcell(fmt::bytes_short(m), t.dim),
            _ => dash(),
        },
        Col::Sm => match u.sm_util {
            Some(v) => rcell(format!("{v:.0}%"), t.util.at(v as f64 / 100.0)),
            None => dash(),
        },
        Col::EncDec => match (u.enc_util, u.dec_util) {
            (Some(e), Some(d)) => rcell(format!("{e:.0}/{d:.0}"), t.fg),
            _ => dash(),
        },
        Col::Cpu => match p.cpu {
            Some(v) => rcell(format!("{v:.1}"), t.cpu.at(v as f64 / 100.0)),
            None => dash(),
        },
        Col::Rss => p.rss.map(|v| rcell(fmt::bytes_short(v), t.fg)).unwrap_or_else(dash),
        Col::Time => p.runtime.map(|v| rcell(fmt::duration(v), t.fg)).unwrap_or_else(dash),
        Col::Command => {
            let txt = if c.full_cmd { p.cmdline.clone() } else { short_cmd(&p.cmdline, &p.name) };
            let mut spans = Vec::new();
            match p.mapping {
                PidMapping::Translated => spans.push(s("⇄ ", t.accent)),
                PidMapping::Guessed => spans.push(s("≈ ", t.warn)),
                _ => {}
            }
            if let Some(c) = &p.container {
                let short = c.rsplit(':').next().unwrap_or(c);
                spans.push(s(format!("[{}] ", &short[..short.len().min(8)]), t.accent2));
            }
            spans.push(Span::raw(fmt::truncate(&txt, c.cmd_w as usize)));
            Cell::from(Line::from(spans))
        }
    }
}

pub(super) fn draw(f: &mut Frame, app: &App, area: Rect, hit: &mut Hit) {
    let t = app.theme();
    let shown = app.view.len();
    let total = app.procs().len();
    let mut title = vec![s(" Processes ", t.title).bold(), s(format!("{shown} "), t.dim)];
    if shown != total {
        title.push(s(format!("of {total} "), t.dim));
    }
    if app.gpu_only {
        title.push(s(format!("· GPU {} only ", app.sel_gpu), t.accent));
    }
    if !app.tagged.is_empty() {
        title.push(s(format!("· {} tagged ", app.tagged.len()), t.accent2));
    }
    let mut b = block(t, true).title_top(Line::from(title));
    if app.filter_edit {
        b = b.title_top(Line::from(vec![s(" / ", t.accent).bold(), s(app.spec.filter.as_str(), t.fg), s("▏ ", t.accent)]).right_aligned());
    } else if !app.spec.filter.is_empty() {
        b = b.title_top(Line::from(vec![s(" filter: ", t.dim), s(app.spec.filter.as_str(), t.accent), s(" ", t.dim)]).right_aligned());
    }
    let inner = b.inner(area);

    let wide = inner.width >= 100;
    let mid = inner.width >= 76;
    let has_sys = app.procs().iter().any(|p| p.usage.gpu_mem_shared.is_some());
    let cols: Vec<Col> = [
        (Col::Gpu, true),
        (Col::Pid, true),
        (Col::User, mid),
        (Col::Kind, true),
        (Col::GpuMem, true),
        (Col::SysMem, has_sys && mid),
        (Col::Sm, true),
        (Col::EncDec, wide),
        (Col::Cpu, true),
        (Col::Rss, mid),
        (Col::Time, wide),
        (Col::Command, true),
    ]
    .into_iter()
    .filter_map(|(c, on)| on.then_some(c))
    .collect();

    // 2 columns for the highlight symbol, 1 between columns.
    let fixed: u16 = cols.iter().map(|c| c.width() + 1).sum::<u16>() + 2;
    let cmd_w = inner.width.saturating_sub(fixed).max(10);
    let width_of = |c: Col| if c.width() == 0 { cmd_w } else { c.width() };

    let arrow = if app.spec.desc { "▼" } else { "▲" };
    let header = Row::new(cols.iter().map(|&c| {
        let active = c.sort() == Some(app.spec.sort);
        let label = if active { format!("{}{arrow}", c.header()) } else { c.header().to_string() };
        let line = Line::from(label).style(if active { Style::new().fg(t.accent).bold() } else { Style::new().fg(t.dim).bold() });
        Cell::from(if c.right() { line.right_aligned() } else { line })
    }));
    let mut x = inner.x + 2;
    for &c in &cols {
        let w = width_of(c);
        if let Some(k) = c.sort() {
            hit.proc_header.push((x, x + w, k));
        }
        x += w + 1;
    }
    hit.proc_header_y = inner.y;
    hit.proc_body = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };

    let body: Vec<Row> = app
        .rows()
        .map(|p| {
            let mine = match (&p.user, &app.me) {
                (Some(u), Some(m)) => u.eq_ignore_ascii_case(m) || u.to_lowercase().ends_with(&format!("\\{}", m.to_lowercase())),
                _ => false,
            };
            let base = if p.mapping == PidMapping::External {
                Style::new().fg(t.faint).add_modifier(Modifier::ITALIC)
            } else if mine || p.user.is_none() {
                Style::new().fg(t.fg)
            } else {
                Style::new().fg(t.dim)
            };
            let ctx = Ctx {
                t,
                base,
                tagged: app.tagged.contains(&p.key()),
                gpu_total: app.sample(p.gpu).and_then(|g| g.mem_total),
                full_cmd: app.full_cmd,
                cmd_w,
            };
            Row::new(cols.iter().map(|&c| cell(c, p, &ctx))).style(base)
        })
        .collect();

    let empty = body.is_empty();
    let table = Table::new(body, cols.iter().map(|&c| Constraint::Length(width_of(c))))
        .header(header)
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(t.sel_bg).add_modifier(Modifier::BOLD))
        .highlight_symbol("▌ ")
        .highlight_spacing(HighlightSpacing::Always)
        .block(b);
    f.render_stateful_widget(table, area, &mut app.table.borrow_mut());
    if empty {
        let msg = if app.info.host.wsl {
            "WSL doesn't expose GPU processes (driver limitation); run on Windows to see them"
        } else if !app.spec.filter.is_empty() {
            "no processes match the filter"
        } else {
            "no GPU processes"
        };
        if inner.height > 2 {
            f.render_widget(Paragraph::new(s(msg, t.faint)).centered(), Rect { y: inner.y + 2, height: 1, ..inner });
        }
    }
}
