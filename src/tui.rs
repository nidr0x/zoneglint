use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::style::force_color_output;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table};
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::sync::mpsc;

use crate::query::{self, QueryEvent, RecordType, ServerResult};

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        let height = crossterm::terminal::size()
            .context("could not read terminal size")?
            .1
            .saturating_sub(1)
            .max(1);
        enable_raw_mode().context("could not enable terminal raw mode")?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, crossterm::cursor::Hide) {
            let _ = disable_raw_mode();
            let _ = execute!(stdout, crossterm::cursor::Show);
            return Err(error).context("could not set up terminal display");
        }
        let backend = CrosstermBackend::new(stdout);
        match Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        ) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stdout = io::stdout();
                let _ = execute!(stdout, crossterm::cursor::Show);
                Err(error).context("could not initialize terminal renderer")
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), crossterm::cursor::Show);
        let _ = self.terminal.show_cursor();
    }
}

pub async fn run(
    domain: &str,
    record_type: RecordType,
    events: &mut mpsc::Receiver<QueryEvent>,
) -> Result<Vec<ServerResult>> {
    force_color_output(true);
    let mut terminal = TerminalGuard::new()?;
    let mut total = 0usize;
    let mut results = Vec::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(50));
    let mut animation_frame = 0u64;
    let started_at = Instant::now();
    let mut stats = QueryStats::default();
    let mut progress = 0.0;
    let mut progress_velocity = 0.0;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let mut interrupted = false;

    loop {
        terminal.terminal.draw(|frame| {
            let area = frame.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(4),
                    Constraint::Length(5),
                ])
                .split(area);
            let summary = query::summarize(&results);
            let title = Paragraph::new(Line::from(vec![
                Span::styled(
                    domain.to_owned(),
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  ·  {}  ·  ", record_type.as_str()),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{}/{} responders", results.len(), total),
                    Style::default().fg(Color::White),
                ),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(" ZONEGLINT "),
            );
            frame.render_widget(title, chunks[0]);

            let rows: Vec<Row> = results
                .iter()
                .map(|result| {
                    let detail = if let Some(error) = &result.error {
                        format!("ERROR: {error}")
                    } else if result.records.is_empty() {
                        format!(
                            "{} (no records)",
                            result.response_code.as_deref().unwrap_or("pending")
                        )
                    } else {
                        format!(
                            "{}: {}",
                            result.response_code.as_deref().unwrap_or(""),
                            result.records.join(", ")
                        )
                    };
                    let agreed = query::outcome_label(result)
                        .zip(summary.consensus.as_deref())
                        .is_some_and(|(label, consensus)| label == consensus);
                    let color = if result.error.is_some() {
                        Color::LightRed
                    } else if agreed {
                        Color::LightGreen
                    } else {
                        Color::LightYellow
                    };
                    Row::new(vec![
                        Cell::from(result.server.name.clone()),
                        Cell::from(result.server.ip.to_string()),
                        Cell::from(detail),
                        Cell::from(format!("{} ms", result.latency_ms)),
                    ])
                    .style(Style::default().fg(color))
                })
                .collect();
            let table = Table::new(
                rows,
                [
                    Constraint::Length(30),
                    Constraint::Length(17),
                    Constraint::Min(32),
                    Constraint::Length(12),
                ],
            )
            .header(
                Row::new(["RESOLVER", "IP ADDRESS", "ANSWER / ERROR", "LATENCY"]).style(
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                ),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .title(" RESOLVER RESULTS "),
            );
            frame.render_widget(table, chunks[1]);

            let status = if total == 0 {
                "DISCOVERING"
            } else if results.len() == total {
                "COMPLETE"
            } else {
                "QUERYING"
            };
            let agreement = if summary.total == 0 {
                "waiting for answers".to_owned()
            } else {
                format!(
                    "agreement {}/{} ({:.1}%)",
                    summary.propagated, summary.total, summary.percentage
                )
            };
            let summary_line = Line::from(vec![
                Span::styled(
                    status,
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}/{} resolvers", results.len(), total),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("  ·  {agreement}"),
                    Style::default().fg(Color::LightYellow),
                ),
            ]);
            let stats_line = stats.line(started_at.elapsed());
            let mut progress_line = progress_bar(
                progress,
                results.len(),
                total,
                usize::from(area.width.saturating_sub(70)),
                animation_frame,
            );
            progress_line.push(Span::raw("  "));
            progress_line.push(Span::styled(
                if results.len() == total && total > 0 {
                    "ready"
                } else {
                    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                        [animation_frame as usize % 10]
                },
                Style::default().fg(Color::LightCyan),
            ));
            let footer = Paragraph::new(Text::from(vec![
                summary_line,
                stats_line,
                Line::from(progress_line),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan)),
            );
            frame.render_widget(footer, chunks[2]);
        })?;

        if total > 0 && results.len() == total {
            break;
        }

        tokio::select! {
            _ = &mut ctrl_c => {
                interrupted = true;
                break;
            }
            _ = ticker.tick() => {
                animation_frame = animation_frame.wrapping_add(1);
                let target = if total == 0 {
                    0.0
                } else {
                    results.len() as f64 / total as f64
                };
                spring_step(&mut progress, &mut progress_velocity, target);
                if event::poll(Duration::ZERO).context("could not poll terminal input")?
                    && let Event::Key(key) = event::read().context("could not read terminal input")?
                    && key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    interrupted = true;
                    break;
                }
            }
            event = events.recv() => match event {
                Some(QueryEvent::Started { total: count }) => total = count,
                Some(QueryEvent::Completed(result)) => {
                    stats.observe(&result);
                    results.push(result);
                    results.sort_by_key(|result| result.catalog_index);
                    if total > 0 && results.len() == total {
                        progress = 1.0;
                        progress_velocity = 0.0;
                    }
                }
                None => break,
            }
        }
    }
    results.sort_by_key(|result| result.catalog_index);
    if interrupted {
        drop(terminal);
        println!(
            "Stopped after {} of {} resolver responses.",
            results.len(),
            total
        );
    }
    Ok(results)
}

#[derive(Default)]
struct QueryStats {
    successful: usize,
    errors: usize,
    total_latency_ms: u128,
    fastest_ms: Option<u64>,
    slowest_ms: Option<u64>,
}

impl QueryStats {
    fn observe(&mut self, result: &ServerResult) {
        if result.error.is_some() {
            self.errors += 1;
            return;
        }

        self.successful += 1;
        self.total_latency_ms += u128::from(result.latency_ms);
        self.fastest_ms = Some(
            self.fastest_ms
                .map_or(result.latency_ms, |fastest| fastest.min(result.latency_ms)),
        );
        self.slowest_ms = Some(
            self.slowest_ms
                .map_or(result.latency_ms, |slowest| slowest.max(result.latency_ms)),
        );
    }

    fn line(&self, elapsed: Duration) -> Line<'static> {
        let average = if self.successful == 0 {
            "—".to_owned()
        } else {
            format!("{} ms", self.total_latency_ms / self.successful as u128)
        };
        let latency =
            |value: Option<u64>| value.map_or_else(|| "—".to_owned(), |ms| format!("{ms} ms"));
        Line::from(vec![
            Span::styled("ELAPSED ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1}s", elapsed.as_secs_f64()),
                Style::default().fg(Color::White),
            ),
            Span::styled("  ·  FAST ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                latency(self.fastest_ms),
                Style::default().fg(Color::LightGreen),
            ),
            Span::styled("  ·  AVG ", Style::default().fg(Color::DarkGray)),
            Span::styled(average, Style::default().fg(Color::LightCyan)),
            Span::styled("  ·  SLOW ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                latency(self.slowest_ms),
                Style::default().fg(Color::LightYellow),
            ),
            Span::styled("  ·  ERRORS ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                self.errors.to_string(),
                Style::default().fg(Color::LightRed),
            ),
        ])
    }
}

fn spring_step(position: &mut f64, velocity: &mut f64, target: f64) {
    const DELTA_SECONDS: f64 = 0.05;
    const STIFFNESS: f64 = 36.0;
    const DAMPING: f64 = 12.0;
    let acceleration = STIFFNESS * (target - *position) - DAMPING * *velocity;
    *velocity += acceleration * DELTA_SECONDS;
    *position = (*position + *velocity * DELTA_SECONDS).clamp(0.0, 1.0);
}

fn progress_bar(
    progress: f64,
    completed: usize,
    total: usize,
    width: usize,
    frame: u64,
) -> Vec<Span<'static>> {
    let filled = (progress.clamp(0.0, 1.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let remaining = width - filled;
    let pulse_width = remaining.min(4);
    let travel = remaining - pulse_width;
    let period = travel.saturating_mul(2);
    let phase = frame as usize % period.saturating_add(1);
    let pulse_start = if phase <= travel {
        phase
    } else {
        period - phase
    };

    vec![
        Span::styled("█".repeat(filled), Style::default().fg(Color::LightGreen)),
        Span::styled(
            "░".repeat(pulse_start),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            "█".repeat(pulse_width),
            Style::default().fg(Color::LightCyan),
        ),
        Span::styled(
            "░".repeat(remaining - pulse_start - pulse_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!(
                "  {completed}/{total}  {:>3.0}%",
                progress.clamp(0.0, 1.0) * 100.0
            ),
            Style::default().fg(Color::White),
        ),
    ]
}
