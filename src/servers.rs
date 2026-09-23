use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug)]
pub struct DnsServer {
    pub id: String,
    pub name: String,
    pub ip: IpAddr,
    pub location: Option<String>,
}

pub fn built_in_servers() -> Vec<DnsServer> {
    let entries = [
        ("google", "Google Public DNS", "8.8.8.8", "Global"),
        ("google", "Google Public DNS", "8.8.4.4", "Global"),
        ("cloudflare", "Cloudflare", "1.1.1.1", "Global"),
        ("cloudflare", "Cloudflare", "1.0.0.1", "Global"),
        ("quad9", "Quad9", "9.9.9.9", "Global"),
        ("quad9", "Quad9", "149.112.112.112", "Global"),
        ("opendns", "OpenDNS", "208.67.222.222", "Global"),
        ("opendns", "OpenDNS", "208.67.220.220", "Global"),
        ("level3", "Lumen / Level3", "205.171.202.25", "US"),
        (
            "verisign",
            "Verisign Public DNS (legacy)",
            "64.6.64.6",
            "Global",
        ),
        ("comodo", "Comodo Secure DNS", "8.26.56.26", "Global"),
        ("dnswatch", "DNS.WATCH", "84.200.69.80", "Global"),
        ("alidns", "AliDNS", "223.5.5.5", "China"),
        ("114dns", "114DNS", "114.114.114.114", "China"),
    ];

    entries
        .into_iter()
        .map(|(id, name, address, location)| DnsServer {
            id: id.to_owned(),
            name: name.to_owned(),
            ip: address.parse().expect("catalog contains valid IP literals"),
            location: Some(location.to_owned()),
        })
        .collect()
}

pub fn filter_servers(all: &[DnsServer], selection: Option<&str>) -> Result<Vec<DnsServer>> {
    let Some(selection) = selection else {
        return Ok(all.to_vec());
    };

    let mut requested = HashSet::new();
    for raw in selection.split(',') {
        let id = raw.trim().to_ascii_lowercase();
        if id.is_empty() {
            bail!("provider selection contains an empty ID");
        }
        if !all.iter().any(|server| server.id == id) {
            bail!("unknown provider ID {id:?}; use --help to list available IDs");
        }
        requested.insert(id);
    }

    let selected: Vec<_> = all
        .iter()
        .filter(|server| requested.contains(&server.id))
        .cloned()
        .collect();
    if selected.is_empty() {
        bail!("provider selection matched no resolvers");
    }
    Ok(selected)
}

pub async fn fetch_public_dns(max_count: usize) -> Result<Vec<DnsServer>> {
    let max_count = max_count.min(500);
    if max_count == 0 {
        return Ok(Vec::new());
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("could not configure resolver discovery client")?;
    let response = client
        .get("https://public-dns.info/nameservers.csv")
        .send()
        .await
        .context("could not download public-dns.info resolver list")?
        .error_for_status()
        .context("public-dns.info returned an unsuccessful HTTP status")?;
    let body = response
        .text()
        .await
        .context("could not read public-dns.info response")?;

    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(body.as_bytes());
    let headers = reader
        .headers()
        .context("CSV has no readable header row")?
        .clone();
    let index = |required: &str| -> Result<usize> {
        headers
            .iter()
            .position(|header| {
                header
                    .trim_start_matches('\u{feff}')
                    .trim()
                    .eq_ignore_ascii_case(required)
            })
            .with_context(|| format!("CSV is missing required column {required:?}"))
    };
    let ip_index = index("ip_address")?;
    let country_index = index("country_code")?;
    let reliability_index = index("reliability")?;
    let optional_index = |name: &str| {
        headers.iter().position(|header| {
            header
                .trim_start_matches('\u{feff}')
                .trim()
                .eq_ignore_ascii_case(name)
        })
    };
    let name_index = optional_index("name");
    let org_index = optional_index("as_org");
    let city_index = optional_index("city");

    let mut accepted = Vec::new();
    let mut seen_ips = HashSet::new();
    let mut country_counts = HashMap::<String, usize>::new();
    for row in reader.records() {
        let Ok(row) = row else { continue };
        let Some(address) = row
            .get(ip_index)
            .and_then(|value| value.trim().parse::<IpAddr>().ok())
        else {
            continue;
        };
        if !address.is_ipv4() || seen_ips.contains(&address) {
            continue;
        }
        let country = row
            .get(country_index)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        let reliability = row
            .get(reliability_index)
            .and_then(|value| value.trim().parse::<f64>().ok());
        if country.is_empty() || !reliability.is_some_and(|value| value >= 0.95) {
            continue;
        }
        let count = country_counts.entry(country.clone()).or_default();
        if *count >= 2 {
            continue;
        }
        seen_ips.insert(address);
        *count += 1;

        let field = |at: Option<usize>| {
            at.and_then(|position| row.get(position))
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let name = field(name_index)
            .or_else(|| field(org_index))
            .unwrap_or(&country)
            .to_owned();
        let location = field(city_index)
            .map(|city| format!("{city}, {country}"))
            .or_else(|| Some(country.clone()));
        accepted.push(DnsServer {
            id: "public-dns-info".to_owned(),
            name,
            ip: address,
            location,
        });
        if accepted.len() == max_count {
            break;
        }
    }
    Ok(accepted)
}

pub fn merge_servers(
    base: Vec<DnsServer>,
    fetched: Vec<DnsServer>,
    max_count: usize,
) -> Vec<DnsServer> {
    let mut merged = base;
    let mut seen: HashSet<IpAddr> = merged.iter().map(|server| server.ip).collect();
    let mut appended = 0;
    for server in fetched {
        if appended >= max_count.min(500) {
            break;
        }
        if seen.insert(server.ip) {
            merged.push(server);
            appended += 1;
        }
    }
    merged
}
