//! Custom widgets: gradient bars, braille area charts, sparklines.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use super::theme::{Gradient, Look, look, paint};

const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];

/// Horizontal bar with eighth-cell resolution, colored by position along a
/// gradient (so a full bar runs green -> red, btop style).
pub struct GradBar {
    pub frac: Option<f64>,
    pub grad: Gradient,
    pub track: Color,
}

impl Widget for GradBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let w = area.width as usize;
        let y = area.y + area.height / 2;
        let Some(frac) = self.frac else {
            for i in 0..w {
                buf[(area.x + i as u16, y)].set_char('╌').set_style(Style::new().fg(self.track));
            }
            return;
        };
        let frac = frac.clamp(0.0, 1.0);
        let filled = frac * w as f64;
        let last = (filled.ceil() as usize).saturating_sub(1);
        let thin = matches!(look(), Look::Outline | Look::Minimal);
        for i in 0..w {
            let pos = (i as f64 + 0.5) / w as f64;
            let col = paint(self.grad, pos, frac, i == last);
            let cell = &mut buf[(area.x + i as u16, y)];
            let amount = (filled - i as f64).clamp(0.0, 1.0);
            if thin {
                // A line rather than a slab: heavy where filled, light track after.
                let (ch, fg) = if amount > 0.0 { ('━', paint(self.grad, pos, frac, true)) } else { ('─', self.track) };
                cell.set_char(ch).set_style(Style::new().fg(fg));
                continue;
            }
            if amount >= 1.0 {
                cell.set_char('█').set_style(Style::new().fg(col).bg(self.track));
            } else if amount > 0.0 {
                let e = ((amount * 8.0) as usize).max(1);
                cell.set_char(EIGHTHS[e]).set_style(Style::new().fg(col).bg(self.track));
            } else {
                cell.set_char(' ').set_style(Style::new().bg(self.track));
            }
        }
    }
}

/// Braille area chart. `data` is normalized to [0,1], newest last; each
/// cell holds two samples. Rows are colored along a vertical gradient.
pub struct AreaChart<'a> {
    pub data: &'a [Option<f64>],
    pub grad: Gradient,
}

const LEFT_UP: [u8; 4] = [0x40, 0x04, 0x02, 0x01];
const RIGHT_UP: [u8; 4] = [0x80, 0x20, 0x10, 0x08];

impl Widget for AreaChart<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let w = area.width as usize;
        let h = area.height as usize;
        let slots = w * 2;
        let n = self.data.len();
        // Right-align: the newest sample sits in the rightmost half-cell.
        let sample = |slot: usize| -> Option<f64> {
            let from_right = slots - 1 - slot;
            if from_right >= n { None } else { self.data[n - 1 - from_right] }
        };
        let total_dots = (h * 4) as f64;
        for cx in 0..w {
            let l = sample(cx * 2);
            let r = sample(cx * 2 + 1);
            for row in 0..h {
                // row counted from the baseline
                let dots = |v: Option<f64>| -> usize {
                    match v {
                        None => 0,
                        Some(v) => {
                            let d = (v.clamp(0.0, 1.0) * total_dots).round() as isize;
                            // Keep a visible baseline for tiny non-zero values.
                            let d = if d == 0 && v > 0.004 { 1 } else { d };
                            (d - (row as isize * 4)).clamp(0, 4) as usize
                        }
                    }
                };
                let (ld, rd) = (dots(l), dots(r));
                if ld == 0 && rd == 0 {
                    continue;
                }
                let bits = LEFT_UP[..ld].iter().chain(&RIGHT_UP[..rd]).fold(0u8, |b, d| b | d);
                let ch = char::from_u32(0x2800 + bits as u32).unwrap_or(' ');
                let y = area.y + (h - 1 - row) as u16;
                let v = l.unwrap_or(0.0).max(r.unwrap_or(0.0));
                let top = ((v.clamp(0.0, 1.0) * total_dots).ceil() as usize).div_ceil(4).max(1) - 1;
                let col = paint(self.grad, (row as f64 + 0.5) / h as f64, v, row == top);
                buf[(area.x + cx as u16, y)].set_char(ch).set_fg(col);
            }
        }
    }
}

/// One-row sparkline from block elements, colored by value.
pub struct Spark<'a> {
    pub data: &'a [Option<f64>],
    pub grad: Gradient,
}

impl Widget for Spark<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        const BLK: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let w = area.width as usize;
        let n = self.data.len();
        for i in 0..w {
            let from_right = w - 1 - i;
            if from_right >= n {
                continue;
            }
            let Some(v) = self.data[n - 1 - from_right] else { continue };
            let v = v.clamp(0.0, 1.0);
            let idx = ((v * 8.0).round() as usize).max(if v > 0.004 { 1 } else { 0 });
            buf[(area.x + i as u16, area.y)].set_char(BLK[idx]).set_fg(paint(self.grad, v, v, true));
        }
    }
}

/// Solid area chart from eighth-block characters: one sample per column,
/// 1/8-cell vertical precision, rows colored along a vertical gradient.
pub struct BlockChart<'a> {
    pub data: &'a [Option<f64>],
    pub grad: Gradient,
}

impl Widget for BlockChart<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        const LOWER: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let (w, h) = (area.width as usize, area.height as usize);
        let n = self.data.len();
        for cx in 0..w {
            let from_right = w - 1 - cx;
            if from_right >= n {
                continue;
            }
            let Some(v) = self.data[n - 1 - from_right] else { continue };
            let v = v.clamp(0.0, 1.0);
            let mut eighths = (v * (h * 8) as f64).round() as usize;
            if eighths == 0 && v > 0.004 {
                eighths = 1;
            }
            let top = eighths.div_ceil(8).saturating_sub(1);
            for row in 0..h {
                let fill = eighths.saturating_sub(row * 8).min(8);
                if fill == 0 {
                    break;
                }
                let col = paint(self.grad, (row as f64 + 0.5) / h as f64, v, row == top);
                let x = area.x + cx as u16;
                let y = area.y + (h - 1 - row) as u16;
                buf[(x, y)].set_char(LOWER[fill]).set_fg(col);
            }
        }
    }
}
