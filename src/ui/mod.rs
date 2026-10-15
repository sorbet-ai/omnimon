//! Interactive TUI.

mod draw;
mod gfx;

pub use gfx::ChartStyle;
mod theme;
mod widgets;

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;
use ratatui::widgets::TableState;

use crate::Cli;
use crate::collector::{Collector, Sampler};
use crate::config::Config;
use crate::host::{KillSignal, send_signal};
use crate::model::{PidMapping, ProcRow, Snapshot, SystemInfo};

const HISTORY: usize = 3600;

/// Ring of raw samples (Vec + periodic trim so charts can borrow a slice).
#[derive(Default)]
pub struct Ring(Vec<Option<f64>>);

impl Ring {
    fn push(&mut self, v: Option<f64>) {
        self.0.push(v);
        if self.0.len() > HISTORY * 2 {
            self.0.drain(..self.0.len() - HISTORY);
        }
    }
    pub fn tail(&self, n: usize) -> &[Option<f64>] {
        &self.0[self.0.len().saturating_sub(n)..]
    }
}

#[derive(Default)]
pub struct GpuHist {
    pub util: Ring,
    pub mem: Ring,
    pub power: Ring,
    pub temp: Ring,
    pub clock: Ring,
    pub rx: Ring,
    pub tx: Ring,
    pub enc: Ring,
    pub dec: Ring,
    /// Highest power seen; the bar scale for GPUs without a power limit (Apple).
    pub peak_power: f64,
}

#[derive(Default)]
pub struct ProcHist {
    pub mem: Ring,
    pub sm: Ring,
    pub cpu: Ring,
    seen: bool,
}

#[derive(Default)]
pub struct HostHist {
    pub cpu: Ring,
    pub mem: Ring,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Overview,
    Charts,
    Details,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Overview, Tab::Charts, Tab::Details];
    pub fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Charts => "Charts",
            Tab::Details => "Details",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    GpuMem,
    Sm,
    Cpu,
    Rss,
    Time,
    Pid,
    User,
    Name,
    Gpu,
}

impl SortKey {
    pub const CYCLE: [SortKey; 9] =
        [SortKey::GpuMem, SortKey::Sm, SortKey::Cpu, SortKey::Rss, SortKey::Time, SortKey::Pid, SortKey::User, SortKey::Name, SortKey::Gpu];
    pub fn label(self) -> &'static str {
        match self {
            SortKey::GpuMem => "GPU-MEM",
            SortKey::Sm => "SM",
            SortKey::Cpu => "CPU",
            SortKey::Rss => "RAM",
            SortKey::Time => "TIME",
            SortKey::Pid => "PID",
            SortKey::User => "USER",
            SortKey::Name => "COMMAND",
            SortKey::Gpu => "GPU",
        }
    }
    /// Natural direction: numbers biggest-first, text A-Z.
    fn default_desc(self) -> bool {
        !matches!(self, SortKey::Pid | SortKey::User | SortKey::Name | SortKey::Gpu)
    }
}

pub enum Popup {
    Help,
    Process { gpu: usize, pid: u32 },
    Kill { targets: Vec<(usize, u32, u32, String)>, signal: usize },
}

pub const SIGNALS: [KillSignal; 3] = [KillSignal::Interrupt, KillSignal::Terminate, KillSignal::Kill];

/// Screen regions recorded during draw, for mouse hit-testing.
#[derive(Default)]
pub struct Hit {
    pub tabs: Vec<(Rect, Tab)>,
    pub gpus: Vec<(Rect, usize)>,
    pub proc_body: Rect,
    pub proc_header: Vec<(u16, u16, SortKey)>,
    pub proc_header_y: u16,
}

pub struct App {
    pub info: SystemInfo,
    pub snap: Option<Snapshot>,
    pub hist: Vec<GpuHist>,
    pub host_hist: HostHist,
    pub proc_hist: std::collections::HashMap<(usize, u32), ProcHist>,
    pub tab: Tab,
    pub sel_gpu: usize,
    pub table: TableState,
    sel_key: Option<(usize, u32)>,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub filter: String,
    pub filter_edit: bool,
    pub gpu_only: bool,
    pub full_cmd: bool,
    pub tagged: HashSet<(usize, u32)>,
    pub popup: Option<Popup>,
    pub toast: Option<(String, Instant, bool)>,
    pub theme: usize,
    pub truecolor: bool,
    pub readonly: bool,
    pub paused: bool,
    pub interval: Duration,
    pub hit: Hit,
    pub gfx: gfx::Gfx,
    pub chart_style: ChartStyle,
    /// Bumped on every new snapshot; keys the chart image cache.
    pub version: u64,
    pub me: Option<String>,
    quit: bool,
}

impl App {
    fn new(info: SystemInfo, interval: Duration, cli: &Cli, cfg: &Config, gfx: gfx::Gfx) -> Self {
        let chart_style = match cfg.chart_style {
            Some(ChartStyle::Pixels) | None if gfx.pixels_available() => ChartStyle::Pixels,
            Some(ChartStyle::Pixels) | None => ChartStyle::Blocks,
            Some(s) => s,
        };
        let n = info.gpus.len();
        let theme = cli
            .theme
            .clone()
            .or(crate::brand::env("THEME"))
            .or(cfg.theme.clone())
            .and_then(|t| theme::by_name(&t))
            .unwrap_or(0);
        let sort = cfg
            .sort
            .as_deref()
            .and_then(|s| SortKey::CYCLE.iter().copied().find(|k| k.label().eq_ignore_ascii_case(s)))
            .unwrap_or(SortKey::GpuMem);
        App {
            hist: (0..n).map(|_| GpuHist::default()).collect(),
            info,
            snap: None,
            host_hist: HostHist::default(),
            proc_hist: Default::default(),
            tab: Tab::Overview,
            sel_gpu: 0,
            table: TableState::default().with_selected(Some(0)),
            sel_key: None,
            sort,
            sort_desc: cfg.sort_desc.unwrap_or(sort.default_desc()),
            filter: String::new(),
            filter_edit: false,
            gpu_only: false,
            full_cmd: cfg.full_cmd.unwrap_or(false),
            tagged: HashSet::new(),
            popup: None,
            toast: None,
            theme,
            truecolor: theme::truecolor_supported(),
            readonly: cli.readonly,
            paused: false,
            interval,
            hit: Hit::default(),
            gfx,
            chart_style,
            version: 0,
            me: std::env::var("USER").or_else(|_| std::env::var("USERNAME")).ok(),
            quit: false,
        }
    }

    /// Power as a fraction of the limit, or of the peak seen if there's no limit.
    pub fn power_frac(&self, i: usize, s: &crate::model::GpuSample) -> Option<f64> {
        let p = s.power?;
        let scale = s
            .power_limit
            .or(self.info.gpus.get(i)?.power_limit_max)
            .unwrap_or(self.hist.get(i)?.peak_power.max(5.0));
        Some((p / scale).min(1.0))
    }

    pub fn chart_ctx(&self) -> gfx::ChartCtx<'_> {
        let bg = match self.gfx.bg {
            Some((r, g, b)) => ratatui::style::Color::Rgb(r, g, b),
            None => ratatui::style::Color::Rgb(0x1a, 0x1b, 0x26),
        };
        gfx::ChartCtx { gfx: &self.gfx, style: self.chart_style, version: self.version, theme: self.theme, bg }
    }

    fn cycle_chart_style(&mut self) {
        let order: Vec<ChartStyle> = [ChartStyle::Pixels, ChartStyle::Blocks, ChartStyle::Braille]
            .into_iter()
            .filter(|s| *s != ChartStyle::Pixels || self.gfx.pixels_available())
            .collect();
        let i = order.iter().position(|s| *s == self.chart_style).unwrap_or(0);
        self.chart_style = order[(i + 1) % order.len()];
        let extra = if self.chart_style == ChartStyle::Pixels { format!(" ({})", self.gfx.protocol_name()) } else { String::new() };
        self.toast(format!("charts: {}{extra}", self.chart_style.label()), false);
    }

    pub fn theme(&self) -> &'static theme::Theme {
        &theme::THEMES[self.theme]
    }

    fn ingest(&mut self, s: Snapshot) {
        for (i, g) in s.gpus.iter().enumerate() {
            let Some(h) = self.hist.get_mut(i) else { continue };
            let info = &self.info.gpus[i];
            h.util.push(g.util_gpu.map(|v| v as f64 / 100.0));
            h.mem.push(g.mem_frac());
            if let Some(p) = g.power {
                h.peak_power = h.peak_power.max(p);
            }
            let scale = g.power_limit.or(info.power_limit_max).unwrap_or(h.peak_power.max(5.0));
            h.power.push(g.power.map(|p| (p / scale).min(1.0)));
            let tmax = info.temp_shutdown.unwrap_or(100).max(1) as f64;
            h.temp.push(g.temp.map(|t| t as f64 / tmax));
            h.clock.push(match (g.clock_sm.or(g.clock_graphics), info.max_clock_sm.or(info.max_clock_graphics)) {
                (Some(c), Some(m)) if m > 0 => Some(c as f64 / m as f64),
                _ => None,
            });
            h.rx.push(g.pcie_rx.map(|v| v as f64));
            h.tx.push(g.pcie_tx.map(|v| v as f64));
            h.enc.push(g.enc_util.map(|v| v as f64 / 100.0));
            h.dec.push(g.dec_util.map(|v| v as f64 / 100.0));
        }
        self.host_hist.cpu.push(Some(s.host.cpu as f64 / 100.0));
        self.host_hist.mem.push(if s.host.mem_total > 0 { Some(s.host.mem_used as f64 / s.host.mem_total as f64) } else { None });
        // Drop tags for processes that went away.
        self.tagged.retain(|k| s.processes.iter().any(|p| (p.gpu, p.pid) == *k));
        for p in &s.processes {
            let h = self.proc_hist.entry((p.gpu, p.pid)).or_default();
            h.mem.push(p.gpu_mem.map(|v| v as f64));
            h.sm.push(p.sm_util.map(|v| v as f64 / 100.0));
            h.cpu.push(p.cpu.map(|v| v as f64));
            h.seen = true;
        }
        self.proc_hist.retain(|_, h| std::mem::take(&mut h.seen));
        self.snap = Some(s);
        self.version += 1;
        self.restore_selection();
    }

    /// Filtered + sorted process rows.
    pub fn rows(&self) -> Vec<&ProcRow> {
        let Some(s) = &self.snap else { return Vec::new() };
        let f = self.filter.to_lowercase();
        let mut v: Vec<&ProcRow> = s
            .processes
            .iter()
            .filter(|p| !self.gpu_only || p.gpu == self.sel_gpu)
            .filter(|p| {
                f.is_empty()
                    || p.cmdline.to_lowercase().contains(&f)
                    || p.user.as_deref().is_some_and(|u| u.to_lowercase().contains(&f))
                    || p.pid.to_string() == f
                    || p.local_pid.is_some_and(|l| l.to_string() == f)
            })
            .collect();
        // Sorting by GPU memory when no process reports it (WDDM without
        // counters, Apple) is useless; fall back to GPU utilization.
        let key = if self.sort == SortKey::GpuMem && v.iter().all(|p| p.gpu_mem.is_none()) { SortKey::Sm } else { self.sort };
        v.sort_by(|a, b| {
            use std::cmp::Ordering as O;
            let num = |x: Option<f64>, y: Option<f64>| x.unwrap_or(-1.0).partial_cmp(&y.unwrap_or(-1.0)).unwrap_or(O::Equal);
            let o = match key {
                SortKey::GpuMem => num(a.gpu_mem.map(|v| v as f64), b.gpu_mem.map(|v| v as f64)),
                SortKey::Sm => num(a.sm_util.map(f64::from), b.sm_util.map(f64::from)),
                SortKey::Cpu => num(a.cpu.map(f64::from), b.cpu.map(f64::from)),
                SortKey::Rss => num(a.rss.map(|v| v as f64), b.rss.map(|v| v as f64)),
                SortKey::Time => num(a.runtime.map(|v| v as f64), b.runtime.map(|v| v as f64)),
                SortKey::Pid => a.pid.cmp(&b.pid),
                SortKey::User => a.user.cmp(&b.user),
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::Gpu => a.gpu.cmp(&b.gpu),
            };
            let o = if self.sort_desc { o.reverse() } else { o };
            o.then(a.gpu.cmp(&b.gpu)).then(a.pid.cmp(&b.pid))
        });
        v
    }

    fn restore_selection(&mut self) {
        let rows = self.rows();
        let n = rows.len();
        let idx = self.sel_key.and_then(|k| rows.iter().position(|p| (p.gpu, p.pid) == k));
        let idx = idx.unwrap_or_else(|| self.table.selected().unwrap_or(0).min(n.saturating_sub(1)));
        let key = rows.get(idx).map(|p| (p.gpu, p.pid));
        self.table.select(if n == 0 { None } else { Some(idx) });
        self.sel_key = key;
    }

    fn move_sel(&mut self, delta: isize) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let cur = self.table.selected().unwrap_or(0) as isize;
        let idx = (cur + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.sel_key = Some((rows[idx].gpu, rows[idx].pid));
        self.table.select(Some(idx));
    }

    fn set_sort(&mut self, k: SortKey) {
        if self.sort == k {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort = k;
            self.sort_desc = k.default_desc();
        }
        self.restore_selection();
    }

    fn toast(&mut self, msg: impl Into<String>, err: bool) {
        self.toast = Some((msg.into(), Instant::now(), err));
    }

    fn open_kill(&mut self) {
        if self.readonly {
            self.toast("read-only mode", true);
            return;
        }
        let rows = self.rows();
        let sel: Vec<&ProcRow> = if self.tagged.is_empty() {
            self.table.selected().and_then(|i| rows.get(i).copied()).into_iter().collect()
        } else {
            rows.iter().copied().filter(|p| self.tagged.contains(&(p.gpu, p.pid))).collect()
        };
        let mut targets = Vec::new();
        let mut skipped = 0;
        for p in sel {
            match (p.local_pid, p.mapping) {
                (Some(l), m) if m != PidMapping::External => targets.push((p.gpu, p.pid, l, p.name.clone())),
                _ => skipped += 1,
            }
        }
        if targets.is_empty() {
            self.toast(
                if skipped > 0 { "process lives outside this PID namespace; can't signal it from here" } else { "no process selected" },
                true,
            );
            return;
        }
        self.popup = Some(Popup::Kill { targets, signal: 1 });
    }

    fn on_key(&mut self, k: KeyEvent, sampler: &Sampler) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c')) {
            self.quit = true;
            return;
        }
        // Popups grab input.
        if let Some(p) = &mut self.popup {
            match p {
                Popup::Help | Popup::Process { .. } => match k.code {
                    KeyCode::Char('K') | KeyCode::Delete | KeyCode::F(9) | KeyCode::Char('x') if matches!(p, Popup::Process { .. }) => {
                        self.popup = None;
                        self.open_kill();
                    }
                    _ => self.popup = None,
                },
                Popup::Kill { targets, signal } => match k.code {
                    KeyCode::Left | KeyCode::Char('h') => *signal = signal.saturating_sub(1),
                    KeyCode::Right | KeyCode::Char('l') => *signal = (*signal + 1).min(SIGNALS.len() - 1),
                    KeyCode::Char('i') => *signal = 0,
                    KeyCode::Char('t') => *signal = 1,
                    KeyCode::Char('k') => *signal = 2,
                    KeyCode::Enter | KeyCode::Char('y') => {
                        let sig = SIGNALS[*signal];
                        let targets = std::mem::take(targets);
                        self.popup = None;
                        let (mut ok, mut errs) = (0, Vec::new());
                        for (gpu, pid, local, _) in &targets {
                            match send_signal(*local, sig) {
                                Ok(()) => {
                                    ok += 1;
                                    self.tagged.remove(&(*gpu, *pid));
                                }
                                Err(e) => errs.push(e),
                            }
                        }
                        if errs.is_empty() {
                            self.toast(format!("sent {} to {ok} process(es)", sig.label()), false);
                        } else {
                            self.toast(errs.join("; "), true);
                        }
                    }
                    _ => self.popup = None,
                },
            }
            return;
        }
        if self.filter_edit {
            match k.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filter_edit = false;
                }
                KeyCode::Enter => self.filter_edit = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            self.restore_selection();
            return;
        }
        let ngpu = self.info.gpus.len().max(1);
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.restore_selection();
                } else if !self.tagged.is_empty() {
                    self.tagged.clear();
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Char('?') | KeyCode::F(1) => self.popup = Some(Popup::Help),
            KeyCode::Tab => self.tab = Tab::ALL[(Tab::ALL.iter().position(|&t| t == self.tab).unwrap() + 1) % Tab::ALL.len()],
            KeyCode::BackTab => self.tab = Tab::ALL[(Tab::ALL.iter().position(|&t| t == self.tab).unwrap() + Tab::ALL.len() - 1) % Tab::ALL.len()],
            KeyCode::Char('1') => self.tab = Tab::Overview,
            KeyCode::Char('2') => self.tab = Tab::Charts,
            KeyCode::Char('3') => self.tab = Tab::Details,
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::Home => self.move_sel(isize::MIN / 2),
            KeyCode::End => self.move_sel(isize::MAX / 2),
            KeyCode::Left | KeyCode::Char('[') => {
                self.sel_gpu = (self.sel_gpu + ngpu - 1) % ngpu;
                self.restore_selection();
            }
            KeyCode::Right | KeyCode::Char(']') => {
                self.sel_gpu = (self.sel_gpu + 1) % ngpu;
                self.restore_selection();
            }
            KeyCode::Char('s') | KeyCode::F(6) => {
                let i = SortKey::CYCLE.iter().position(|&s| s == self.sort).unwrap();
                let k = SortKey::CYCLE[(i + 1) % SortKey::CYCLE.len()];
                self.sort = k;
                self.sort_desc = k.default_desc();
                self.restore_selection();
            }
            KeyCode::Char('S') => {
                let i = SortKey::CYCLE.iter().position(|&s| s == self.sort).unwrap();
                let k = SortKey::CYCLE[(i + SortKey::CYCLE.len() - 1) % SortKey::CYCLE.len()];
                self.sort = k;
                self.sort_desc = k.default_desc();
                self.restore_selection();
            }
            KeyCode::Char('r') => {
                self.sort_desc = !self.sort_desc;
                self.restore_selection();
            }
            KeyCode::Char('/') => self.filter_edit = true,
            KeyCode::Char('g') => {
                self.gpu_only = !self.gpu_only;
                self.restore_selection();
            }
            KeyCode::Char('c') => self.full_cmd = !self.full_cmd,
            KeyCode::Char(' ') => {
                if let Some(key) = self.sel_key {
                    if !self.tagged.remove(&key) {
                        self.tagged.insert(key);
                    }
                    self.move_sel(1);
                }
            }
            KeyCode::Char('K') | KeyCode::Delete | KeyCode::F(9) => self.open_kill(),
            KeyCode::Char('x') => self.open_kill(),
            KeyCode::Enter => {
                if let Some((gpu, pid)) = self.sel_key {
                    self.popup = Some(Popup::Process { gpu, pid });
                }
            }
            KeyCode::Char('G') => self.cycle_chart_style(),
            KeyCode::Char('t') => {
                self.theme = (self.theme + 1) % theme::THEMES.len();
                self.toast(format!("theme: {}", self.theme().name), false);
            }
            KeyCode::Char('p') => {
                self.paused = !self.paused;
                sampler.ctl.paused.store(self.paused, Ordering::Relaxed);
                sampler.poke();
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.set_interval(sampler, false),
            KeyCode::Char('-') | KeyCode::Char('_') => self.set_interval(sampler, true),
            _ => {}
        }
    }

    fn set_interval(&mut self, sampler: &Sampler, faster: bool) {
        const STEPS: [u64; 9] = [100, 250, 500, 1000, 2000, 3000, 5000, 10000, 30000];
        let cur = self.interval.as_millis() as u64;
        let next = if faster {
            STEPS.iter().rev().find(|&&s| s < cur).copied().unwrap_or(STEPS[0])
        } else {
            STEPS.iter().find(|&&s| s > cur).copied().unwrap_or(*STEPS.last().unwrap())
        };
        self.interval = Duration::from_millis(next);
        sampler.ctl.interval_ms.store(next, Ordering::Relaxed);
        sampler.poke();
        self.toast(format!("refresh every {:.2}s", next as f64 / 1000.0), false);
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let inside = |r: Rect| m.column >= r.x && m.column < r.x + r.width && m.row >= r.y && m.row < r.y + r.height;
        match m.kind {
            MouseEventKind::ScrollDown => self.move_sel(3),
            MouseEventKind::ScrollUp => self.move_sel(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                if self.popup.is_some() {
                    self.popup = None;
                    return;
                }
                if let Some(&(_, t)) = self.hit.tabs.iter().find(|(r, _)| inside(*r)) {
                    self.tab = t;
                    return;
                }
                if let Some(&(_, g)) = self.hit.gpus.iter().find(|(r, _)| inside(*r)) {
                    self.sel_gpu = g;
                    self.restore_selection();
                    return;
                }
                if m.row == self.hit.proc_header_y {
                    if let Some(&(_, _, k)) = self.hit.proc_header.iter().find(|(x0, x1, _)| m.column >= *x0 && m.column < *x1) {
                        self.set_sort(k);
                    }
                    return;
                }
                let b = self.hit.proc_body;
                if inside(b) {
                    let idx = self.table.offset() + (m.row - b.y) as usize;
                    let rows = self.rows();
                    if let Some(p) = rows.get(idx) {
                        self.sel_key = Some((p.gpu, p.pid));
                        self.table.select(Some(idx));
                    }
                }
            }
            _ => {}
        }
    }
}

pub fn run(collector: Collector, info: SystemInfo, interval: Duration, cli: &Cli, cfg: Config) -> anyhow::Result<()> {
    let sampler = Sampler::spawn(collector, interval);
    let mut terminal = ratatui::init();
    // Probe for pixel graphics after entering the alternate screen and before
    // reading any input (the terminal answers on stdin).
    let gfx = if cli.graphics == "off" { gfx::Gfx::none() } else { gfx::Gfx::detect(Some(cli.graphics.as_str()).filter(|g| *g != "auto")) };
    let mut app = App::new(info, interval, cli, &cfg, gfx);
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = (|| -> anyhow::Result<()> {
        let mut dirty = true;
        let mut last_draw = Instant::now();
        loop {
            while let Ok(s) = sampler.rx.try_recv() {
                app.ingest(s);
                dirty = true;
            }
            if let Some((_, t, _)) = &app.toast
                && t.elapsed() > Duration::from_secs(3) {
                    app.toast = None;
                    dirty = true;
                }
            // Clock in the header ticks once a second even when paused.
            if last_draw.elapsed() >= Duration::from_secs(1) {
                dirty = true;
            }
            if dirty {
                terminal.draw(|f| {
                    draw::draw(f, &mut app);
                    if !app.truecolor {
                        theme::quantize(f.buffer_mut());
                    }
                })?;
                dirty = false;
                last_draw = Instant::now();
            }
            if event::poll(Duration::from_millis(80))? {
                // Sixel/iTerm2 images aren't tied to cells: anything that changes
                // what covers them needs a full repaint.
                let before = (app.tab, app.popup.is_some(), app.theme, app.chart_style, app.sel_gpu);
                loop {
                    match event::read()? {
                        Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k, &sampler),
                        Event::Mouse(m) => app.on_mouse(m),
                        _ => {}
                    }
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
                if app.chart_ctx().pixels() && before != (app.tab, app.popup.is_some(), app.theme, app.chart_style, app.sel_gpu) {
                    app.gfx.invalidate();
                    terminal.clear()?;
                }
                dirty = true;
            }
            if app.quit {
                return Ok(());
            }
        }
    })();
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    crate::config::save(&Config {
        theme: Some(app.theme().name.to_string()),
        interval: Some(app.interval.as_secs_f64()),
        sort: Some(app.sort.label().to_string()),
        sort_desc: Some(app.sort_desc),
        full_cmd: Some(app.full_cmd),
        chart_style: Some(app.chart_style),
    });
    res
}
