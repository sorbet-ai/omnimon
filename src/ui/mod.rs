use std::time::Duration;

use crate::Cli;
use crate::collector::Collector;
use crate::model::SystemInfo;

pub fn run(_c: Collector, _i: SystemInfo, _iv: Duration, _cli: &Cli) -> anyhow::Result<()> {
    Ok(())
}
