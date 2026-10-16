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

/// Decimal bytes per second ("1.21 TB/s"), the way bandwidth is specified.
pub fn bandwidth(bps: f64) -> String {
    match bps {
        b if b >= 1e12 => format!("{:.2} TB/s", b / 1e12),
        b if b >= 1e9 => format!("{:.0} GB/s", b / 1e9),
        b => format!("{:.0} MB/s", b / 1e6),
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

/// Percent with no decimals, or "N/A".
pub fn pct(v: Option<f32>) -> String {
    v.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "N/A".into())
}

/// Truncate to `n` characters, ending with "…" when cut.
pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else if n == 0 {
        String::new()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(bytes(1023), "1023B");
        assert_eq!(bytes(1024), "1.00KiB");
        assert_eq!(bytes(24 << 30), "24.0GiB");
        assert_eq!(bytes_short(1536 << 20), "1.5G");
        assert_eq!(bytes_short(812 << 20), "812M");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(59), "0:59");
        assert_eq!(duration(3600), "1:00:00");
        assert_eq!(duration(90000), "1d01h");
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("İstanbul", 3), "İs…");
    }
}
