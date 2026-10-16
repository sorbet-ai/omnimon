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
}

/// "http://host:9465/json", "host:9465" or "host" -> ("host:port", "/json").
fn parse_addr(addr: &str) -> (String, String) {
    let a = addr.trim().trim_start_matches("http://");
    let (hp, path) = match a.split_once('/') {
        Some((hp, p)) if !p.is_empty() => (hp, format!("/{p}")),
        Some((hp, _)) => (hp, "/json".to_string()),
        None => (a, "/json".to_string()),
    };
    let hp = if hp.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) { hp.to_string() } else { format!("{hp}:9465") };
    (hp, path)
}

impl Remote {
    fn get(&self) -> anyhow::Result<JsonIn> {
        let sock = self.host_port.to_socket_addrs()?.next().ok_or_else(|| anyhow!("cannot resolve {}", self.host_port))?;
        let mut s = TcpStream::connect_timeout(&sock, Duration::from_secs(3))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        write!(s, "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", self.path, self.host_port)?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf)?;
        let text = String::from_utf8_lossy(&buf);
        let (head, body) = text.split_once("\r\n\r\n").ok_or_else(|| anyhow!("malformed HTTP response"))?;
        let status = head.lines().next().unwrap_or_default();
        if !status.contains(" 200") {
            bail!("{} answered {status}", self.host_port);
        }
        serde_json::from_str(body).context("unexpected JSON (is it a --serve endpoint of the same version?)")
    }
}

/// Connect and fetch the first snapshot.
pub fn connect(addr: &str) -> anyhow::Result<(Remote, SystemInfo, Snapshot)> {
    let (host_port, path) = parse_addr(addr);
    let r = Remote { host_port, path };
    let first = r.get().with_context(|| format!("connecting to {}", r.host_port))?;
    let mut info = first.system;
    if first.schema != crate::report::SCHEMA {
        info.warnings.push(format!("remote speaks JSON schema {}, this build {}", first.schema, crate::report::SCHEMA));
    }
    Ok((r, info, first.snapshot))
}

impl Source for Remote {
    fn collect(&mut self) -> Option<Snapshot> {
        match self.get() {
            Ok(j) => Some(j.snapshot),
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
        assert_eq!(parse_addr("gpu-box"), ("gpu-box:9465".into(), "/json".into()));
        assert_eq!(parse_addr("10.0.0.5:9000"), ("10.0.0.5:9000".into(), "/json".into()));
        assert_eq!(parse_addr("http://h:1/x/json"), ("h:1".into(), "/x/json".into()));
        assert_eq!(parse_addr("http://h:1/"), ("h:1".into(), "/json".into()));
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
