//! Custom widgets: gradient bars, braille area charts, sparklines.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use super::theme::Gradient;

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
        let filled = frac.clamp(0.0, 1.0) * w as f64;
        for i in 0..w {
            let pos = (i as f64 + 0.5) / w as f64;
            let col = self.grad.at(pos);
            let cell = &mut buf[(area.x + i as u16, y)];
            let amount = (filled - i as f64).clamp(0.0, 1.0);
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
/// `inverted` hangs the chart from the top (for mirrored up/down charts).
pub struct AreaChart<'a> {
    pub data: &'a [Option<f64>],
    pub grad: Gradient,
    pub inverted: bool,
}

const LEFT_UP: [u8; 4] = [0x40, 0x04, 0x02, 0x01];
const RIGHT_UP: [u8; 4] = [0x80, 0x20, 0x10, 0x08];
const LEFT_DOWN: [u8; 4] = [0x01, 0x02, 0x04, 0x40];
const RIGHT_DOWN: [u8; 4] = [0x08, 0x10, 0x20, 0x80];

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
                // row counted from the baseline (bottom, or top if inverted)
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
                let (lt, rt) = if self.inverted { (&LEFT_DOWN, &RIGHT_DOWN) } else { (&LEFT_UP, &RIGHT_UP) };
                let mut bits = 0u8;
                for i in 0..ld {
                    bits |= lt[i];
                }
                for i in 0..rd {
                    bits |= rt[i];
                }
                let ch = char::from_u32(0x2800 + bits as u32).unwrap_or(' ');
                let y = if self.inverted { area.y + row as u16 } else { area.y + (h - 1 - row) as u16 };
                let col = self.grad.at((row as f64 + 0.5) / h as f64);
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
            buf[(area.x + i as u16, area.y)].set_char(BLK[idx]).set_fg(self.grad.at(v));
        }
    }
}
