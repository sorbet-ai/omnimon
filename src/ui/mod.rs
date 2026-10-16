//! Interactive TUI: state and input handling live here; rendering is in the
//! view modules (`draw`, `overview`, `procs`, `charts`, `details`, `popups`).

mod charts;
mod details;
mod draw;
mod filter;
mod gfx;
mod history;
mod input;
mod overview;
mod popups;
mod procs;
mod procview;
mod recorder;
mod store;
mod theme;
mod timeline;
mod widgets;

pub use gfx::ChartStyle;
pub use input::Action;
pub use procview::SortKey;
pub use theme::Look;

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;

use crate::collector::{Sampler, Source};
use crate::config::Config;
use crate::host::{KillSignal, send_signal};
use crate::model::{GpuInfo, GpuSample, PidMapping, ProcRow, Snapshot, SystemInfo};

/// What the UI needs from the command line.
pub struct UiOptions {
    pub theme: Option<String>,
    pub readonly: bool,
    /// "auto", "off", or a protocol to force ("kitty", "sixel", "iterm2").
    pub graphics: String,
    /// Watching another machine (`--connect`): its address.
    pub remote: Option<String>,
    /// Chart colors (`--look`).
    pub look: Option<String>,
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

/// Overview panels that can be hidden (`:hide host`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Panel {
    #[serde(rename = "gpus", alias = "gpu", alias = "cards")]
    Cards,
    #[serde(alias = "cpu", alias = "system")]
    Host,
    #[serde(alias = "processes", alias = "table")]
    Procs,
}

impl Panel {
    const ALL: [Panel; 3] = [Panel::Cards, Panel::Host, Panel::Procs];
    fn name(self) -> &'static str {
        match self {
            Panel::Cards => "gpus",
            Panel::Host => "host",
            Panel::Procs => "procs",
        }
    }
    fn parse(s: &str) -> Option<Panel> {
        match s {
            "gpus" | "gpu" | "cards" => Some(Panel::Cards),
            "host" | "cpu" | "system" => Some(Panel::Host),
            "procs" | "processes" | "table" => Some(Panel::Procs),
            _ => None,
        }
    }
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
    /// Adjust the selected GPU's power limit.
    Tune {
        gpu: usize,
        watts: f64,
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
    /// Snapshots, rewind, chart history, events.
    store: store::Store,
    tab: Tab,
    sel_gpu: usize,
    /// The process list: filter, sort, selection, tags.
    view: procview::ProcView,
    popup: Option<Popup>,
    toast: Option<(String, Instant, bool)>,
    theme: usize,
    look: Look,
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
    events_scroll: usize,
    keymap: input::Keymap,
    /// Command line being typed after `:`.
    cmd_edit: Option<String>,
    hidden: HashSet<Panel>,
    remote: Option<String>,
    alerts: crate::alerts::Alerts,
    price_per_kwh: Option<f64>,
    quit: bool,
}

impl App {
    fn new(info: SystemInfo, interval: Duration, opts: &UiOptions, cfg: &Config, gfx: gfx::Gfx) -> Self {
        let theme = opts.theme.clone().or(crate::brand::env("THEME")).or(cfg.theme.clone()).and_then(|t| theme::by_name(&t)).unwrap_or(0);
        let chart_style = match cfg.chart_style {
            Some(ChartStyle::Pixels) | None if gfx.pixels_available() => ChartStyle::Pixels,
            Some(ChartStyle::Pixels) | None => ChartStyle::Blocks,
            Some(s) => s,
        };
        App {
            info,
            store: store::Store::default(),
            tab: Tab::Overview,
            sel_gpu: 0,
            view: procview::ProcView::new(cfg.sort.unwrap_or(SortKey::GpuMem), cfg.sort_desc, cfg.full_cmd.unwrap_or(false)),
            popup: None,
            toast: None,
            theme,
            look: opts.look.as_deref().and_then(Look::parse).or(cfg.look).unwrap_or_default(),
            truecolor: theme::truecolor_supported(),
            readonly: opts.readonly,
            paused: false,
            interval,
            hit: Hit::default(),
            me: std::env::var("USER").or_else(|_| std::env::var("USERNAME")).ok(),
            gfx,
            chart_style,
            saved_chart_style: cfg.chart_style,
            events_scroll: 0,
            keymap: input::Keymap::new(&cfg.keys),
            cmd_edit: None,
            hidden: cfg.hidden.iter().copied().collect(),
            remote: opts.remote.clone(),
            alerts: crate::alerts::Alerts::new(cfg.alert_command.clone(), cfg.alert_min),
            price_per_kwh: cfg.price_per_kwh,
            quit: false,
        }
    }

    fn theme(&self) -> &'static theme::Theme {
        &theme::THEMES[self.theme]
    }

    fn gpu(&self, i: usize) -> Option<&GpuInfo> {
        self.info.gpus.get(i)
    }

    /// The snapshot on screen (live, or a past one while rewinding).
    fn snap(&self) -> Option<&Snapshot> {
        self.store.current()
    }

    fn sample(&self, i: usize) -> Option<&GpuSample> {
        self.snap()?.gpus.get(i)
    }

    fn procs(&self) -> &[ProcRow] {
        self.snap().map(|s| s.processes.as_slice()).unwrap_or(&[])
    }

    /// Visible rows in display order.
    fn rows(&self) -> impl Iterator<Item = &ProcRow> {
        let all = self.procs();
        self.view.rows.iter().filter_map(move |&i| all.get(i))
    }

    fn row_by_key(&self, key: (usize, u32)) -> Option<&ProcRow> {
        self.procs().iter().find(|p| p.key() == key)
    }

    fn row(&self, n: usize) -> Option<&ProcRow> {
        self.view.row(self.procs(), n)
    }

    fn power_frac(&self, i: usize, s: &GpuSample) -> Option<f64> {
        self.store.hist.power_frac(i, s, self.gpu(i)?.power_limit_max)
    }

    fn chart_ctx(&self) -> gfx::ChartCtx<'_> {
        let bg = match self.gfx.bg {
            Some((r, g, b)) => ratatui::style::Color::Rgb(r, g, b),
            None => ratatui::style::Color::Rgb(0x1a, 0x1b, 0x26),
        };
        gfx::ChartCtx { gfx: &self.gfx, style: self.chart_style, version: self.store.version, theme: self.theme, bg }
    }

    fn ingest(&mut self, s: Snapshot) {
        self.alerts.handle(&s.events);
        self.view.tagged.retain(|k| s.processes.iter().any(|p| p.key() == *k));
        self.store.ingest(&self.info, s, self.interval.as_secs_f64());
        self.refresh_view();
    }

    fn set_rewind(&mut self, n: usize) {
        self.store.set_rewind(n);
        self.refresh_view();
    }

    /// Recompute visible rows and keep the selection on the same process.
    fn refresh_view(&mut self) {
        let procs = self.store.current().map(|s| s.processes.as_slice()).unwrap_or(&[]);
        self.view.refresh(procs, self.sel_gpu);
    }

    fn move_sel(&mut self, delta: isize) {
        let procs = self.store.current().map(|s| s.processes.as_slice()).unwrap_or(&[]);
        self.view.move_sel(procs, delta);
    }

    fn set_sort(&mut self, k: SortKey) {
        self.view.set_sort(k);
        self.refresh_view();
    }

    fn scroll_events(&mut self, d: isize) {
        self.events_scroll = self.events_scroll.saturating_add_signed(d).min(self.store.events.len().saturating_sub(1));
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
        let sel: Vec<&ProcRow> = if self.view.tagged.is_empty() {
            self.view.table.borrow().selected().and_then(|i| self.row(i)).into_iter().collect()
        } else {
            self.procs().iter().filter(|p| self.view.tagged.contains(&p.key())).collect()
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
        if matches!(self.popup, Some(Popup::Tune { .. })) {
            self.on_tune_key(k, sampler);
            return;
        }
        if self.popup.is_some() {
            self.on_popup_key(k);
            return;
        }
        if self.view.filter_edit {
            match k.code {
                KeyCode::Esc => {
                    self.view.spec.filter.clear();
                    self.view.filter_edit = false;
                }
                KeyCode::Enter => self.view.filter_edit = false,
                KeyCode::Backspace => {
                    self.view.spec.filter.pop();
                }
                KeyCode::Char(c) => self.view.spec.filter.push(c),
                _ => {}
            }
            self.refresh_view();
            return;
        }
        if let Some(line) = &mut self.cmd_edit {
            match k.code {
                KeyCode::Esc => self.cmd_edit = None,
                KeyCode::Enter => {
                    let line = std::mem::take(line);
                    self.cmd_edit = None;
                    match self.run_command(&line, sampler) {
                        Ok(msg) if !msg.is_empty() => self.toast(msg, false),
                        Ok(_) => {}
                        Err(e) => self.toast(e, true),
                    }
                }
                KeyCode::Backspace => {
                    line.pop();
                }
                KeyCode::Char(c) => line.push(c),
                _ => {}
            }
            return;
        }
        if k.code == KeyCode::Esc {
            if !self.view.spec.filter.is_empty() {
                self.view.spec.filter.clear();
                self.refresh_view();
            } else if !self.view.tagged.is_empty() {
                self.view.tagged.clear();
            } else {
                self.quit = true;
            }
            return;
        }
        if let Some(a) = self.keymap.action(&k) {
            self.apply(a, sampler);
        }
    }

    fn apply(&mut self, a: Action, sampler: &Sampler) {
        let ngpu = self.info.gpus.len().max(1);
        let scroll_events = self.tab == Tab::Events;
        match a {
            Action::Quit => self.quit = true,
            Action::Help => self.popup = Some(Popup::Help),
            Action::NextTab => self.tab = self.tab.step(1),
            Action::PrevTab => self.tab = self.tab.step(-1),
            Action::Overview => self.tab = Tab::Overview,
            Action::Charts => self.tab = Tab::Charts,
            Action::Details => self.tab = Tab::Details,
            Action::Events => self.tab = Tab::Events,
            Action::Up | Action::Down | Action::PageUp | Action::PageDown if scroll_events => self.scroll_events(match a {
                Action::Up => -1,
                Action::Down => 1,
                Action::PageUp => -20,
                _ => 20,
            }),
            Action::Up => self.move_sel(-1),
            Action::Down => self.move_sel(1),
            Action::PageUp => self.move_sel(-10),
            Action::PageDown => self.move_sel(10),
            Action::Top => self.move_sel(isize::MIN),
            Action::Bottom => self.move_sel(isize::MAX),
            Action::PrevGpu | Action::NextGpu => {
                let d = if a == Action::PrevGpu { ngpu - 1 } else { 1 };
                self.sel_gpu = (self.sel_gpu + d) % ngpu;
                self.refresh_view();
            }
            Action::SortNext | Action::SortPrev => {
                let k = self.view.spec.sort.step(if a == Action::SortPrev { -1 } else { 1 });
                self.view.spec.sort = k;
                self.view.spec.desc = k.default_desc();
                self.refresh_view();
            }
            Action::SortReverse => {
                self.view.spec.desc = !self.view.spec.desc;
                self.refresh_view();
            }
            Action::Filter => self.view.filter_edit = true,
            Action::Command => self.cmd_edit = Some(String::new()),
            Action::GpuOnly => {
                self.view.gpu_only = !self.view.gpu_only;
                self.refresh_view();
            }
            Action::FullCommand => self.view.full_cmd = !self.view.full_cmd,
            Action::Tag => {
                if let Some(key) = self.view.sel_key {
                    if !self.view.tagged.remove(&key) {
                        self.view.tagged.insert(key);
                    }
                    self.move_sel(1);
                }
            }
            Action::ProcessDetails => {
                if let Some(key) = self.view.sel_key {
                    // Process details come from this machine's process table.
                    let local = if self.remote.is_some() { None } else { self.row_by_key(key).and_then(|p| p.local_pid) };
                    let details = local.and_then(crate::host::process_details).map(Box::new);
                    self.popup = Some(Popup::Process { key, details, full_env: false });
                }
            }
            Action::Kill => self.open_kill(),
            Action::ChartStyle => self.cycle_chart_style(),
            Action::Look => {
                let i = Look::ALL.iter().position(|l| *l == self.look).unwrap_or(0);
                self.look = Look::ALL[(i + 1) % Look::ALL.len()];
                self.toast(format!("chart colors: {}", self.look.name()), false);
            }
            Action::Theme => {
                self.theme = (self.theme + 1) % theme::THEMES.len();
                self.toast(format!("theme: {}", self.theme().name), false);
            }
            Action::Pause => {
                self.paused = !self.paused;
                sampler.ctl.paused.store(self.paused, Ordering::Relaxed);
                sampler.poke();
            }
            Action::Slower => self.set_interval(sampler, false),
            Action::Faster => self.set_interval(sampler, true),
            Action::RewindBack => self.set_rewind(self.store.rewind + 10),
            Action::RewindBackFar => self.set_rewind(self.store.rewind + 60),
            Action::RewindForward => self.set_rewind(self.store.rewind.saturating_sub(10)),
            Action::RewindForwardFar => self.set_rewind(self.store.rewind.saturating_sub(60)),
            Action::Live => self.set_rewind(0),
            Action::Tune => self.open_tune(),
        }
    }

    /// `:` commands. Returns a confirmation for the toast.
    fn run_command(&mut self, line: &str, sampler: &Sampler) -> Result<String, String> {
        let mut words = line.split_whitespace();
        let Some(cmd) = words.next() else { return Ok(String::new()) };
        let rest: Vec<&str> = words.collect();
        let arg = rest.first().copied().unwrap_or("");
        match cmd {
            "q" | "quit" => {
                self.apply(Action::Quit, sampler);
                Ok(String::new())
            }
            "theme" => {
                let i = theme::by_name(arg)
                    .ok_or_else(|| format!("themes: {}", theme::THEMES.iter().map(|t| t.name).collect::<Vec<_>>().join(", ")))?;
                self.theme = i;
                Ok(format!("theme: {arg}"))
            }
            "sort" => {
                let key = SortKey::parse(arg)
                    .ok_or_else(|| format!("sort by: {}", SortKey::CYCLE.iter().map(|k| k.name()).collect::<Vec<_>>().join(" ")))?;
                self.view.spec.sort = key;
                self.view.spec.desc = match rest.get(1) {
                    Some(&"asc") => false,
                    Some(&"desc") => true,
                    _ => key.default_desc(),
                };
                self.refresh_view();
                Ok(format!("sorted by {}", key.name()))
            }
            "filter" | "f" => {
                self.view.spec.filter = rest.join(" ");
                self.refresh_view();
                Ok(String::new())
            }
            "clear" => {
                self.view.spec.filter.clear();
                self.view.tagged.clear();
                self.refresh_view();
                Ok("cleared".into())
            }
            "hide" | "show" | "toggle" => {
                let panel = Panel::parse(arg)
                    .ok_or_else(|| format!("panels: {}", Panel::ALL.iter().map(|p| p.name()).collect::<Vec<_>>().join(", ")))?;
                let hide = match cmd {
                    "hide" => true,
                    "show" => false,
                    _ => !self.hidden.contains(&panel),
                };
                if hide {
                    self.hidden.insert(panel);
                } else {
                    self.hidden.remove(&panel);
                }
                Ok(format!("{} {}", if hide { "hid" } else { "showing" }, panel.name()))
            }
            "look" | "colors" => {
                self.look = Look::parse(arg)
                    .ok_or_else(|| format!("looks: {}", Look::ALL.iter().map(|l| l.name()).collect::<Vec<_>>().join(", ")))?;
                Ok(format!("chart colors: {arg}"))
            }
            "charts" => {
                self.chart_style = match arg {
                    "pixels" if self.gfx.pixels_available() => ChartStyle::Pixels,
                    "pixels" => return Err("this terminal didn't report image support".into()),
                    "blocks" => ChartStyle::Blocks,
                    "braille" => ChartStyle::Braille,
                    _ => return Err("charts: pixels, blocks, braille".into()),
                };
                self.saved_chart_style = Some(self.chart_style);
                Ok(format!("charts: {arg}"))
            }
            "interval" | "i" => {
                let secs: f64 = arg.parse().map_err(|_| "interval <seconds>")?;
                let ms = (secs.clamp(0.1, 60.0) * 1000.0) as u64;
                self.interval = Duration::from_millis(ms);
                sampler.ctl.interval_ms.store(ms, Ordering::Relaxed);
                sampler.poke();
                Ok(format!("refresh every {secs}s"))
            }
            "gpu" => {
                let g: usize = arg.parse().map_err(|_| "gpu <index>")?;
                if g >= self.info.gpus.len() {
                    return Err(format!("no GPU {g}"));
                }
                self.sel_gpu = g;
                self.refresh_view();
                Ok(String::new())
            }
            "tab" | "view" => {
                self.tab =
                    Tab::ALL.into_iter().find(|t| t.label().eq_ignore_ascii_case(arg)).ok_or("views: overview charts details events")?;
                Ok(String::new())
            }
            "help" => {
                self.apply(Action::Help, sampler);
                Ok(String::new())
            }
            _ => Err(format!("unknown command {cmd:?}: theme sort filter clear hide show charts interval gpu view quit")),
        }
    }

    fn open_tune(&mut self) {
        if self.readonly {
            self.toast("read-only mode", true);
            return;
        }
        let i = self.sel_gpu;
        let Some(g) = self.gpu(i) else { return };
        let current = self.sample(i).and_then(|s| s.power_limit).or(g.power_limit_default);
        match current {
            Some(watts) if g.power_limit_max.is_some() => self.popup = Some(Popup::Tune { gpu: i, watts }),
            _ => self.toast("this GPU doesn't expose an adjustable power limit", true),
        }
    }

    fn on_tune_key(&mut self, k: KeyEvent, sampler: &Sampler) {
        let Some(Popup::Tune { gpu, watts }) = &mut self.popup else { return };
        let g = self.info.gpus.get(*gpu);
        let (lo, hi) = (g.and_then(|g| g.power_limit_min).unwrap_or(0.0), g.and_then(|g| g.power_limit_max).unwrap_or(f64::MAX));
        // Shift+arrow or H/L for big steps (Shift is how '+' is typed, so it can't mean "big" there).
        let big = matches!(k.code, KeyCode::Left | KeyCode::Right) && k.modifiers.contains(KeyModifiers::SHIFT);
        let step = if big || matches!(k.code, KeyCode::Char('H' | 'L')) { 25.0 } else { 5.0 };
        match k.code {
            KeyCode::Left | KeyCode::Char('h' | 'H' | '-') => *watts = (*watts - step).max(lo),
            KeyCode::Right | KeyCode::Char('l' | 'L' | '+' | '=') => *watts = (*watts + step).min(hi),
            KeyCode::Enter | KeyCode::Char('d') => {
                let cmd = if k.code == KeyCode::Enter {
                    crate::backend::Tune::PowerLimit(Some(*watts))
                } else {
                    crate::backend::Tune::PowerLimit(None)
                };
                sampler.ctl.commands.lock().unwrap().push((*gpu, cmd));
                sampler.poke();
                self.popup = None;
            }
            _ => self.popup = None,
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
                                    self.view.tagged.remove(&(*gpu, *pid));
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
            Popup::Help | Popup::Tune { .. } => self.popup = None,
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
            MouseEventKind::ScrollDown if self.tab == Tab::Events => self.scroll_events(3),
            MouseEventKind::ScrollUp if self.tab == Tab::Events => self.scroll_events(-3),
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
                    let idx = self.view.table.borrow().offset() + (m.row - self.hit.proc_body.y) as usize;
                    if let Some(key) = self.row(idx).map(ProcRow::key) {
                        self.view.sel_key = Some(key);
                        self.view.table.borrow_mut().select(Some(idx));
                    }
                }
            }
            _ => {}
        }
    }

    fn save_config(&self) {
        crate::config::update(|c| {
            c.theme = Some(self.theme().name.to_string());
            c.interval = Some(self.interval.as_secs_f64());
            c.sort = Some(self.view.spec.sort);
            c.sort_desc = Some(self.view.spec.desc);
            c.full_cmd = Some(self.view.full_cmd);
            c.chart_style = self.saved_chart_style;
            c.look = Some(self.look);
            c.hidden = Panel::ALL.into_iter().filter(|p| self.hidden.contains(p)).collect();
        });
    }
}

pub fn run(source: impl Source + 'static, info: SystemInfo, interval: Duration, opts: &UiOptions, cfg: Config) -> anyhow::Result<()> {
    let sampler = Sampler::spawn(source, interval);
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
    let record = !app.info.gpus.iter().all(|g| g.vendor == crate::model::Vendor::Demo);
    app.store = store::Store::open(&app.info, interval.as_secs_f64(), record);
    app.refresh_view();
    if app.chart_ctx().pixels() {
        app.toast(format!("charts: pixels ({}) · press G if they stay empty", app.gfx.protocol_name()), false);
    }
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = (|| -> anyhow::Result<()> {
        let mut dirty = true;
        let mut last_draw = Instant::now();
        loop {
            loop {
                match sampler.rx.try_recv() {
                    Ok(s) => {
                        app.ingest(s);
                        dirty = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => anyhow::bail!("the sampler thread stopped"),
                }
            }
            for r in std::mem::take(&mut *sampler.ctl.results.lock().unwrap()) {
                match r {
                    Ok(m) => app.toast(m, false),
                    Err(e) => app.toast(e, true),
                }
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
                theme::set_look(app.look, app.chart_ctx().bg);
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
