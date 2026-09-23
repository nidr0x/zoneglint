use std::net::IpAddr;

use anyhow::{Context, Result};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::query::{PropagationSummary, RecordType, ServerResult, outcome_label};

pub async fn collect_results(
    events: &mut tokio::sync::mpsc::Receiver<crate::query::QueryEvent>,
) -> Vec<ServerResult> {
    let mut results = Vec::new();
    while let Some(event) = events.recv().await {
        if let crate::query::QueryEvent::Completed(result) = event {
            results.push(result);
        }
    }
    results.sort_by_key(|result| result.catalog_index);
    results
}

pub fn render_table(
    domain: &str,
    record_type: RecordType,
    results: &[ServerResult],
    summary: &PropagationSummary,
) -> String {
    let mut output = format!("Zoneglint · {} {}\n", domain, record_type.as_str());
    output.push_str("SERVER                         IP               LOCATION       RESULT                                      LATENCY    STATUS\n");
    output.push_str("─────────────────────────────  ───────────────  ─────────────  ─────────────────────────────────────────  ─────────  ─────────────\n");
    for result in results {
        let location = result.server.location.as_deref().unwrap_or("—");
        let detail = if let Some(error) = &result.error {
            format!("ERROR: {error}")
        } else if let Some(code) = &result.response_code {
            if result.records.is_empty() {
                format!("{code} (no records)")
            } else {
                format!("{code}: {}", result.records.join(", "))
            }
        } else {
            "no response".to_owned()
        };
        let status = match (
            result.error.is_some(),
            outcome_label(result),
            summary.consensus.as_deref(),
        ) {
            (true, _, _) => "failed",
            (false, Some(label), Some(consensus)) if label == consensus => "agree",
            (false, Some(_), _) => "split/unknown",
            _ => "unknown",
        };
        output.push_str(&format!(
            "{:<29}  {:<15}  {:<12}  {:<40}  {:>6} ms  {}\n",
            result.server.name, result.server.ip, location, detail, result.latency_ms, status,
        ));
    }
    let consensus = summary.consensus.as_deref().unwrap_or("split/unknown");
    output.push_str(&format!(
        "\nPropagation: {}/{} ({:.1}%) · {}\n",
        summary.propagated, summary.total, summary.percentage, consensus
    ));
    output
}

#[derive(Serialize)]
struct JsonDocument<'a> {
    domain: &'a str,
    record_type: &'static str,
    timestamp: String,
    results: Vec<JsonResult<'a>>,
    propagation: &'a PropagationSummary,
}

#[derive(Serialize)]
struct JsonResult<'a> {
    server: &'a str,
    ip: IpAddr,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<&'a str>,
    records: &'a [String],
    latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

pub fn render_json(
    domain: &str,
    record_type: RecordType,
    timestamp: OffsetDateTime,
    results: &[ServerResult],
    summary: &PropagationSummary,
) -> Result<String> {
    let timestamp = timestamp
        .to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .context("could not format UTC timestamp")?;
    let document = JsonDocument {
        domain,
        record_type: record_type.as_str(),
        timestamp,
        results: results
            .iter()
            .map(|result| JsonResult {
                server: &result.server.name,
                ip: result.server.ip,
                location: result.server.location.as_deref(),
                records: &result.records,
                latency_ms: result.latency_ms,
                response_code: result.response_code.as_deref(),
                error: result.error.as_deref(),
            })
            .collect(),
        propagation: summary,
    };
    serde_json::to_string_pretty(&document).context("could not serialize DNS results")
}
