use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, ValueHint};
use hickory_resolver::proto::rr::Name;

#[derive(Debug, Parser)]
#[command(
    name = "zoneglint",
    version,
    about = "Compare DNS answers across public resolvers",
    after_help = "Built-in provider IDs: google, cloudflare, quad9, opendns, level3, verisign, comodo, dnswatch, alidns, 114dns."
)]
pub struct Args {
    #[arg(value_name = "DOMAIN", value_hint = ValueHint::Hostname)]
    pub domain: String,

    #[arg(short = 't', long = "type", default_value = "A", value_name = "TYPE")]
    pub record_type: String,

    #[arg(long, conflicts_with = "simple")]
    pub json: bool,

    #[arg(long)]
    pub simple: bool,

    #[arg(long, value_parser = parse_timeout, default_value = "5s", value_name = "DURATION")]
    pub timeout: Duration,

    #[arg(
        long,
        value_name = "IDS",
        help = "Comma-separated built-in provider IDs"
    )]
    pub servers: Option<String>,

    #[arg(long, help = "Fetch additional resolvers from public-dns.info")]
    pub fetch: bool,

    #[arg(long, value_parser = parse_fetch_count, default_value_t = 30)]
    pub fetch_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputMode {
    Json,
    Simple,
    Interactive,
}

pub fn output_mode(args: &Args, stdout_is_terminal: bool) -> OutputMode {
    if args.json {
        OutputMode::Json
    } else if args.simple || !stdout_is_terminal {
        OutputMode::Simple
    } else {
        OutputMode::Interactive
    }
}

pub fn validate_domain(value: &str) -> Result<Name> {
    Name::from_ascii(value).with_context(|| format!("invalid DNS name: {value:?}"))
}

fn parse_timeout(value: &str) -> std::result::Result<Duration, String> {
    let duration = humantime::parse_duration(value).map_err(|error| error.to_string())?;
    if duration.is_zero() {
        return Err("timeout must be greater than zero".to_owned());
    }
    Ok(duration)
}

fn parse_fetch_count(value: &str) -> std::result::Result<usize, String> {
    let count = value
        .parse::<usize>()
        .map_err(|error| format!("invalid fetch count: {error}"))?;
    if !(1..=500).contains(&count) {
        return Err("fetch count must be between 1 and 500".to_owned());
    }
    Ok(count)
}
