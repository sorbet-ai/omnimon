//! Number formatting shared by the TUI and the text output.

pub fn bytes(b: u64) -> String {
    const U: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b}B")
    } else if v >= 100.0 {
        format!("{v:.0}{}", U[i])
    } else if v >= 10.0 {
        format!("{v:.1}{}", U[i])
    } else {
        format!("{v:.2}{}", U[i])
    }
}

/// Compact bytes for tight columns: "23.5G", "812M".
pub fn bytes_short(b: u64) -> String {
    const U: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b}B")
    } else if v >= 100.0 || i <= 2 {
        format!("{v:.0}{}", U[i])
    } else {
        format!("{v:.1}{}", U[i])
    }
}

pub fn rate(bps: u64) -> String {
    format!("{}/s", bytes_short(bps))
}

pub fn duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, (secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    if d > 0 {
        format!("{d}d{h:02}h")
    } else if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn opt<T: std::fmt::Display>(v: Option<T>, unit: &str) -> String {
    match v {
        Some(v) => format!("{v}{unit}"),
        None => "N/A".into(),
    }
}
