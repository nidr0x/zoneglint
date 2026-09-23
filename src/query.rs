use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use hickory_resolver::Resolver;
use hickory_resolver::config::{NameServerConfig, ResolveHosts, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::net::{DnsError, NetError};
use hickory_resolver::proto::op::ResponseCode;
use hickory_resolver::proto::rr::{Name, RecordType as HickoryRecordType};
use serde::Serialize;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use crate::servers::DnsServer;

const MAX_CONCURRENT_QUERIES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RecordType {
    A,
    Aaaa,
    Cname,
    Mx,
    Txt,
    Ns,
    Soa,
    Ptr,
    Srv,
    Caa,
}

impl RecordType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::Aaaa => "AAAA",
            Self::Cname => "CNAME",
            Self::Mx => "MX",
            Self::Txt => "TXT",
            Self::Ns => "NS",
            Self::Soa => "SOA",
            Self::Ptr => "PTR",
            Self::Srv => "SRV",
            Self::Caa => "CAA",
        }
    }

    fn hickory(self) -> HickoryRecordType {
        match self {
            Self::A => HickoryRecordType::A,
            Self::Aaaa => HickoryRecordType::AAAA,
            Self::Cname => HickoryRecordType::CNAME,
            Self::Mx => HickoryRecordType::MX,
            Self::Txt => HickoryRecordType::TXT,
            Self::Ns => HickoryRecordType::NS,
            Self::Soa => HickoryRecordType::SOA,
            Self::Ptr => HickoryRecordType::PTR,
            Self::Srv => HickoryRecordType::SRV,
            Self::Caa => HickoryRecordType::CAA,
        }
    }
}

impl TryFrom<&str> for RecordType {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "A" => Ok(Self::A),
            "AAAA" => Ok(Self::Aaaa),
            "CNAME" => Ok(Self::Cname),
            "MX" => Ok(Self::Mx),
            "TXT" => Ok(Self::Txt),
            "NS" => Ok(Self::Ns),
            "SOA" => Ok(Self::Soa),
            "PTR" => Ok(Self::Ptr),
            "SRV" => Ok(Self::Srv),
            "CAA" => Ok(Self::Caa),
            other => anyhow::bail!(
                "unsupported record type {other:?}; choose A, AAAA, CNAME, MX, TXT, NS, SOA, PTR, SRV, or CAA"
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServerResult {
    pub catalog_index: usize,
    pub server: DnsServer,
    pub records: Vec<String>,
    pub response_code: Option<String>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum QueryEvent {
    Started { total: usize },
    Completed(ServerResult),
}

#[derive(Clone, Debug, Serialize)]
pub struct PropagationSummary {
    pub propagated: usize,
    pub total: usize,
    pub percentage: f64,
    pub consensus: Option<String>,
}

pub async fn run_queries(
    domain: Name,
    record_type: RecordType,
    servers: Vec<DnsServer>,
    timeout: Duration,
    events: mpsc::Sender<QueryEvent>,
) -> Result<()> {
    let total = servers.len();
    if events.send(QueryEvent::Started { total }).await.is_err() {
        return Ok(());
    }

    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_QUERIES));
    let mut tasks = JoinSet::new();
    for (catalog_index, server) in servers.into_iter().enumerate() {
        let domain = domain.clone();
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            let permit = semaphore
                .acquire_owned()
                .await
                .context("DNS concurrency limiter closed")?;
            let result = query_one(catalog_index, server, domain, record_type, timeout).await;
            drop(permit);
            Ok::<_, anyhow::Error>(result)
        });
    }

    while let Some(joined) = tasks.join_next().await {
        let result = joined.context("DNS worker task panicked")??;
        if events.send(QueryEvent::Completed(result)).await.is_err() {
            tasks.abort_all();
            break;
        }
    }
    Ok(())
}

async fn query_one(
    catalog_index: usize,
    server: DnsServer,
    domain: Name,
    record_type: RecordType,
    timeout: Duration,
) -> ServerResult {
    let mut options = ResolverOpts::default();
    options.timeout = timeout;
    options.attempts = 1;
    options.use_hosts_file = ResolveHosts::Never;
    let config = ResolverConfig::from_name_servers(vec![NameServerConfig::udp(server.ip)]);
    let resolver = match Resolver::builder_with_config(config, TokioRuntimeProvider::default())
        .with_options(options)
        .build()
    {
        Ok(resolver) => resolver,
        Err(error) => {
            return ServerResult {
                catalog_index,
                server,
                records: Vec::new(),
                response_code: None,
                latency_ms: 0,
                error: Some(error.to_string()),
            };
        }
    };

    let started = Instant::now();
    let lookup =
        tokio::time::timeout(timeout, resolver.lookup(domain, record_type.hickory())).await;
    let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let (records, response_code, error) = match lookup {
        Ok(Ok(lookup)) => {
            let mut values: Vec<String> = lookup
                .answers()
                .iter()
                .filter(|record| record.record_type() == record_type.hickory())
                .map(|record| normalize_record(record.data.to_string(), record_type))
                .collect();
            values.sort();
            values.dedup();
            (values, Some("NOERROR".to_owned()), None)
        }
        Ok(Err(NetError::Dns(DnsError::NoRecordsFound(no_records)))) => (
            Vec::new(),
            Some(format_response_code(no_records.response_code)),
            None,
        ),
        Ok(Err(NetError::Dns(DnsError::ResponseCode(code)))) => {
            (Vec::new(), Some(format_response_code(code)), None)
        }
        Ok(Err(error)) => (Vec::new(), None, Some(error.to_string())),
        Err(_) => (
            Vec::new(),
            None,
            Some(format!(
                "timeout after {}",
                humantime::format_duration(timeout)
            )),
        ),
    };

    ServerResult {
        catalog_index,
        server,
        records,
        response_code,
        latency_ms,
        error,
    }
}

fn format_response_code(code: ResponseCode) -> String {
    format!("{code:?}").to_ascii_uppercase()
}

fn normalize_record(value: String, record_type: RecordType) -> String {
    if matches!(
        record_type,
        RecordType::Cname
            | RecordType::Mx
            | RecordType::Ns
            | RecordType::Soa
            | RecordType::Ptr
            | RecordType::Srv
    ) {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn outcome_key(result: &ServerResult) -> Option<(String, Vec<String>)> {
    result
        .response_code
        .as_ref()
        .map(|code| (code.clone(), result.records.clone()))
}

pub fn outcome_label(result: &ServerResult) -> Option<String> {
    let (code, records) = outcome_key(result)?;
    Some(format_outcome(&code, &records))
}

fn format_outcome(code: &str, records: &[String]) -> String {
    if records.is_empty() {
        format!("{code} (no records)")
    } else {
        format!("{code}: {}", records.join(", "))
    }
}

pub fn summarize(results: &[ServerResult]) -> PropagationSummary {
    let mut groups = HashMap::<(String, Vec<String>), usize>::new();
    for result in results {
        if let Some(key) = outcome_key(result) {
            *groups.entry(key).or_default() += 1;
        }
    }
    let mut largest = 0;
    let mut leader = None;
    let mut tied = false;
    for (key, count) in &groups {
        if *count > largest {
            largest = *count;
            leader = Some(key);
            tied = false;
        } else if *count == largest {
            tied = true;
        }
    }
    let consensus = if tied {
        None
    } else {
        leader.map(|(code, records)| format_outcome(code, records))
    };
    let total = results.len();
    PropagationSummary {
        propagated: largest,
        total,
        percentage: if total == 0 {
            0.0
        } else {
            largest as f64 * 100.0 / total as f64
        },
        consensus,
    }
}
