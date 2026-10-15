//! Themes. Everything is defined in truecolor; on terminals without it the
//! finished frame is quantized to the xterm-256 palette in one pass.

use ratatui::buffer::Buffer;
use ratatui::style::Color;

#[derive(Clone, Copy)]
pub struct Gradient(pub [Color; 3]);

impl Gradient {
    /// Color at `t` in [0,1] across low -> mid -> high.
    pub fn at(&self, t: f64) -> Color {
        let t = t.clamp(0.0, 1.0);
        let (a, b, u) = if t < 0.5 { (self.0[0], self.0[1], t * 2.0) } else { (self.0[1], self.0[2], (t - 0.5) * 2.0) };
        lerp(a, b, u)
    }
}

pub fn lerp(a: Color, b: Color, t: f64) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
            Color::Rgb(m(r1, r2), m(g1, g2), m(b1, b2))
        }
        _ => {
            if t < 0.5 {
                a
            } else {
                b
            }
        }
    }
}

pub struct Theme {
    pub name: &'static str,
    pub fg: Color,
    pub dim: Color,
    pub faint: Color,
    pub border: Color,
    pub border_hi: Color,
    pub title: Color,
    pub accent: Color,
    pub accent2: Color,
    pub ok: Color,
    pub warn: Color,
    pub crit: Color,
    /// Empty part of bars.
    pub track: Color,
    pub sel_bg: Color,
    pub util: Gradient,
    pub mem: Gradient,
    pub power: Gradient,
    pub temp: Gradient,
    pub cpu: Gradient,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const THEMES: &[Theme] = &[
    Theme {
        name: "neon",
        fg: rgb(0xc8d3f5),
        dim: rgb(0x828bb8),
        faint: rgb(0x444a73),
        border: rgb(0x3b4261),
        border_hi: rgb(0x82aaff),
        title: rgb(0xc099ff),
        accent: rgb(0x86e1fc),
        accent2: rgb(0xff007c),
        ok: rgb(0xc3e88d),
        warn: rgb(0xffc777),
        crit: rgb(0xff5370),
        track: rgb(0x222436),
        sel_bg: rgb(0x2f334d),
        util: Gradient([rgb(0x4fd6be), rgb(0xffc777), rgb(0xff5370)]),
        mem: Gradient([rgb(0x65bcff), rgb(0xc099ff), rgb(0xff007c)]),
        power: Gradient([rgb(0xc3e88d), rgb(0xff966c), rgb(0xff5370)]),
        temp: Gradient([rgb(0x65bcff), rgb(0xffc777), rgb(0xff5370)]),
        cpu: Gradient([rgb(0x4fd6be), rgb(0xffc777), rgb(0xff5370)]),
    },
    Theme {
        name: "catppuccin",
        fg: rgb(0xcdd6f4),
        dim: rgb(0x9399b2),
        faint: rgb(0x585b70),
        border: rgb(0x45475a),
        border_hi: rgb(0xb4befe),
        title: rgb(0xcba6f7),
        accent: rgb(0x89dceb),
        accent2: rgb(0xf5c2e7),
        ok: rgb(0xa6e3a1),
        warn: rgb(0xf9e2af),
        crit: rgb(0xf38ba8),
        track: rgb(0x262637),
        sel_bg: rgb(0x313244),
        util: Gradient([rgb(0xa6e3a1), rgb(0xf9e2af), rgb(0xf38ba8)]),
        mem: Gradient([rgb(0x89b4fa), rgb(0xcba6f7), rgb(0xf5c2e7)]),
        power: Gradient([rgb(0xa6e3a1), rgb(0xfab387), rgb(0xf38ba8)]),
        temp: Gradient([rgb(0x74c7ec), rgb(0xf9e2af), rgb(0xf38ba8)]),
        cpu: Gradient([rgb(0x94e2d5), rgb(0xf9e2af), rgb(0xf38ba8)]),
    },
    Theme {
        name: "dracula",
        fg: rgb(0xf8f8f2),
        dim: rgb(0xa0a4c0),
        faint: rgb(0x6272a4),
        border: rgb(0x44475a),
        border_hi: rgb(0xbd93f9),
        title: rgb(0xff79c6),
        accent: rgb(0x8be9fd),
        accent2: rgb(0xff79c6),
        ok: rgb(0x50fa7b),
        warn: rgb(0xf1fa8c),
        crit: rgb(0xff5555),
        track: rgb(0x2a2c3a),
        sel_bg: rgb(0x44475a),
        util: Gradient([rgb(0x50fa7b), rgb(0xf1fa8c), rgb(0xff5555)]),
        mem: Gradient([rgb(0x8be9fd), rgb(0xbd93f9), rgb(0xff79c6)]),
        power: Gradient([rgb(0x50fa7b), rgb(0xffb86c), rgb(0xff5555)]),
        temp: Gradient([rgb(0x8be9fd), rgb(0xf1fa8c), rgb(0xff5555)]),
        cpu: Gradient([rgb(0x50fa7b), rgb(0xf1fa8c), rgb(0xff5555)]),
    },
    Theme {
        name: "nord",
        fg: rgb(0xd8dee9),
        dim: rgb(0x9aa5b8),
        faint: rgb(0x4c566a),
        border: rgb(0x434c5e),
        border_hi: rgb(0x88c0d0),
        title: rgb(0x88c0d0),
        accent: rgb(0x8fbcbb),
        accent2: rgb(0xb48ead),
        ok: rgb(0xa3be8c),
        warn: rgb(0xebcb8b),
        crit: rgb(0xbf616a),
        track: rgb(0x2b303b),
        sel_bg: rgb(0x3b4252),
        util: Gradient([rgb(0xa3be8c), rgb(0xebcb8b), rgb(0xbf616a)]),
        mem: Gradient([rgb(0x81a1c1), rgb(0xb48ead), rgb(0xd08770)]),
        power: Gradient([rgb(0xa3be8c), rgb(0xd08770), rgb(0xbf616a)]),
        temp: Gradient([rgb(0x5e81ac), rgb(0xebcb8b), rgb(0xbf616a)]),
        cpu: Gradient([rgb(0x8fbcbb), rgb(0xebcb8b), rgb(0xbf616a)]),
    },
    Theme {
        name: "matrix",
        fg: rgb(0xb8f5c0),
        dim: rgb(0x5fa86a),
        faint: rgb(0x2d5a34),
        border: rgb(0x1f4a28),
        border_hi: rgb(0x39ff6a),
        title: rgb(0x39ff6a),
        accent: rgb(0x7dffa0),
        accent2: rgb(0xd4ff3a),
        ok: rgb(0x39ff6a),
        warn: rgb(0xd4ff3a),
        crit: rgb(0xff4f4f),
        track: rgb(0x0f2414),
        sel_bg: rgb(0x173a20),
        util: Gradient([rgb(0x0f8a2e), rgb(0x39ff6a), rgb(0xd4ff3a)]),
        mem: Gradient([rgb(0x0f6a5a), rgb(0x2ee6c0), rgb(0xa0fff0)]),
        power: Gradient([rgb(0x0f8a2e), rgb(0xa8e03a), rgb(0xffd23a)]),
        temp: Gradient([rgb(0x0f8a2e), rgb(0xd4ff3a), rgb(0xff4f4f)]),
        cpu: Gradient([rgb(0x0f8a2e), rgb(0x39ff6a), rgb(0xd4ff3a)]),
    },
    Theme {
        name: "mono",
        fg: Color::Reset,
        dim: Color::Gray,
        faint: Color::DarkGray,
        border: Color::DarkGray,
        border_hi: Color::White,
        title: Color::White,
        accent: Color::White,
        accent2: Color::White,
        ok: Color::White,
        warn: Color::White,
        crit: Color::White,
        track: Color::Reset,
        sel_bg: Color::DarkGray,
        util: Gradient([Color::Gray, Color::Gray, Color::White]),
        mem: Gradient([Color::Gray, Color::Gray, Color::White]),
        power: Gradient([Color::Gray, Color::Gray, Color::White]),
        temp: Gradient([Color::Gray, Color::Gray, Color::White]),
        cpu: Gradient([Color::Gray, Color::Gray, Color::White]),
    },
];

pub fn by_name(name: &str) -> Option<usize> {
    THEMES.iter().position(|t| t.name.eq_ignore_ascii_case(name))
}

/// Best guess at 24-bit color support.
pub fn truecolor_supported() -> bool {
    if let Some(v) = crate::brand::env("COLOR") {
        return v != "256";
    }
    if let Ok(c) = std::env::var("COLORTERM")
        && (c.contains("truecolor") || c.contains("24bit"))
    {
        return true;
    }
    if cfg!(windows) {
        // Windows Terminal and modern conhost both handle 24-bit SGR.
        return true;
    }
    if std::env::var_os("WT_SESSION").is_some() {
        return true;
    }
    if let Ok(t) = std::env::var("TERM_PROGRAM")
        && ["iTerm.app", "WezTerm", "vscode", "ghostty", "Hyper", "Tabby", "rio"].contains(&t.as_str())
    {
        return true;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    term.contains("truecolor")
        || term.contains("24bit")
        || term.contains("kitty")
        || term.contains("ghostty")
        || term.contains("alacritty")
        || term.contains("foot")
}

fn to_256(c: Color) -> Color {
    let Color::Rgb(r, g, b) = c else { return c };
    const LV: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let idx = |v: u8| -> usize {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v as usize - 35) / 40).min(5)
        }
    };
    let (ri, gi, bi) = (idx(r), idx(g), idx(b));
    let cube = (LV[ri], LV[gi], LV[bi]);
    let d = |a: (u8, u8, u8)| {
        let f = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        f(a.0, r) + f(a.1, g) + f(a.2, b)
    };
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gray_i = if avg > 238 { 23 } else { (avg.saturating_sub(3) / 10) as u8 };
    let gv = 8 + 10 * gray_i;
    if d((gv, gv, gv)) < d(cube) { Color::Indexed(232 + gray_i) } else { Color::Indexed(16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8) }
}

/// Downgrade a rendered frame for terminals without truecolor.
pub fn quantize(buf: &mut Buffer) {
    for cell in buf.content.iter_mut() {
        cell.fg = to_256(cell.fg);
        cell.bg = to_256(cell.bg);
    }
}
