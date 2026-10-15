//! Charts. Three styles, best available picked automatically:
//!
//! - `Pixels`: real anti-aliased plots rendered with tiny-skia and shown via a
//!   terminal graphics protocol (kitty, sixel, iTerm2) through ratatui-image.
//! - `Blocks`: solid area charts from eighth-block characters (works anywhere).
//! - `Braille`: 2x4 dots per cell (higher resolution, dotty look).

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use image::{DynamicImage, RgbaImage};
use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;
use ratatui_image::Image;
use ratatui_image::picker::{Capability, Picker, ProtocolType, cap_parser::QueryStdioOptions};
use ratatui_image::protocol::{Protocol, kitty::Kitty};
use tiny_skia::{
    FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, PathBuilder, Pixmap, Point, SpreadMode, Stroke, Transform,
};

use super::theme::Gradient;
use super::widgets::{AreaChart, BlockChart};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChartStyle {
    Pixels,
    Blocks,
    Braille,
}

impl ChartStyle {
    pub fn label(self) -> &'static str {
        match self {
            ChartStyle::Pixels => "pixels",
            ChartStyle::Blocks => "blocks",
            ChartStyle::Braille => "braille",
        }
    }
}

/// One data series. Values are normalized to [0,1], newest last.
pub struct Series<'a> {
    pub data: &'a [Option<f64>],
    pub grad: Gradient,
    /// Hang from the top / middle instead of growing from the bottom.
    pub down: bool,
}

struct Cached {
    key: (u64, Rect, usize),
    proto: Protocol,
}

pub struct Gfx {
    picker: Option<Picker>,
    /// Terminal background (from OSC 11), used so plots blend in.
    pub bg: Option<(u8, u8, u8)>,
    cache: RefCell<HashMap<u32, Cached>>,
}

impl Gfx {
    /// Probe the terminal. Must run after entering the alternate screen and
    /// before reading input. `force` overrides detection ("kitty", "sixel",
    /// "iterm2", "off").
    pub fn detect(force: Option<&str>) -> Self {
        let none = Gfx::none();
        if force == Some("off") || (force.is_none() && !should_probe()) {
            return none;
        }
        let opts = QueryStdioOptions {
            timeout: Duration::from_millis(600),
            terminal_background_color_osc: true,
            // Plots are mostly flat color and compress ~20x; raw RGBA would be
            // megabytes per second at a 1 s refresh.
            kitty_compression: true,
            ..Default::default()
        };
        let Ok(mut picker) = Picker::from_query_stdio_with_options(opts) else { return none };
        let bg = picker.capabilities().iter().find_map(|c| match c {
            Capability::Background(r, g, b) => Some((*r, *g, *b)),
            _ => None,
        });
        match force {
            Some("kitty") => picker.set_protocol_type(ProtocolType::Kitty),
            Some("sixel") => picker.set_protocol_type(ProtocolType::Sixel),
            Some("iterm2") => picker.set_protocol_type(ProtocolType::Iterm2),
            _ => {}
        }
        if picker.protocol_type() == ProtocolType::Halfblocks {
            return Gfx { bg, ..none };
        }
        Gfx { picker: Some(picker), bg, cache: Default::default() }
    }

    pub fn none() -> Self {
        Gfx { picker: None, bg: None, cache: Default::default() }
    }

    pub fn pixels_available(&self) -> bool {
        self.picker.is_some()
    }

    pub fn protocol_name(&self) -> &'static str {
        match self.picker.as_ref().map(|p| p.protocol_type()) {
            Some(ProtocolType::Kitty) => "kitty",
            Some(ProtocolType::Sixel) => "sixel",
            Some(ProtocolType::Iterm2) => "iterm2",
            _ => "none",
        }
    }

    /// Forget cached images (after a resize, theme change, or popup).
    pub fn invalidate(&self) {
        self.cache.borrow_mut().clear();
    }

    /// Draw `series` into `area` as a pixel image. `slot` identifies the chart
    /// (stable across frames); `version` changes whenever the data does.
    fn draw_pixels(&self, f: &mut Frame, area: Rect, slot: u32, version: u64, theme: usize, series: &[Series], bg: Color) {
        let Some(picker) = &self.picker else { return };
        let key = (version, area, theme);
        let mut cache = self.cache.borrow_mut();
        let fresh = cache.get(&slot).is_some_and(|c| c.key == key);
        if !fresh {
            let fs = picker.font_size();
            let (w, h) = (area.width as u32 * fs.width as u32, area.height as u32 * fs.height as u32);
            if w == 0 || h == 0 {
                return;
            }
            let img = DynamicImage::ImageRgba8(render(w, h, fs.height as f32 / 18.0, series, bg));
            let size = Size::new(area.width, area.height);
            let proto = match picker.protocol_type() {
                // Reuse one kitty image id per slot so the terminal replaces the
                // image instead of accumulating a new one every tick.
                ProtocolType::Kitty => Kitty::new(img, size, 0x4E56_0000 | slot, picker.tmux_detected(), picker.capabilities().contains(&Capability::KittyCompression))
                    .map(Protocol::Kitty),
                _ => picker.new_protocol(img, size, ratatui_image::Resize::Fit(None)),
            };
            match proto {
                Ok(proto) => {
                    cache.insert(slot, Cached { key, proto });
                }
                Err(_) => return,
            }
        }
        if let Some(c) = cache.get(&slot) {
            f.render_widget(Image::new(&c.proto), area);
        }
    }
}

/// The probe reads the terminal's answers from stdin on a helper thread. A
/// terminal that doesn't answer every query leaves that thread blocked in
/// read(), where it eats the user's next keypress. So only probe terminals
/// known to answer, and never through a multiplexer.
fn should_probe() -> bool {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let term = var("TERM");
    if std::env::var_os("TMUX").is_some() || term.starts_with("tmux") || term.starts_with("screen") || std::env::var_os("STY").is_some() {
        return false;
    }
    let prog = var("TERM_PROGRAM");
    std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var_os("WEZTERM_EXECUTABLE").is_some()
        || std::env::var_os("KONSOLE_VERSION").is_some()
        || std::env::var_os("WT_SESSION").is_some()
        || term.contains("kitty")
        || term.contains("ghostty")
        || term.starts_with("foot")
        || term.contains("mlterm")
        || ["iTerm.app", "WezTerm", "ghostty", "vscode", "rio", "Tabby", "contour"].contains(&prog.as_str())
}

/// Context for drawing a chart in whichever style is active.
pub struct ChartCtx<'a> {
    pub gfx: &'a Gfx,
    pub style: ChartStyle,
    pub version: u64,
    pub theme: usize,
    pub bg: Color,
}

impl ChartCtx<'_> {
    /// How many samples to show across `width` cells.
    pub fn samples(&self, width: u16) -> usize {
        match self.style {
            ChartStyle::Blocks if !self.pixels() => width as usize,
            _ => width as usize * 2,
        }
    }

    pub fn pixels(&self) -> bool {
        self.style == ChartStyle::Pixels && self.gfx.pixels_available()
    }

    /// Draw one chart. With one `down` series, the area is split: up-series on
    /// top, down-series hanging below (mirrored).
    pub fn draw(&self, f: &mut Frame, area: Rect, slot: u32, series: &[Series]) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        if self.pixels() {
            self.gfx.draw_pixels(f, area, slot, self.version, self.theme, series, self.bg);
            return;
        }
        let mirrored = series.iter().any(|s| s.down) && series.iter().any(|s| !s.down);
        let up_h = if mirrored { area.height.div_ceil(2) } else { area.height };
        for s in series {
            let a = if !mirrored {
                area
            } else if s.down {
                Rect { y: area.y + up_h, height: area.height - up_h, ..area }
            } else {
                Rect { height: up_h, ..area }
            };
            match self.style {
                ChartStyle::Braille => f.render_widget(AreaChart { data: s.data, grad: s.grad, inverted: s.down }, a),
                _ => f.render_widget(BlockChart { data: s.data, grad: s.grad, inverted: s.down }, a),
            }
        }
    }
}

// ---------------------------------------------------------------- rasterizer

fn sk(c: Color, alpha: f32) -> tiny_skia::Color {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(i) => (i, i, i),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        Color::DarkGray => (90, 90, 90),
        _ => (180, 180, 180),
    };
    tiny_skia::Color::from_rgba8(r, g, b, (alpha.clamp(0.0, 1.0) * 255.0) as u8)
}

/// Vertical gradient shader spanning `y_low` (value 0) to `y_high` (value 1).
fn shader(grad: Gradient, y_low: f32, y_high: f32, alpha: [f32; 3]) -> Option<tiny_skia::Shader<'static>> {
    LinearGradient::new(
        Point::from_xy(0.0, y_low),
        Point::from_xy(0.0, y_high),
        vec![
            GradientStop::new(0.0, sk(grad.at(0.0), alpha[0])),
            GradientStop::new(0.5, sk(grad.at(0.5), alpha[1])),
            GradientStop::new(1.0, sk(grad.at(1.0), alpha[2])),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
}

/// Render series into an RGBA image. `scale` ~ 1.0 at an 18px-tall font.
pub fn render(w: u32, h: u32, scale: f32, series: &[Series], bg: Color) -> RgbaImage {
    let mut pm = Pixmap::new(w, h).expect("non-zero size");
    pm.fill(sk(bg, 1.0));
    let (wf, hf) = (w as f32, h as f32);
    let scale = scale.max(0.5);
    let pad = 3.0 * scale;
    let mirrored = series.iter().any(|s| s.down) && series.iter().any(|s| !s.down);

    // Grid: quarter lines, faint.
    let grid_col = lerp_rgb(bg, Color::Rgb(255, 255, 255), 0.07);
    let mut grid = Paint::default();
    grid.set_color(sk(grid_col, 1.0));
    grid.anti_alias = false;
    let grid_stroke = Stroke { width: 1.0, ..Default::default() };
    let lines: &[f32] = if mirrored { &[0.25, 0.5, 0.75] } else { &[0.25, 0.5, 0.75] };
    for &q in lines {
        let y = (hf * q).round() + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(0.0, y);
        pb.line_to(wf, y);
        if let Some(p) = pb.finish() {
            pm.stroke_path(&p, &grid, &grid_stroke, Transform::identity(), None);
        }
    }

    for s in series {
        // Value 0 / 1 positions for this series.
        let (y0, y1) = match (mirrored, s.down) {
            (true, false) => (hf / 2.0, pad),
            (true, true) => (hf / 2.0, hf - pad),
            (false, false) => (hf - 1.0, pad),
            (false, true) => (0.0, hf - pad),
        };
        let n = s.data.len();
        if n == 0 {
            continue;
        }
        let slots = n.max(2) as f32;
        let step = (wf - pad) / (slots - 1.0);
        let pts: Vec<Option<(f32, f32)>> = s
            .data
            .iter()
            .enumerate()
            .map(|(i, v)| {
                v.map(|v| {
                    let x = wf - pad - (n - 1 - i) as f32 * step;
                    (x, y0 + (y1 - y0) * v.clamp(0.0, 1.0) as f32)
                })
            })
            .collect();

        // Split into contiguous runs of known values.
        let mut runs: Vec<Vec<(f32, f32)>> = vec![Vec::new()];
        for p in pts {
            match p {
                Some(p) => runs.last_mut().unwrap().push(p),
                None if !runs.last().unwrap().is_empty() => runs.push(Vec::new()),
                None => {}
            }
        }

        let (ymin, ymax) = (y0.min(y1), y0.max(y1));
        for run in runs.iter().filter(|r| !r.is_empty()) {
            let curve = |pb: &mut PathBuilder| smooth(pb, run, ymin, ymax);

            // Area fill: gradient fading towards the baseline.
            let mut pb = PathBuilder::new();
            pb.move_to(run[0].0, y0);
            pb.line_to(run[0].0, run[0].1);
            curve(&mut pb);
            pb.line_to(run[run.len() - 1].0, y0);
            pb.close();
            if let (Some(path), Some(sh)) = (pb.finish(), shader(s.grad, y0, y1, [0.10, 0.30, 0.55])) {
                let paint = Paint { shader: sh, anti_alias: true, ..Default::default() };
                pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            }

            // Line: soft glow underneath, crisp stroke on top.
            let mut pb = PathBuilder::new();
            pb.move_to(run[0].0, run[0].1);
            curve(&mut pb);
            if let Some(path) = pb.finish() {
                for (width, alpha) in [(5.0 * scale, [0.10, 0.14, 0.18]), (1.7 * scale, [0.95, 1.0, 1.0])] {
                    if let Some(sh) = shader(s.grad, y0, y1, alpha) {
                        let paint = Paint { shader: sh, anti_alias: true, ..Default::default() };
                        let stroke = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
                        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                    }
                }
            }
        }

        // Highlight the newest point.
        if let Some(&(x, y)) = runs.last().and_then(|r| r.last()) {
            let t = ((y - y0) / (y1 - y0)).clamp(0.0, 1.0) as f64;
            let col = s.grad.at(t);
            for (r, a) in [(5.5 * scale, 0.18), (2.6 * scale, 1.0)] {
                if let Some(c) = PathBuilder::from_circle(x, y, r) {
                    let mut p = Paint::default();
                    p.set_color(sk(col, a));
                    p.anti_alias = true;
                    pm.fill_path(&c, &p, FillRule::Winding, Transform::identity(), None);
                }
            }
        }
    }

    if mirrored {
        let mut mid = Paint::default();
        mid.set_color(sk(lerp_rgb(bg, Color::Rgb(255, 255, 255), 0.18), 1.0));
        let mut pb = PathBuilder::new();
        pb.move_to(0.0, (hf / 2.0).round() + 0.5);
        pb.line_to(wf, (hf / 2.0).round() + 0.5);
        if let Some(p) = pb.finish() {
            pm.stroke_path(&p, &mid, &Stroke { width: 1.0, ..Default::default() }, Transform::identity(), None);
        }
    }

    // Opaque background, so premultiplied == straight RGBA.
    RgbaImage::from_raw(w, h, pm.take()).expect("buffer size matches")
}

/// Catmull-Rom spline through `pts` (starting at pts[0], already moved to),
/// with control points clamped so the curve never overshoots the plot range.
fn smooth(pb: &mut PathBuilder, pts: &[(f32, f32)], ymin: f32, ymax: f32) {
    if pts.len() == 1 {
        pb.line_to(pts[0].0 + 0.01, pts[0].1);
        return;
    }
    let c = |y: f32| y.clamp(ymin, ymax);
    for i in 0..pts.len() - 1 {
        let p0 = pts[i.saturating_sub(1)];
        let p1 = pts[i];
        let p2 = pts[i + 1];
        let p3 = pts[(i + 2).min(pts.len() - 1)];
        let c1 = (p1.0 + (p2.0 - p0.0) / 6.0, c(p1.1 + (p2.1 - p0.1) / 6.0));
        let c2 = (p2.0 - (p3.0 - p1.0) / 6.0, c(p2.1 - (p3.1 - p1.1) / 6.0));
        pb.cubic_to(c1.0, c1.1, c2.0, c2.1, p2.0, p2.1);
    }
}

fn lerp_rgb(a: Color, b: Color, t: f64) -> Color {
    super::theme::lerp(a, b, t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::THEMES;

    /// Renders sample charts to target/chart-*.png for eyeballing.
    #[test]
    fn renders_sample_charts() {
        let t = &THEMES[0];
        let util: Vec<Option<f64>> = (0..120).map(|i| Some((0.55 + 0.4 * ((i as f64) / 9.0).sin()).clamp(0.0, 1.0))).collect();
        let mem: Vec<Option<f64>> = (0..120).map(|i| if i < 10 { None } else { Some(0.3 + 0.002 * i as f64) }).collect();
        let bg = Color::Rgb(26, 27, 38);
        let img = render(900, 220, 1.0, &[Series { data: &util, grad: t.util, down: false }, Series { data: &mem, grad: t.mem, down: true }], bg);
        assert_eq!(img.dimensions(), (900, 220));
        let _ = std::fs::create_dir_all("target");
        let _ = img.save("target/chart-mirrored.png");
        let img = render(600, 300, 1.0, &[Series { data: &util, grad: t.power, down: false }], bg);
        let _ = img.save("target/chart-single.png");
    }
}
