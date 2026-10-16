//! Interactive TUI: state and input handling live here; rendering is in the
//! view modules (`draw`, `overview`, `procs`, `charts`, `details`, `popups`).

mod charts;
mod details;
mod draw;
mod gfx;
mod history;
mod overview;
mod popups;
mod procs;
mod theme;
mod timeline;
mod widgets;

pub use gfx::ChartStyle;

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;
use ratatui::widgets::TableState;
use serde::{Deserialize, Serialize};

use crate::collector::{Collector, Sampler};
use crate::config::Config;
use crate::host::{KillSignal, send_signal};
use crate::model::{GpuInfo, GpuSample, PidMapping, ProcRow, Snapshot, SystemInfo};
use history::History;

/// What the UI needs from the command line.
pub struct UiOptions {
    pub theme: Option<String>,
    pub readonly: bool,
    /// "auto", "off", or a protocol to force ("kitty", "sixel", "iterm2").
    pub graphics: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Overview,
    Charts,
    Details,
    Events,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Overview, Tab::Charts, Tab::Details, Tab::Events];
    fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Charts => "Charts",
            Tab::Details => "Details",
            Tab::Events => "Events",
        }
    }
    fn step(self, d: isize) -> Tab {
        let i = Tab::ALL.iter().position(|&t| t == self).unwrap_or(0) as isize;
        Tab::ALL[(i + d).rem_euclid(Tab::ALL.len() as isize) as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    GpuMem,
    SharedMem,
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
    const CYCLE: [SortKey; 10] = [
        SortKey::GpuMem,
        SortKey::SharedMem,
        SortKey::Sm,
        SortKey::Cpu,
        SortKey::Rss,
        SortKey::Time,
        SortKey::Pid,
        SortKey::User,
        SortKey::Name,
        SortKey::Gpu,
    ];
    /// Natural direction: numbers biggest-first, text A-Z.
    fn default_desc(self) -> bool {
        !matches!(self, SortKey::Pid | SortKey::User | SortKey::Name | SortKey::Gpu)
    }
    fn step(self, d: isize) -> SortKey {
        let i = Self::CYCLE.iter().position(|&s| s == self).unwrap_or(0) as isize;
        Self::CYCLE[(i + d).rem_euclid(Self::CYCLE.len() as isize) as usize]
    }
}

/// How the process list is filtered and ordered.
#[derive(Clone, Debug)]
struct ViewSpec {
    filter: String,
    gpu: Option<usize>,
    sort: SortKey,
    desc: bool,
}

/// Indices into `procs`, filtered and sorted per `v`. Pure, so it is tested.
fn filter_sort(procs: &[ProcRow], v: &ViewSpec) -> Vec<usize> {
    let f = v.filter.to_lowercase();
    let mut idx: Vec<usize> = (0..procs.len())
        .filter(|&i| {
            let p = &procs[i];
            v.gpu.is_none_or(|g| p.gpu == g)
                && (f.is_empty()
                    || p.cmdline.to_lowercase().contains(&f)
                    || p.user.as_deref().is_some_and(|u| u.to_lowercase().contains(&f))
                    || p.pid().to_string() == f
                    || p.local_pid.is_some_and(|l| l.to_string() == f))
        })
        .collect();
    // Sorting by a column nobody reports (GPU memory under WDDM without
    // counters, Apple) is useless; fall back to GPU utilization.
    let key = match v.sort {
        SortKey::GpuMem if idx.iter().all(|&i| procs[i].usage.gpu_mem.is_none()) => SortKey::Sm,
        k => k,
    };
    let num = |p: &ProcRow| -> f64 {
        let u = &p.usage;
        match key {
            SortKey::GpuMem => u.gpu_mem.map(|v| v as f64),
            SortKey::SharedMem => u.gpu_mem_shared.map(|v| v as f64),
            SortKey::Sm => u.sm_util.map(f64::from),
            SortKey::Cpu => p.cpu.map(f64::from),
            SortKey::Rss => p.rss.map(|v| v as f64),
            SortKey::Time => p.runtime.map(|v| v as f64),
            _ => None,
        }
        .unwrap_or(-1.0)
    };
    idx.sort_by(|&a, &b| {
        let (pa, pb) = (&procs[a], &procs[b]);
        let o = match key {
            SortKey::Pid => pa.pid().cmp(&pb.pid()),
            SortKey::User => pa.user.cmp(&pb.user),
            SortKey::Name => pa.name.to_lowercase().cmp(&pb.name.to_lowercase()),
            SortKey::Gpu => pa.gpu.cmp(&pb.gpu),
            _ => num(pa).total_cmp(&num(pb)),
        };
        let o = if v.desc { o.reverse() } else { o };
        o.then(pa.gpu.cmp(&pb.gpu)).then(pa.pid().cmp(&pb.pid()))
    });
    idx
}

enum Popup {
    Help,
    Process {
        key: (usize, u32),
        /// Fetched once when the popup opens.
        details: Option<Box<crate::host::ProcDetails>>,
        /// Show the whole environment instead of the GPU-relevant part.
        full_env: bool,
    },
    /// (gpu, driver pid, local pid, name) per target.
    Kill {
        targets: Vec<(usize, u32, u32, String)>,
        signal: usize,
    },
}

/// Screen regions recorded during draw, for mouse hit-testing.
#[derive(Default)]
struct Hit {
    tabs: Vec<(Rect, Tab)>,
    gpus: Vec<(Rect, usize)>,
    proc_body: Rect,
    proc_header: Vec<(u16, u16, SortKey)>,
    proc_header_y: u16,
}

struct App {
    info: SystemInfo,
    snap: Option<Snapshot>,
    hist: History,
    tab: Tab,
    sel_gpu: usize,
    /// Visible process rows: indices into `snap.processes`.
    view: Vec<usize>,
    spec: ViewSpec,
    gpu_only: bool,
    table: RefCell<TableState>,
    sel_key: Option<(usize, u32)>,
    filter_edit: bool,
    full_cmd: bool,
    tagged: HashSet<(usize, u32)>,
    popup: Option<Popup>,
    toast: Option<(String, Instant, bool)>,
    theme: usize,
    truecolor: bool,
    readonly: bool,
    paused: bool,
    interval: Duration,
    hit: Hit,
    me: Option<String>,
    gfx: gfx::Gfx,
    chart_style: ChartStyle,
    /// The style to persist: only an explicit choice (`G`), never a fallback
    /// forced by the current terminal.
    saved_chart_style: Option<ChartStyle>,
    /// Bumped on every new snapshot; keys the chart image cache.
    version: u64,
    events: std::collections::VecDeque<crate::events::Event>,
    events_scroll: usize,
    quit: bool,
}

impl App {
    fn new(info: SystemInfo, interval: Duration, opts: &UiOptions, cfg: &Config, gfx: gfx::Gfx) -> Self {
        let theme = opts.theme.clone().or(crate::brand::env("THEME")).or(cfg.theme.clone()).and_then(|t| theme::by_name(&t)).unwrap_or(0);
        let sort = cfg.sort.unwrap_or(SortKey::GpuMem);
        let chart_style = match cfg.chart_style {
            Some(ChartStyle::Pixels) | None if gfx.pixels_available() => ChartStyle::Pixels,
            Some(ChartStyle::Pixels) | None => ChartStyle::Blocks,
            Some(s) => s,
        };
        App {
            info,
            snap: None,
            hist: History::default(),
            tab: Tab::Overview,
            sel_gpu: 0,
            view: Vec::new(),
            spec: ViewSpec { filter: String::new(), gpu: None, sort, desc: cfg.sort_desc.unwrap_or(sort.default_desc()) },
            gpu_only: false,
            table: RefCell::new(TableState::default().with_selected(Some(0))),
            sel_key: None,
            filter_edit: false,
            full_cmd: cfg.full_cmd.unwrap_or(false),
            tagged: HashSet::new(),
            popup: None,
            toast: None,
            theme,
            truecolor: theme::truecolor_supported(),
            readonly: opts.readonly,
            paused: false,
            interval,
            hit: Hit::default(),
            me: std::env::var("USER").or_else(|_| std::env::var("USERNAME")).ok(),
            gfx,
            chart_style,
            saved_chart_style: cfg.chart_style,
            version: 0,
            events: Default::default(),
            events_scroll: 0,
            quit: false,
        }
    }

    fn theme(&self) -> &'static theme::Theme {
        &theme::THEMES[self.theme]
    }

    fn gpu(&self, i: usize) -> Option<&GpuInfo> {
        self.info.gpus.get(i)
    }

    fn sample(&self, i: usize) -> Option<&GpuSample> {
        self.snap.as_ref()?.gpus.get(i)
    }

    fn procs(&self) -> &[ProcRow] {
        self.snap.as_ref().map(|s| s.processes.as_slice()).unwrap_or(&[])
    }

    /// Visible rows in display order.
    fn rows(&self) -> impl Iterator<Item = &ProcRow> {
        let all = self.procs();
        self.view.iter().filter_map(move |&i| all.get(i))
    }

    fn row_by_key(&self, key: (usize, u32)) -> Option<&ProcRow> {
        self.procs().iter().find(|p| p.key() == key)
    }

    fn row(&self, n: usize) -> Option<&ProcRow> {
        self.procs().get(*self.view.get(n)?)
    }

    fn power_frac(&self, i: usize, s: &GpuSample) -> Option<f64> {
        self.hist.power_frac(i, s, self.gpu(i)?.power_limit_max)
    }

    fn chart_ctx(&self) -> gfx::ChartCtx<'_> {
        let bg = match self.gfx.bg {
            Some((r, g, b)) => ratatui::style::Color::Rgb(r, g, b),
            None => ratatui::style::Color::Rgb(0x1a, 0x1b, 0x26),
        };
        gfx::ChartCtx { gfx: &self.gfx, style: self.chart_style, version: self.version, theme: self.theme, bg }
    }

    fn ingest(&mut self, s: Snapshot) {
        self.hist.push(&self.info, &s);
        self.events.extend(s.events.iter().cloned());
        while self.events.len() > 2000 {
            self.events.pop_front();
        }
        self.tagged.retain(|k| s.processes.iter().any(|p| p.key() == *k));
        self.snap = Some(s);
        self.version += 1;
        self.refresh_view();
    }

    /// Recompute visible rows and keep the selection on the same process.
    fn refresh_view(&mut self) {
        self.spec.gpu = self.gpu_only.then_some(self.sel_gpu);
        self.view = filter_sort(self.procs(), &self.spec);
        let n = self.view.len();
        let idx = self
            .sel_key
            .and_then(|k| self.rows().position(|p| p.key() == k))
            .unwrap_or_else(|| self.table.borrow().selected().unwrap_or(0).min(n.saturating_sub(1)));
        self.sel_key = self.row(idx).map(ProcRow::key);
        self.table.borrow_mut().select(if n == 0 { None } else { Some(idx) });
    }

    fn move_sel(&mut self, delta: isize) {
        if self.view.is_empty() {
            return;
        }
        let cur = self.table.borrow().selected().unwrap_or(0) as isize;
        let idx = cur.saturating_add(delta).clamp(0, self.view.len() as isize - 1) as usize;
        self.sel_key = self.row(idx).map(ProcRow::key);
        self.table.borrow_mut().select(Some(idx));
    }

    fn set_sort(&mut self, k: SortKey) {
        if self.spec.sort == k {
            self.spec.desc = !self.spec.desc;
        } else {
            self.spec.sort = k;
            self.spec.desc = k.default_desc();
        }
        self.refresh_view();
    }

    fn toast(&mut self, msg: impl Into<String>, err: bool) {
        self.toast = Some((msg.into(), Instant::now(), err));
    }

    fn open_kill(&mut self) {
        if self.readonly {
            self.toast("read-only mode", true);
            return;
        }
        // Tagged processes, even those hidden by the current filter; else the selection.
        let sel: Vec<&ProcRow> = if self.tagged.is_empty() {
            self.table.borrow().selected().and_then(|i| self.row(i)).into_iter().collect()
        } else {
            self.procs().iter().filter(|p| self.tagged.contains(&p.key())).collect()
        };
        let mut targets = Vec::new();
        let mut skipped = 0;
        for p in sel {
            match (p.local_pid, p.mapping) {
                (Some(l), m) if m != PidMapping::External => targets.push((p.gpu, p.pid(), l, p.name.clone())),
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
        // Default to SIGTERM where signals exist.
        let signal = KillSignal::available().len().saturating_sub(2);
        self.popup = Some(Popup::Kill { targets, signal });
    }

    fn on_key(&mut self, k: KeyEvent, sampler: &Sampler) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.popup.is_some() {
            self.on_popup_key(k);
            return;
        }
        if self.filter_edit {
            match k.code {
                KeyCode::Esc => {
                    self.spec.filter.clear();
                    self.filter_edit = false;
                }
                KeyCode::Enter => self.filter_edit = false,
                KeyCode::Backspace => {
                    self.spec.filter.pop();
                }
                KeyCode::Char(c) => self.spec.filter.push(c),
                _ => {}
            }
            self.refresh_view();
            return;
        }
        let ngpu = self.info.gpus.len().max(1);
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if !self.spec.filter.is_empty() {
                    self.spec.filter.clear();
                    self.refresh_view();
                } else if !self.tagged.is_empty() {
                    self.tagged.clear();
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Char('?') | KeyCode::F(1) => self.popup = Some(Popup::Help),
            KeyCode::Tab => self.tab = self.tab.step(1),
            KeyCode::BackTab => self.tab = self.tab.step(-1),
            KeyCode::Char('1') => self.tab = Tab::Overview,
            KeyCode::Char('2') => self.tab = Tab::Charts,
            KeyCode::Char('3') => self.tab = Tab::Details,
            KeyCode::Char('4') => self.tab = Tab::Events,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') | KeyCode::PageUp | KeyCode::PageDown
                if self.tab == Tab::Events =>
            {
                let d: isize = match k.code {
                    KeyCode::Up | KeyCode::Char('k') => -1,
                    KeyCode::Down | KeyCode::Char('j') => 1,
                    KeyCode::PageUp => -20,
                    _ => 20,
                };
                self.events_scroll = self.events_scroll.saturating_add_signed(d).min(self.events.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::Home => self.move_sel(isize::MIN),
            KeyCode::End => self.move_sel(isize::MAX),
            KeyCode::Left | KeyCode::Char('[') => {
                self.sel_gpu = (self.sel_gpu + ngpu - 1) % ngpu;
                self.refresh_view();
            }
            KeyCode::Right | KeyCode::Char(']') => {
                self.sel_gpu = (self.sel_gpu + 1) % ngpu;
                self.refresh_view();
            }
            KeyCode::Char('s') | KeyCode::F(6) | KeyCode::Char('S') => {
                let k = self.spec.sort.step(if k.code == KeyCode::Char('S') { -1 } else { 1 });
                self.spec.sort = k;
                self.spec.desc = k.default_desc();
                self.refresh_view();
            }
            KeyCode::Char('r') => {
                self.spec.desc = !self.spec.desc;
                self.refresh_view();
            }
            KeyCode::Char('/') => self.filter_edit = true,
            KeyCode::Char('g') => {
                self.gpu_only = !self.gpu_only;
                self.refresh_view();
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
            KeyCode::Enter => {
                if let Some(key) = self.sel_key {
                    let details = self.row_by_key(key).and_then(|p| p.local_pid).and_then(crate::host::process_details).map(Box::new);
                    self.popup = Some(Popup::Process { key, details, full_env: false });
                }
            }
            KeyCode::Char('K') | KeyCode::Char('x') | KeyCode::Delete | KeyCode::F(9) => self.open_kill(),
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

    fn on_popup_key(&mut self, k: KeyEvent) {
        let Some(popup) = &mut self.popup else { return };
        match popup {
            Popup::Kill { targets, signal } => {
                let n = KillSignal::available().len();
                match k.code {
                    KeyCode::Left | KeyCode::Char('h') => *signal = signal.saturating_sub(1),
                    KeyCode::Right | KeyCode::Char('l') => *signal = (*signal + 1).min(n - 1),
                    KeyCode::Enter | KeyCode::Char('y') => {
                        let sig = KillSignal::available()[*signal];
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
                }
            }
            Popup::Process { full_env, .. } => match k.code {
                KeyCode::Char('e') => *full_env = !*full_env,
                KeyCode::Char('K') | KeyCode::Char('x') | KeyCode::Delete | KeyCode::F(9) => {
                    self.popup = None;
                    self.open_kill();
                }
                _ => self.popup = None,
            },
            Popup::Help => self.popup = None,
        }
    }

    fn cycle_chart_style(&mut self) {
        let order: Vec<ChartStyle> = [ChartStyle::Pixels, ChartStyle::Blocks, ChartStyle::Braille]
            .into_iter()
            .filter(|s| *s != ChartStyle::Pixels || self.gfx.pixels_available())
            .collect();
        let i = order.iter().position(|s| *s == self.chart_style).unwrap_or(0);
        self.chart_style = order[(i + 1) % order.len()];
        self.saved_chart_style = Some(self.chart_style);
        let extra = if self.chart_style == ChartStyle::Pixels {
            format!(" ({}) · press G again if charts stay empty", self.gfx.protocol_name())
        } else {
            String::new()
        };
        self.toast(format!("charts: {}{extra}", self.chart_style.label()), false);
    }

    fn set_interval(&mut self, sampler: &Sampler, faster: bool) {
        const STEPS: [u64; 9] = [100, 250, 500, 1000, 2000, 3000, 5000, 10000, 30000];
        let cur = self.interval.as_millis() as u64;
        let next = if faster {
            STEPS.iter().rev().find(|&&s| s < cur).copied().unwrap_or(STEPS[0])
        } else {
            STEPS.iter().find(|&&s| s > cur).copied().unwrap_or(STEPS[STEPS.len() - 1])
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
                } else if let Some(&(_, t)) = self.hit.tabs.iter().find(|(r, _)| inside(*r)) {
                    self.tab = t;
                } else if let Some(&(_, g)) = self.hit.gpus.iter().find(|(r, _)| inside(*r)) {
                    self.sel_gpu = g;
                    self.refresh_view();
                } else if m.row == self.hit.proc_header_y {
                    if let Some(&(_, _, k)) = self.hit.proc_header.iter().find(|(x0, x1, _)| m.column >= *x0 && m.column < *x1) {
                        self.set_sort(k);
                    }
                } else if inside(self.hit.proc_body) {
                    let idx = self.table.borrow().offset() + (m.row - self.hit.proc_body.y) as usize;
                    if let Some(key) = self.row(idx).map(ProcRow::key) {
                        self.sel_key = Some(key);
                        self.table.borrow_mut().select(Some(idx));
                    }
                }
            }
            _ => {}
        }
    }

    fn save_config(&self) {
        crate::config::save(&Config {
            theme: Some(self.theme().name.to_string()),
            interval: Some(self.interval.as_secs_f64()),
            sort: Some(self.spec.sort),
            sort_desc: Some(self.spec.desc),
            full_cmd: Some(self.full_cmd),
            chart_style: self.saved_chart_style,
        });
    }
}

pub fn run(collector: Collector, info: SystemInfo, interval: Duration, opts: &UiOptions, cfg: Config) -> anyhow::Result<()> {
    let sampler = Sampler::spawn(collector, interval);
    let mut terminal = ratatui::init();
    crate::logging::capture(true);
    // Probe for pixel graphics after entering the alternate screen and before
    // reading any input (the terminal answers on stdin).
    let gfx = match opts.graphics.as_str() {
        "off" => gfx::Gfx::none(),
        "auto" => gfx::Gfx::detect(None),
        forced => gfx::Gfx::detect(Some(forced)),
    };
    let mut app = App::new(info, interval, opts, &cfg, gfx);
    if app.chart_ctx().pixels() {
        app.toast(format!("charts: pixels ({}) · press G if they stay empty", app.gfx.protocol_name()), false);
    }
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = (|| -> anyhow::Result<()> {
        let mut dirty = true;
        let mut last_draw = Instant::now();
        loop {
            while let Ok(s) = sampler.rx.try_recv() {
                app.ingest(s);
                dirty = true;
            }
            if app.toast.as_ref().is_some_and(|(_, t, _)| t.elapsed() > Duration::from_secs(3)) {
                app.toast = None;
                dirty = true;
            }
            // The header clock ticks once a second even when paused.
            if last_draw.elapsed() >= Duration::from_secs(1) {
                dirty = true;
            }
            if dirty {
                let mut hit = Hit::default();
                terminal.draw(|f| {
                    hit = draw::draw(f, &app);
                    if !app.truecolor {
                        theme::quantize(f.buffer_mut());
                    }
                })?;
                app.hit = hit;
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
    crate::logging::capture(false);
    app.save_config();
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GpuProcess;

    fn row(gpu: usize, pid: u32, mem: Option<u64>, sm: Option<f32>, cmd: &str) -> ProcRow {
        ProcRow {
            gpu,
            usage: GpuProcess { pid, gpu_mem: mem, sm_util: sm, ..Default::default() },
            local_pid: Some(pid),
            mapping: PidMapping::Direct,
            name: cmd.split(' ').next().unwrap_or("").into(),
            cmdline: cmd.into(),
            user: Some("alice".into()),
            cpu: None,
            rss: None,
            runtime: None,
            spilling: false,
        }
    }

    fn spec(sort: SortKey) -> ViewSpec {
        ViewSpec { filter: String::new(), gpu: None, sort, desc: sort.default_desc() }
    }

    #[test]
    fn sorts_by_memory_desc_with_none_last() {
        let p = [row(0, 1, Some(10), None, "a"), row(0, 2, None, None, "b"), row(0, 3, Some(30), None, "c")];
        assert_eq!(filter_sort(&p, &spec(SortKey::GpuMem)), [2, 0, 1]);
    }

    #[test]
    fn memory_sort_falls_back_to_utilization() {
        let p = [row(0, 1, None, Some(5.0), "a"), row(0, 2, None, Some(50.0), "b")];
        assert_eq!(filter_sort(&p, &spec(SortKey::GpuMem)), [1, 0]);
    }

    #[test]
    fn filters_by_text_pid_and_gpu() {
        let p = [row(0, 1, None, None, "python train.py"), row(1, 22, None, None, "ollama serve")];
        let mut v = spec(SortKey::Pid);
        v.filter = "TRAIN".into();
        assert_eq!(filter_sort(&p, &v), [0]);
        v.filter = "22".into();
        assert_eq!(filter_sort(&p, &v), [1]);
        v.filter.clear();
        v.gpu = Some(1);
        assert_eq!(filter_sort(&p, &v), [1]);
    }

    #[test]
    fn ties_break_by_gpu_then_pid() {
        let p = [row(1, 5, Some(1), None, "x"), row(0, 9, Some(1), None, "x"), row(0, 3, Some(1), None, "x")];
        assert_eq!(filter_sort(&p, &spec(SortKey::GpuMem)), [2, 1, 0]);
    }
}
