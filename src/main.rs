mod cli;
mod output;
mod query;
mod servers;
mod tui;

use std::io::IsTerminal;

use anyhow::{Context, Result};
use clap::Parser;
use time::OffsetDateTime;
use tokio::sync::mpsc;

use crate::cli::{Args, OutputMode};
use crate::query::RecordType;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let domain = cli::validate_domain(&args.domain)?;
    let record_type = RecordType::try_from(args.record_type.as_str())?;
    let mut selected =
        servers::filter_servers(&servers::built_in_servers(), args.servers.as_deref())?;

    if args.fetch {
        match servers::fetch_public_dns(args.fetch_count).await {
            Ok(discovered) => {
                selected = servers::merge_servers(selected, discovered, args.fetch_count);
            }
            Err(error) => eprintln!("warning: public resolver discovery failed: {error:#}"),
        }
    }

    let mode = cli::output_mode(&args, std::io::stdout().is_terminal());
    let (sender, mut receiver) = mpsc::channel(32);
    let timeout = args.timeout;
    let query_task = tokio::spawn(async move {
        query::run_queries(domain, record_type, selected, timeout, sender).await
    });

    let results = match mode {
        OutputMode::Interactive => tui::run(&args.domain, record_type, &mut receiver).await,
        OutputMode::Json | OutputMode::Simple => Ok(output::collect_results(&mut receiver).await),
    };
    receiver.close();
    query_task.await.context("DNS query task panicked")??;
    let results = results?;

    let summary = query::summarize(&results);
    match mode {
        OutputMode::Json => {
            println!(
                "{}",
                output::render_json(
                    &args.domain,
                    record_type,
                    OffsetDateTime::now_utc(),
                    &results,
                    &summary
                )?
            );
        }
        OutputMode::Simple => {
            print!(
                "{}",
                output::render_table(&args.domain, record_type, &results, &summary)
            );
        }
        OutputMode::Interactive => {}
    }
    Ok(())
}
