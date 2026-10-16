//! `--connect ADDR`: watch another machine running `--serve`. Fetches the
//! same JSON document `/json` serves (plain HTTP, no dependencies) and feeds
//! it to the normal UI. Read-only by nature: signals and tuning would act on
//! this machine, not the remote one.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use serde::Deserialize;

use crate::backend::Tune;
use crate::collector::Source;
use crate::model::{Snapshot, SystemInfo};

#[derive(Deserialize)]
struct JsonIn {
    schema: u32,
    system: SystemInfo,
    snapshot: Snapshot,
}

pub struct Remote {
    host_port: String,
    path: String,
    /// The snapshot fetched while connecting, handed to the UI first.
    pending: Option<Snapshot>,
    /// Timestamp of the last snapshot returned (the server may sample slower
    /// than we poll; repeats would draw flat steps in the charts).
    last: Option<f64>,
}

/// "http://host:9465/json", "host:9465" or "host" -> ("host:port", "/json").
fn parse_addr(addr: &str) -> anyhow::Result<(String, String)> {
    let a = addr.trim();
    let a = match a.split_once("://") {
        Some(("http", rest)) => rest,
        Some((scheme, _)) => bail!("{scheme}:// is not supported (plain http only; tunnel it, e.g. ssh -L 9465:localhost:9465)"),
        None => a,
    };
    let (hp, path) = match a.split_once('/') {
        Some((hp, p)) if !p.is_empty() => (hp, format!("/{p}")),
        Some((hp, _)) => (hp, "/json".to_string()),
        None => (a, "/json".to_string()),
    };
    if hp.is_empty() {
        bail!("no host in {addr:?}");
    }
    let hp = if hp.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) { hp.to_string() } else { format!("{hp}:9465") };
    Ok((hp, path))
}

/// The body of a complete `200` response (honoring Content-Length, so a cut
/// connection is an error rather than half a JSON document).
fn body_of(buf: &[u8]) -> anyhow::Result<&[u8]> {
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| anyhow!("malformed HTTP response"))?;
    let head = String::from_utf8_lossy(&buf[..split]);
    let body = &buf[split + 4..];
    let status = head.lines().next().unwrap_or_default();
    if !status.contains(" 200") {
        bail!("server answered {status}");
    }
    let len = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok()).flatten()
    });
    match len {
        Some(n) if body.len() < n => bail!("response cut short ({} of {n} bytes)", body.len()),
        Some(n) => Ok(&body[..n]),
        None => Ok(body),
    }
}

impl Remote {
    fn get(&self) -> anyhow::Result<JsonIn> {
        let sock = self.host_port.to_socket_addrs()?.next().ok_or_else(|| anyhow!("cannot resolve {}", self.host_port))?;
        let mut s = TcpStream::connect_timeout(&sock, Duration::from_secs(3))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        write!(s, "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", self.path, self.host_port)?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf)?;
        serde_json::from_slice(body_of(&buf)?).context("unexpected JSON (is it a --serve endpoint?)")
    }
}

/// Connect and fetch the first snapshot.
pub fn connect(addr: &str) -> anyhow::Result<(Remote, SystemInfo, Snapshot)> {
    let (host_port, path) = parse_addr(addr)?;
    let mut r = Remote { host_port, path, pending: None, last: None };
    let first = r.get().with_context(|| format!("connecting to {}", r.host_port))?;
    let mut info = first.system;
    if first.schema > crate::report::SCHEMA {
        bail!(
            "{} runs a newer version (JSON schema {}, this build reads {}); update this one",
            r.host_port,
            first.schema,
            crate::report::SCHEMA
        );
    }
    if first.schema < crate::report::SCHEMA {
        info.warnings.push(format!("remote speaks JSON schema {}, this build {}", first.schema, crate::report::SCHEMA));
    }
    r.pending = Some(first.snapshot.clone());
    Ok((r, info, first.snapshot))
}

impl Source for Remote {
    fn collect(&mut self) -> Option<Snapshot> {
        let snap = match self.pending.take() {
            Some(s) => Ok(s),
            None => self.get().map(|j| j.snapshot),
        };
        match snap {
            Ok(s) if self.last == Some(s.timestamp) => None,
            Ok(s) => {
                self.last = Some(s.timestamp);
                Some(s)
            }
            Err(e) => {
                log::warn!("remote: {e:#}");
                None
            }
        }
    }

    fn apply(&mut self, _gpu: usize, _cmd: &Tune) -> Result<String, String> {
        Err("not available on a remote connection".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let p = |a| parse_addr(a).ok();
        assert_eq!(p("gpu-box"), Some(("gpu-box:9465".into(), "/json".into())));
        assert_eq!(p("10.0.0.5:9000"), Some(("10.0.0.5:9000".into(), "/json".into())));
        assert_eq!(p("http://h:1/x/json"), Some(("h:1".into(), "/x/json".into())));
        assert_eq!(p("http://h:1/"), Some(("h:1".into(), "/json".into())));
        assert_eq!(p("https://h"), None);
        assert_eq!(p("http://"), None);
    }

    #[test]
    fn http_bodies() {
        assert_eq!(body_of(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap(), b"{}");
        assert_eq!(body_of(b"HTTP/1.1 200 OK\r\n\r\n{}").unwrap(), b"{}");
        assert!(body_of(b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\n{}").is_err(), "truncated");
        assert!(body_of(b"HTTP/1.1 404 Not Found\r\n\r\n").is_err());
    }

    /// Serve one snapshot over the real exporter code path and read it back.
    #[test]
    fn round_trip_through_json() {
        let info = SystemInfo { host: crate::model::HostInfo { hostname: "box".into(), ..Default::default() }, ..Default::default() };
        let snap = Snapshot { timestamp: 42.0, ..Default::default() };
        let text = serde_json::to_string(&crate::report::JsonOut::new(&info, &snap)).unwrap();
        let back: JsonIn = serde_json::from_str(&text).unwrap();
        assert_eq!(back.system.host.hostname, "box");
        assert_eq!(back.snapshot.timestamp, 42.0);
    }
}
