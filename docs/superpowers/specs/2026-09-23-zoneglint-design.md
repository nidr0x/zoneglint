# Zoneglint Design Specification

**Status:** Conversational scope and architecture approved; awaiting written-spec review.

## Goal

Build `zoneglint`, a standalone Rust CLI that compares DNS answers from multiple public resolvers so a user can inspect propagation from one command.

## Product scope

The first release is a single executable with the core behavior of the reference `digga` project, implemented independently in Rust. It accepts one domain, queries selected resolvers concurrently, reports answer differences and per-resolver latency, and summarizes agreement. It supports interactive terminal progress, plain table output, and JSON output.

The project name is **Zoneglint** and the executable is `zoneglint`.

### Record types

The supported query types are `A`, `AAAA`, `CNAME`, `MX`, `TXT`, `NS`, `SOA`, `PTR`, `SRV`, and `CAA`. The default is `A`. An unsupported type is rejected before any network request.

### Command-line interface

The command shape is:

```text
zoneglint <domain> [options]
```

The initial options are:

| Option | Behavior |
| --- | --- |
| `--type`, `-t` | Select one supported record type; defaults to `A`. |
| `--json` | Write one indented JSON document to stdout. |
| `--simple` | Write a plain table without terminal animation. |
| `--timeout` | Set a positive timeout per resolver; defaults to `5s`. |
| `--servers` | Comma-separated, case-insensitive provider IDs from the built-in resolver catalog; IDs are listed in `--help` and the README. |
| `--fetch` | Opt in to fetching additional resolver endpoints from public-dns.info. |
| `--fetch-count` | Maximum number of additional endpoints to use; defaults to `30`, must be positive, and is capped at `500`. |

`--json` and `--simple` cannot be used together. Unknown provider IDs and a filter that selects no built-in resolver are usage errors and fail before querying. When `--fetch` is enabled, fetched endpoints are appended to the selected built-in endpoints and duplicates are removed by IP address. If discovery fails, Zoneglint writes a warning to stderr and continues with the selected built-in catalog.

The built-in catalog contains named public resolvers and optional region labels based on the reference project. The catalog is static and documented in the CLI help or README; region labels describe catalog metadata and do not claim to identify the physical location of an anycast response.

## Query and result behavior

- Each selected resolver is queried at its configured IP address; results must not silently fall back to the machine's default resolver.
- Queries run concurrently with a maximum of 32 active resolver requests. Each request has its own timeout and result. One timeout or resolver error does not discard other results.
- DNS answers are normalized before comparison: record ordering and case differences in DNS names do not create false mismatches. A comparison key includes the DNS response code and sorted answer set, so `NXDOMAIN` differs from `NOERROR` with no matching records. Valid empty and negative DNS responses participate in consensus; timeout, transport, and malformed-response failures do not. Display order is deterministic and follows the selected resolver catalog order.
- A result row contains resolver name, IP, optional region, normalized records (an array, empty when the response has no matching records), latency in milliseconds, and either a DNS response code or an optional transport/query error.
- For the propagation summary, the largest group of equal successful DNS outcomes is the agreement group. The percentage is `largest_group_size / total_selected_resolvers * 100`; failed queries remain in the denominator. If two or more groups tie for largest, the summary reports a split and has no single consensus. If no resolver returns a valid DNS response, consensus is unknown and agreement is zero.
- A completed run exits successfully even when some resolver requests fail; those failures are represented in the results. Invalid arguments or an unusable resolver selection exit unsuccessfully.

## Output behavior

- With an interactive stdout and no output-mode flag, show an inline progress view that updates as resolver results arrive, then leave a color-coded final summary visible and restore the terminal on normal exit, errors, or interruption.
- With redirected stdout, use the plain table automatically. `--simple` forces the same table even in an interactive terminal.
- `--json` emits only the JSON document on stdout. Warnings and diagnostics go to stderr.
- JSON follows this stable shape: top-level `domain`, `record_type`, and UTC RFC 3339 `timestamp`; `results` contains one object per resolver with `server`, `ip`, `location`, `records`, `latency_ms`, and `response_code` or `error`; `propagation` contains `propagated`, `total`, `percentage`, and `consensus`. `consensus` is null for a split or unknown result. `propagated` is the largest group size, even when the groups tie.
- Plain table and TUI show resolver, region when known, records or error, latency, and agreement status.

## Resolver discovery and privacy

The program queries the requested domain and record type at each selected public resolver. Resolver discovery is opt-in: Zoneglint contacts public-dns.info only when `--fetch` is provided. The README must explain both network behaviors. The program stores no query history and does not send queries to an API owned by Zoneglint.

## Architecture

One Cargo package keeps this utility easy to build and distribute. Querying and result calculation are independent of presentation so the TUI, table, and JSON modes consume the same result model.

| File | Responsibility |
| --- | --- |
| `src/main.rs` | Start the program and coordinate query and output modes. |
| `src/cli.rs` | Define flags, defaults, and preflight validation. |
| `src/servers.rs` | Maintain built-in resolver metadata, provider filtering, fetching, and de-duplication. |
| `src/query.rs` | Convert record types, query each resolver, normalize answers, and calculate agreement. |
| `src/output.rs` | Render deterministic plain-table and JSON output. |
| `src/tui.rs` | Display concurrent progress and final results in an interactive terminal. |
| `README.md` | Explain installation, usage, output modes, resolver behavior, and network/privacy behavior. |
| `.gitignore` | Exclude Cargo build output. |

The implementation will use Clap for argument parsing, Tokio for asynchronous work, Hickory DNS for queries to explicitly configured upstreams, Ratatui with Crossterm for the interactive terminal, Serde/Serde JSON for machine output, and Reqwest for opt-in HTTP discovery. The minimum supported Rust version is 1.88, matching the current Ratatui application crate requirement. Dependency versions will be resolved and locked when implementation begins.

## Out of scope for v1

- Modifying DNS records or managing DNS zones.
- DNS-over-HTTPS, DNS-over-TLS, authenticated resolvers, or a local recursive resolver.
- Saving history, caching results across runs, or exposing a public library API.
- Claiming authoritative geographic measurement from resolver region labels.
- Copying source code from the Go reference implementation.

## Acceptance criteria

1. `zoneglint <domain>` defaults to an `A` query and runs against the built-in resolver catalog.
2. Every listed record type is accepted; invalid types and invalid provider filters fail before DNS queries begin.
3. Resolver requests run concurrently, respect the per-resolver timeout and concurrency limit, and retain partial results when individual requests fail.
4. Interactive terminals show progress; redirected output is a plain table; `--simple` and `--json` select deterministic non-animated formats.
5. JSON is valid, written without non-JSON stdout noise, and includes per-resolver outcomes and the defined agreement summary.
6. `--fetch` is the only path that contacts public-dns.info; fetched endpoints are count-limited and de-duplicated.
7. The README documents supported flags, examples, partial failures, and the DNS/discovery network behavior.

## Reference and technical documentation

- [digga README](https://github.com/knowald/digga#readme) — reference behavior and CLI surface.
- [Hickory resolver documentation](https://docs.rs/hickory-resolver/latest/hickory_resolver/) — Rust DNS resolver.
- [Tokio runtime documentation](https://docs.rs/tokio/latest/tokio/runtime/) — asynchronous runtime.
- [Clap documentation](https://docs.rs/clap/latest/clap/) — command-line parsing.
- [Ratatui documentation](https://docs.rs/ratatui/latest/ratatui/) — terminal UI.
- [Reqwest documentation](https://docs.rs/reqwest/latest/reqwest/) — optional HTTP client.
