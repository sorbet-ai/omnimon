//! Events view: the timeline of notable changes, newest first.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::App;
use super::draw::{block, s};
use crate::events::Severity;

pub(super) fn draw(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let shown = app.store.events.len();
    let b = block(t, true)
        .title_top(Line::from(vec![s(" Events ", t.title).bold(), s(format!("{shown} "), t.dim)]))
        .title_bottom(Line::from(s(" spills, clock limits, errors, process start/exit · newest first ", t.faint)).right_aligned());
    let inner = b.inner(area);
    f.render_widget(b, area);
    if app.store.events.is_empty() {
        f.render_widget(Paragraph::new(s("nothing yet", t.faint)).centered(), Rect { y: inner.y + inner.height / 2, height: 1, ..inner });
        return;
    }
    let lines: Vec<Line> = app
        .store
        .events
        .iter()
        .rev()
        .skip(app.events_scroll)
        .take(inner.height as usize)
        .map(|e| {
            let when = chrono::DateTime::from_timestamp(e.time as i64, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
                .unwrap_or_default();
            let (mark, c) = match e.severity {
                Severity::Crit => ("■", t.crit),
                Severity::Warn => ("■", t.warn),
                Severity::Info => ("·", t.dim),
            };
            let text_c = if e.severity == Severity::Info { t.fg } else { c };
            Line::from(vec![s(format!(" {when} "), t.dim), s(format!("{mark} "), c), s(e.text.as_str(), text_c)])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}
