//! Key bindings: every key maps to an [`Action`]; the config file can rebind
//! any action (`"keys": {"kill": ["K", "f9"]}`).

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Quit,
    Help,
    NextTab,
    PrevTab,
    Overview,
    Charts,
    Details,
    Events,
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    PrevGpu,
    NextGpu,
    SortNext,
    SortPrev,
    SortReverse,
    Filter,
    Command,
    GpuOnly,
    FullCommand,
    Tag,
    ProcessDetails,
    Kill,
    Theme,
    ChartStyle,
    Pause,
    Slower,
    Faster,
}

impl Action {
    /// For the help screen.
    pub fn describe(self) -> &'static str {
        match self {
            Action::Quit => "quit",
            Action::Help => "this help",
            Action::NextTab | Action::PrevTab => "next / previous view",
            Action::Overview => "overview",
            Action::Charts => "charts",
            Action::Details => "details",
            Action::Events => "events",
            Action::Up | Action::Down => "move in the list",
            Action::PageUp | Action::PageDown => "page up / down",
            Action::Top | Action::Bottom => "first / last row",
            Action::PrevGpu | Action::NextGpu => "select GPU",
            Action::SortNext | Action::SortPrev => "next / previous sort column",
            Action::SortReverse => "reverse sort",
            Action::Filter => "filter: text, gpu:0 user:x pid:123 type:c spill",
            Action::Command => "command: theme, sort, hide/show, charts, interval…",
            Action::GpuOnly => "only processes on the selected GPU",
            Action::FullCommand => "full command lines",
            Action::Tag => "tag process (multi-kill)",
            Action::ProcessDetails => "process details: tree, environment",
            Action::Kill => "signal tagged / selected processes",
            Action::Theme => "cycle theme",
            Action::ChartStyle => "chart style: pixels · blocks · braille",
            Action::Pause => "pause sampling",
            Action::Slower | Action::Faster => "slower / faster refresh",
        }
    }
}

/// Default bindings, in help-screen order.
const DEFAULTS: &[(Action, &[&str])] = &[
    (Action::Help, &["?", "f1"]),
    (Action::Command, &[":"]),
    (Action::Filter, &["/"]),
    (Action::Overview, &["1"]),
    (Action::Charts, &["2"]),
    (Action::Details, &["3"]),
    (Action::Events, &["4"]),
    (Action::NextTab, &["tab"]),
    (Action::PrevTab, &["shift-tab"]),
    (Action::Up, &["up", "k"]),
    (Action::Down, &["down", "j"]),
    (Action::PageUp, &["pgup"]),
    (Action::PageDown, &["pgdn"]),
    (Action::Top, &["home"]),
    (Action::Bottom, &["end"]),
    (Action::PrevGpu, &["left", "["]),
    (Action::NextGpu, &["right", "]"]),
    (Action::SortNext, &["s", "f6"]),
    (Action::SortPrev, &["S"]),
    (Action::SortReverse, &["r"]),
    (Action::GpuOnly, &["g"]),
    (Action::FullCommand, &["c"]),
    (Action::Tag, &["space"]),
    (Action::ProcessDetails, &["enter"]),
    (Action::Kill, &["K", "del", "f9"]),
    (Action::Theme, &["t"]),
    (Action::ChartStyle, &["G"]),
    (Action::Pause, &["p"]),
    (Action::Slower, &["+", "="]),
    (Action::Faster, &["-", "_"]),
    (Action::Quit, &["q", "ctrl-c"]),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    code: KeyCode,
    ctrl: bool,
}

/// "q", "K", "ctrl-c", "f9", "enter", "space", "shift-tab", "pgup", ...
fn parse_key(s: &str) -> Option<Key> {
    let (ctrl, name) = match s.strip_prefix("ctrl-") {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let code = match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "tab" => KeyCode::Tab,
        "shift-tab" | "backtab" => KeyCode::BackTab,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pgup" | "pageup" => KeyCode::PageUp,
        "pgdn" | "pagedown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "del" | "delete" => KeyCode::Delete,
        "backspace" => KeyCode::Backspace,
        f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => KeyCode::F(f[1..].parse().ok()?),
        _ => {
            let mut chars = name.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    };
    Some(Key { code, ctrl })
}

pub struct Keymap {
    map: HashMap<Key, Action>,
    /// First key of each action, for the help screen and footer.
    labels: HashMap<Action, String>,
}

impl Keymap {
    pub fn new(overrides: &HashMap<Action, Vec<String>>) -> Self {
        let mut map = HashMap::new();
        let mut labels = HashMap::new();
        for &(action, defaults) in DEFAULTS {
            let keys: Vec<String> = match overrides.get(&action) {
                Some(v) => v.clone(),
                None => defaults.iter().map(|s| s.to_string()).collect(),
            };
            for k in &keys {
                match parse_key(k) {
                    Some(key) => {
                        map.insert(key, action);
                    }
                    None => log::warn!("unknown key {k:?} for {action:?}"),
                }
            }
            if let Some(first) = keys.first() {
                labels.insert(action, first.clone());
            }
        }
        Keymap { map, labels }
    }

    pub fn action(&self, k: &KeyEvent) -> Option<Action> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.map.get(&Key { code: k.code, ctrl }).copied()
    }

    pub fn label(&self, a: Action) -> &str {
        self.labels.get(&a).map(String::as_str).unwrap_or("")
    }

    /// (keys, description) rows for the help screen; paired actions share a row.
    pub fn help_rows(&self) -> Vec<(String, &'static str)> {
        let mut rows: Vec<(String, &'static str)> = Vec::new();
        for &(a, _) in DEFAULTS {
            let mut keys: Vec<String> = self.map.iter().filter(|(_, v)| **v == a).map(|(k, _)| key_name(k)).collect();
            keys.sort();
            if keys.is_empty() {
                continue;
            }
            let desc = a.describe();
            match rows.iter_mut().find(|(_, d)| *d == desc) {
                Some(row) => row.0 = format!("{} / {}", row.0, keys.join(" ")),
                None => rows.push((keys.join(" "), desc)),
            }
        }
        rows
    }
}

fn key_name(k: &Key) -> String {
    let s = match k.code {
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "shift-tab".into(),
        KeyCode::Up => "↑".into(),
        KeyCode::Down => "↓".into(),
        KeyCode::Left => "←".into(),
        KeyCode::Right => "→".into(),
        KeyCode::PageUp => "pgup".into(),
        KeyCode::PageDown => "pgdn".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::Delete => "del".into(),
        KeyCode::F(n) => format!("f{n}"),
        _ => "?".into(),
    };
    if k.ctrl { format!("ctrl-{s}") } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn defaults_and_overrides() {
        let km = Keymap::new(&HashMap::new());
        assert_eq!(km.action(&ev(KeyCode::Char('K'))), Some(Action::Kill));
        assert_eq!(km.action(&ev(KeyCode::F(9))), Some(Action::Kill));
        assert_eq!(km.action(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(Action::Quit));
        assert_eq!(km.action(&ev(KeyCode::Char('c'))), Some(Action::FullCommand));

        let mut o = HashMap::new();
        o.insert(Action::Kill, vec!["x".to_string()]);
        let km = Keymap::new(&o);
        assert_eq!(km.action(&ev(KeyCode::Char('x'))), Some(Action::Kill));
        assert_eq!(km.action(&ev(KeyCode::Char('K'))), None);
        assert_eq!(km.label(Action::Kill), "x");
    }

    #[test]
    fn key_parsing() {
        assert_eq!(parse_key("f12").unwrap().code, KeyCode::F(12));
        assert_eq!(parse_key("space").unwrap().code, KeyCode::Char(' '));
        assert!(parse_key("ctrl-c").unwrap().ctrl);
        assert!(parse_key("nope").is_none());
    }
}
