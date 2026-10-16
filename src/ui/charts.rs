//! Charts view: big history charts for the selected GPU.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::draw::{block, draw_no_gpus, s};
use super::gfx::Series;
use super::history::Ring;
use super::theme::Gradient;
use super::{App, Hit};
use crate::fmt;

struct Chart<'a> {
    title: &'static str,
    value: String,
    data: &'a Ring,
    grad: Gradient,
    /// A second series in a lane below, auto-scaled together (PCIe rx/tx).
    second: Option<(&'a Ring, Gradient)>,
}

pub(super) fn draw(f: &mut Frame, app: &App, area: Rect, hit: &mut Hit) {
    let t = app.theme();
    let n = app.info.gpus.len();
    if n == 0 {
        draw_no_gpus(f, app, area);
        return;
    }
    let i = app.sel_gpu.min(n - 1);
    let (Some(g), Some(sm), Some(h)) = (app.gpu(i), app.sample(i), app.store.hist.gpus.get(i)) else { return };

    // GPU selector strip when there are several.
    let area = if n > 1 {
        let [sel, rest] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        let mut x = sel.x + 1;
        for (j, gj) in app.info.gpus.iter().enumerate() {
            let label = format!(" {} {} ", j, fmt::truncate(gj.short_name(), 18));
            let w = label.chars().count() as u16;
            if x + w > sel.x + sel.width {
                break;
            }
            let r = Rect { x, y: sel.y, width: w, height: 1 };
            let st = if j == i { Style::new().fg(Color::Black).bg(t.accent).bold() } else { Style::new().fg(t.dim) };
            f.render_widget(Paragraph::new(label).style(st), r);
            hit.gpus.push((r, j));
            x += w + 1;
        }
        rest
    } else {
        area
    };

    let mut charts = vec![
        Chart { title: "GPU utilization", value: fmt::pct(sm.util_gpu), data: &h.util, grad: t.util, second: None },
        Chart {
            title: "Memory",
            value: match (sm.mem_used, sm.mem_total) {
                (Some(u), Some(tot)) => format!("{} / {}", fmt::bytes(u), fmt::bytes(tot)),
                _ => "N/A".into(),
            },
            data: &h.mem,
            grad: t.mem,
            second: None,
        },
        Chart {
            title: "Power",
            value: match (sm.power, sm.power_limit) {
                (Some(p), Some(l)) => format!("{p:.0} W / {l:.0} W"),
                (Some(p), None) => format!("{p:.0} W"),
                _ => "N/A".into(),
            },
            data: &h.power,
            grad: t.power,
            second: None,
        },
        Chart { title: "Temperature", value: fmt::opt(sm.temp, "°C"), data: &h.temp, grad: t.temp, second: None },
        Chart {
            title: "SM clock",
            value: match (sm.clock_sm.or(sm.clock_graphics), g.max_clock_sm.or(g.max_clock_graphics)) {
                (Some(c), Some(m)) => format!("{c} / {m} MHz"),
                (Some(c), None) => format!("{c} MHz"),
                _ => "N/A".into(),
            },
            data: &h.clock,
            grad: t.util,
            second: None,
        },
        Chart {
            title: "PCIe ▼rx ▲tx",
            value: match (sm.pcie_rx, sm.pcie_tx) {
                (Some(r), Some(x)) => format!("▼{} ▲{}", fmt::rate(r), fmt::rate(x)),
                _ => "N/A".into(),
            },
            data: &h.rx,
            grad: t.mem,
            second: Some((&h.tx, t.power)),
        },
    ];
    if let Some(a) = &sm.activity {
        charts.push(Chart { title: "Tensor cores", value: fmt::pct(a.tensor), data: &h.tensor, grad: t.util, second: None });
        charts.push(Chart {
            title: "Memory bandwidth",
            value: match (a.dram_bw, g.memory_bandwidth) {
                (Some(u), Some(peak)) => fmt::bandwidth(peak * u as f64 / 100.0),
                (u, _) => fmt::pct(u),
            },
            data: &h.dram,
            grad: t.mem,
            second: None,
        });
    }

    let cols = if area.width >= 150 {
        3
    } else if area.width >= 90 {
        2
    } else {
        1
    };
    let rows = charts.len().div_ceil(cols);
    let row_areas = Layout::vertical(vec![Constraint::Fill(1); rows]).split(area);
    let ctx = app.chart_ctx();
    for (k, c) in charts.iter().enumerate() {
        let cell = Layout::horizontal(vec![Constraint::Fill(1); cols]).split(row_areas[k / cols])[k % cols];
        let b = block(t, false)
            .title_top(Line::from(s(format!(" {} ", c.title), t.title).bold()))
            .title_top(Line::from(s(format!(" {} ", c.value), t.fg).bold()).right_aligned());
        let inner = b.inner(cell);
        let w = ctx.samples(inner.width);
        let raw = app.store.hist.tail(c.data, w);
        let secs = app.store.hist.span(c.data, w).unwrap_or(w as f64 * app.interval.as_secs_f64()) as u64;
        let b = b.title_bottom(Line::from(s(format!(" {} ", fmt::duration(secs)), t.faint)));
        let slot = 100 + (i as u32) * 10 + k as u32;
        match c.second {
            Some((m, grad2)) => {
                let mraw = app.store.hist.tail(m, w);
                let max = raw.iter().chain(mraw).flatten().copied().fold(1.0f64, f64::max);
                let norm = |v: &[Option<f64>]| v.iter().map(|x| x.map(|x| x / max)).collect::<Vec<_>>();
                let (up, down) = (norm(raw), norm(mraw));
                f.render_widget(b.title_bottom(Line::from(s(format!(" peak {} ", fmt::rate(max as u64)), t.dim)).right_aligned()), cell);
                ctx.draw(f, inner, slot, &[Series { data: &up, grad: c.grad }, Series { data: &down, grad: grad2 }]);
            }
            None => {
                let vals: Vec<f64> = raw.iter().flatten().copied().collect();
                let b = if vals.is_empty() {
                    b
                } else {
                    let avg = vals.iter().sum::<f64>() / vals.len() as f64;
                    let max = vals.iter().copied().fold(f64::MIN, f64::max);
                    b.title_bottom(Line::from(s(format!(" avg {:.0}%  max {:.0}% ", avg * 100.0, max * 100.0), t.dim)).right_aligned())
                };
                f.render_widget(b, cell);
                ctx.draw(f, inner, slot, &[Series { data: raw, grad: c.grad }]);
            }
        }
    }
}
