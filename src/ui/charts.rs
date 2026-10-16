//! Charts view: big history charts for one GPU, or for the host. A strip
//! at the top picks which (left/right arrows, or click).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::draw::{block, draw_no_gpus, s};
use super::gfx::Series;
use super::history::Ring;
use super::theme::Gradient;
use super::widgets::BlockChart;
use super::{App, Hit};
use crate::fmt;

enum Data<'a> {
    /// History, normalized to [0,1].
    Fraction(&'a Ring),
    /// History of a rate (bytes/s), scaled to the window's peak; optionally
    /// with a second rate in a lane below (rx/tx).
    Rate(&'a Ring, Option<(&'a Ring, Gradient)>),
    /// A snapshot, one column per item (per-core load), always as blocks.
    Now(Vec<Option<f64>>),
}

struct Chart<'a> {
    title: &'static str,
    value: String,
    data: Data<'a>,
    grad: Gradient,
}

pub(super) fn draw(f: &mut Frame, app: &App, area: Rect, hit: &mut Hit) {
    let t = app.theme();
    let n = app.info.gpus.len();
    if n == 0 && !app.charts_host {
        draw_no_gpus(f, app, area);
        return;
    }
    let host = app.charts_host || n == 0;
    let i = app.sel_gpu.min(n.saturating_sub(1));

    // Selector: every GPU, then the host.
    let [sel, area] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let mut x = sel.x + 1;
    let entries = app.info.gpus.iter().enumerate().map(|(j, g)| (j, format!(" {} {} ", j, fmt::truncate(g.short_name(), 18))));
    for (j, label) in entries.chain(std::iter::once((n, " host ".to_string()))) {
        let w = label.chars().count() as u16;
        if x + w > sel.x + sel.width {
            break;
        }
        let r = Rect { x, y: sel.y, width: w, height: 1 };
        let active = if j == n { host } else { !host && j == i };
        let st = if active { Style::new().fg(Color::Black).bg(t.accent).bold() } else { Style::new().fg(t.dim) };
        f.render_widget(Paragraph::new(label).style(st), r);
        hit.gpus.push((r, j));
        x += w + 1;
    }

    if host {
        host_charts(f, app, area);
    } else {
        gpu_charts(f, app, area, i);
    }
}

fn gpu_charts(f: &mut Frame, app: &App, area: Rect, i: usize) {
    let t = app.theme();
    let (Some(g), Some(sm), Some(h)) = (app.gpu(i), app.sample(i), app.store.hist.gpus.get(i)) else { return };
    let mut charts = vec![
        Chart { title: "GPU utilization", value: fmt::pct(sm.util_gpu), data: Data::Fraction(&h.util), grad: t.util },
        Chart {
            title: "Memory",
            value: match (sm.mem_used, sm.mem_total) {
                (Some(u), Some(tot)) => format!("{} / {}", fmt::bytes(u), fmt::bytes(tot)),
                _ => "N/A".into(),
            },
            data: Data::Fraction(&h.mem),
            grad: t.mem,
        },
        Chart {
            title: "Power",
            value: match (sm.power, sm.power_limit) {
                (Some(p), Some(l)) => format!("{p:.0} W / {l:.0} W"),
                (Some(p), None) => format!("{p:.0} W"),
                _ => "N/A".into(),
            },
            data: Data::Fraction(&h.power),
            grad: t.power,
        },
        Chart { title: "Temperature", value: fmt::opt(sm.temp, "°C"), data: Data::Fraction(&h.temp), grad: t.temp },
        Chart {
            title: "SM clock",
            value: match (sm.clock_sm.or(sm.clock_graphics), g.max_clock_sm.or(g.max_clock_graphics)) {
                (Some(c), Some(m)) => format!("{c} / {m} MHz"),
                (Some(c), None) => format!("{c} MHz"),
                _ => "N/A".into(),
            },
            data: Data::Fraction(&h.clock),
            grad: t.util,
        },
        Chart {
            title: "PCIe ▼rx ▲tx",
            value: match (sm.pcie_rx, sm.pcie_tx) {
                (Some(r), Some(x)) => format!("▼{} ▲{}", fmt::rate(r), fmt::rate(x)),
                _ => "N/A".into(),
            },
            data: Data::Rate(&h.rx, Some((&h.tx, t.power))),
            grad: t.mem,
        },
    ];
    if let Some(a) = &sm.activity {
        charts.push(Chart { title: "Tensor cores", value: fmt::pct(a.tensor), data: Data::Fraction(&h.tensor), grad: t.util });
        charts.push(Chart {
            title: "Memory bandwidth",
            value: match (a.dram_bw, g.memory_bandwidth) {
                (Some(u), Some(peak)) => fmt::bandwidth(peak * u as f64 / 100.0),
                (u, _) => fmt::pct(u),
            },
            data: Data::Fraction(&h.dram),
            grad: t.mem,
        });
    }
    grid(f, app, area, &charts, 100 + i as u32 * 10);
}

fn host_charts(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let Some(snap) = app.snap() else { return };
    let (hs, hist) = (&snap.host, &app.store.hist);
    let rate = |v: Option<u64>| v.map(fmt::rate).unwrap_or_else(|| "N/A".into());
    let mut charts = vec![
        Chart { title: "CPU", value: format!("{:.0}%", hs.cpu), data: Data::Fraction(&hist.cpu), grad: t.cpu },
        Chart {
            title: "Memory",
            value: format!("{} / {}", fmt::bytes(hs.mem_used), fmt::bytes(hs.mem_total)),
            data: Data::Fraction(&hist.mem),
            grad: t.mem,
        },
        Chart {
            title: "Network ▼rx ▲tx",
            value: format!("▼{} ▲{}", rate(hs.net_rx), rate(hs.net_tx)),
            data: Data::Rate(&hist.net_rx, Some((&hist.net_tx, t.power))),
            grad: t.mem,
        },
        Chart {
            title: "Disk",
            value: format!("r {} w {}", rate(hs.disk_read), rate(hs.disk_write)),
            data: Data::Rate(&hist.disk, None),
            grad: t.power,
        },
    ];
    if hs.cpu_temp.is_some() {
        charts.push(Chart {
            title: "CPU temperature",
            value: hs.cpu_temp.map(|c| format!("{c:.0}°C")).unwrap_or_default(),
            data: Data::Fraction(&hist.cpu_temp),
            grad: t.temp,
        });
    }
    if !hs.per_core.is_empty() {
        let max = hs.per_core.iter().copied().fold(0.0f32, f32::max);
        charts.push(Chart {
            title: "Cores now",
            value: format!("{} cores · busiest {max:.0}%", hs.per_core.len()),
            data: Data::Now(hs.per_core.iter().map(|&c| Some((c as f64 / 100.0).clamp(0.0, 1.0))).collect()),
            grad: t.cpu,
        });
    }
    grid(f, app, area, &charts, 90);
}

/// Lay charts out in a grid (1-3 columns by width) and draw each.
fn grid(f: &mut Frame, app: &App, area: Rect, charts: &[Chart], slot_base: u32) {
    let t = app.theme();
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
        let slot = slot_base + k as u32;
        let footer = |text: String| Line::from(s(text, t.dim)).right_aligned();
        match &c.data {
            Data::Now(values) => {
                // One column per item, spread out when there's room.
                let w = (inner.width as usize / values.len().max(1)).clamp(1, 4);
                f.render_widget(b, cell);
                for (j, v) in values.iter().enumerate() {
                    let x = inner.x + (j * w) as u16;
                    if x + w as u16 > inner.x + inner.width {
                        break;
                    }
                    let col = Rect { x, width: (w as u16).saturating_sub(u16::from(w > 1)).max(1), ..inner };
                    f.render_widget(BlockChart { data: std::slice::from_ref(v), grad: c.grad }, col);
                }
            }
            Data::Fraction(ring) | Data::Rate(ring, _) => {
                let w = ctx.samples(inner.width);
                let raw = app.store.hist.tail(ring, w);
                let secs = app.store.hist.span(ring, w).unwrap_or(w as f64 * app.interval.as_secs_f64()) as u64;
                let b = b.title_bottom(Line::from(s(format!(" {} ", fmt::duration(secs)), t.faint)));
                if let Data::Rate(_, second) = &c.data {
                    let other = second.map(|(r, _)| app.store.hist.tail(r, w)).unwrap_or(&[]);
                    let max = raw.iter().chain(other).flatten().copied().fold(1.0f64, f64::max);
                    let norm = |v: &[Option<f64>]| v.iter().map(|x| x.map(|x| x / max)).collect::<Vec<_>>();
                    let (a, b2) = (norm(raw), norm(other));
                    f.render_widget(b.title_bottom(footer(format!(" peak {} ", fmt::rate(max as u64)))), cell);
                    let mut series = vec![Series { data: &a, grad: c.grad }];
                    if let Some((_, g2)) = second {
                        series.push(Series { data: &b2, grad: *g2 });
                    }
                    ctx.draw(f, inner, slot, &series);
                } else {
                    let vals: Vec<f64> = raw.iter().flatten().copied().collect();
                    let b = if vals.is_empty() {
                        b
                    } else {
                        let avg = vals.iter().sum::<f64>() / vals.len() as f64;
                        let max = vals.iter().copied().fold(f64::MIN, f64::max);
                        b.title_bottom(footer(format!(" avg {:.0}%  max {:.0}% ", avg * 100.0, max * 100.0)))
                    };
                    f.render_widget(b, cell);
                    ctx.draw(f, inner, slot, &[Series { data: raw, grad: c.grad }]);
                }
            }
        }
    }
}
