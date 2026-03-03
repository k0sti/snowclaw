use super::{aggregate, build_filter, costs_jsonl_path, read_records, StatsResult};
use anyhow::Result;
use chrono::Utc;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame, Terminal,
};
use std::io::stdout;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Run the live TUI dashboard.
pub fn run(workspace_dir: &Path) -> Result<()> {
    let jsonl_path = costs_jsonl_path(workspace_dir);

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &jsonl_path);

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

fn run_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    jsonl_path: &Path,
) -> Result<()> {
    let mut last_mtime = file_mtime(jsonl_path);
    let mut records = read_records(jsonl_path).unwrap_or_default();
    let mut stats = compute_today_stats(&records);
    let poll_interval = Duration::from_secs(1);

    loop {
        terminal.draw(|frame| draw(frame, &stats, &records))?;

        if event::poll(poll_interval)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press
                    && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                {
                    return Ok(());
                }
            }
        }

        // Check for file changes
        let new_mtime = file_mtime(jsonl_path);
        if new_mtime != last_mtime {
            last_mtime = new_mtime;
            records = read_records(jsonl_path).unwrap_or_default();
            stats = compute_today_stats(&records);
        }
    }
}

fn compute_today_stats(records: &[crate::cost::types::CostRecord]) -> StatsResult {
    let filter = build_filter(None, None, None).unwrap();
    aggregate(records, &filter)
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn draw(frame: &mut Frame, stats: &StatsResult, records: &[crate::cost::types::CostRecord]) {
    let area = frame.area();

    // Main vertical layout
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header summary
            Constraint::Min(8),    // Recent requests
            Constraint::Length(12), // Bottom section (breakdown + rooms)
            Constraint::Length(3),  // Footer
        ])
        .split(area);

    draw_header(frame, chunks[0], stats);
    draw_recent_requests(frame, chunks[1], records);
    draw_bottom_section(frame, chunks[2], stats);
    draw_footer(frame, chunks[3], stats, records);
}

fn draw_header(frame: &mut Frame, area: Rect, stats: &StatsResult) {
    let today = Utc::now().format("%Y-%m-%d");
    let header_text = format!(
        " Today: {} in / {} out / ${:.2}    Requests: {}    Cache: {} read / {} write",
        fmt_num(stats.total_input),
        fmt_num(stats.total_output),
        stats.total_cost,
        stats.request_count,
        fmt_num(stats.total_cache_read),
        fmt_num(stats.total_cache_write),
    );

    let block = Block::default()
        .title(format!(" Snowclaw Token Monitor — {today} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(Line::from(vec![Span::styled(
        header_text,
        Style::default().fg(Color::White),
    )]))
    .block(block);

    frame.render_widget(paragraph, area);
}

fn draw_recent_requests(
    frame: &mut Frame,
    area: Rect,
    records: &[crate::cost::types::CostRecord],
) {
    let today = Utc::now().date_naive();
    let mut today_records: Vec<&crate::cost::types::CostRecord> = records
        .iter()
        .filter(|r| r.usage.timestamp.naive_utc().date() == today)
        .collect();
    today_records.sort_by(|a, b| b.usage.timestamp.cmp(&a.usage.timestamp));
    today_records.truncate(20);

    let header = Row::new(vec![
        Cell::from("Time"),
        Cell::from("Channel/Room"),
        Cell::from("Input"),
        Cell::from("Output"),
        Cell::from("Cost"),
        Cell::from("Model"),
    ])
    .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = today_records
        .iter()
        .map(|r| {
            let time = r
                .usage
                .timestamp
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string();
            let channel = r.channel.as_deref().unwrap_or("?");
            let room = r.room.as_deref().unwrap_or("?");
            let label = format!("{channel}/{room}");
            let cost_color = cost_color(r.usage.cost_usd);

            Row::new(vec![
                Cell::from(time),
                Cell::from(label),
                Cell::from(fmt_num(r.usage.input_tokens)),
                Cell::from(fmt_num(r.usage.output_tokens)),
                Cell::from(format!("${:.3}", r.usage.cost_usd))
                    .style(Style::default().fg(cost_color)),
                Cell::from(short_model(&r.usage.model)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(20),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Min(15),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(" Recent Requests ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(table, area);
}

fn draw_bottom_section(frame: &mut Frame, area: Rect, stats: &StatsResult) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    draw_breakdown(frame, chunks[0], stats);
    draw_rooms(frame, chunks[1], stats);
}

fn draw_breakdown(frame: &mut Frame, area: Rect, stats: &StatsResult) {
    let block = Block::default()
        .title(" Token Breakdown (avg) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    if let Some(ref bd) = stats.breakdown {
        let lines: Vec<Line> = bd
            .categories
            .iter()
            .take(8)
            .map(|(name, _avg, pct)| {
                let bar_width = (pct / 5.0).round() as usize;
                let filled = "\u{2588}".repeat(bar_width.min(10));
                let empty = "\u{2591}".repeat(10_usize.saturating_sub(bar_width));
                let color = if *pct > 25.0 {
                    Color::Red
                } else if *pct > 15.0 {
                    Color::Yellow
                } else {
                    Color::Green
                };

                Line::from(vec![
                    Span::styled(format!("{filled}{empty}"), Style::default().fg(color)),
                    Span::raw(format!(" {:<16} {:>4.0}%", name, pct)),
                ])
            })
            .collect();

        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, area);
    } else {
        let paragraph =
            Paragraph::new("No breakdown data available").block(block);
        frame.render_widget(paragraph, area);
    }
}

fn draw_rooms(frame: &mut Frame, area: Rect, stats: &StatsResult) {
    let block = Block::default()
        .title(" By Room (today) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    if stats.by_channel_room.is_empty() {
        let paragraph = Paragraph::new("No room data").block(block);
        frame.render_widget(paragraph, area);
        return;
    }

    let lines: Vec<Line> = stats
        .by_channel_room
        .iter()
        .take(8)
        .map(|row| {
            let cost_color = cost_color(row.cost);
            Line::from(vec![
                Span::raw(format!(" {:<18}", truncate_str(&row.label, 18))),
                Span::styled(format!("${:.2}", row.cost), Style::default().fg(cost_color)),
                Span::raw(format!("  {}r", row.requests)),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_footer(
    frame: &mut Frame,
    area: Rect,
    stats: &StatsResult,
    records: &[crate::cost::types::CostRecord],
) {
    let today = Utc::now().date_naive();
    let today_records: Vec<&crate::cost::types::CostRecord> = records
        .iter()
        .filter(|r| r.usage.timestamp.naive_utc().date() == today)
        .collect();

    // Rate calc: requests per hour
    let rate = if today_records.len() >= 2 {
        let first = today_records
            .iter()
            .map(|r| r.usage.timestamp)
            .min()
            .unwrap();
        let last = today_records
            .iter()
            .map(|r| r.usage.timestamp)
            .max()
            .unwrap();
        let hours = (last - first).num_seconds() as f64 / 3600.0;
        if hours > 0.01 {
            today_records.len() as f64 / hours
        } else {
            0.0
        }
    } else {
        0.0
    };

    // Cache hit ratio
    let cache_ratio = if stats.total_input > 0 {
        (stats.total_cache_read as f64 / stats.total_input as f64) * 100.0
    } else {
        0.0
    };

    let footer_text = format!(
        " Rate: {:.1} req/hr   Cache hit: {:.0}%   Press 'q' to quit",
        rate, cache_ratio
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(Line::from(vec![Span::styled(
        footer_text,
        Style::default().fg(Color::Gray),
    )]))
    .block(block);

    frame.render_widget(paragraph, area);
}

fn cost_color(cost: f64) -> Color {
    if cost > 0.10 {
        Color::Red
    } else if cost > 0.03 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn short_model(model: &str) -> String {
    // Strip provider prefix and truncate
    let name = model.rsplit_once('/').map(|(_, m)| m).unwrap_or(model);
    if name.len() > 25 {
        format!("{}…", &name[..24])
    } else {
        name.to_string()
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}…", &s[..max - 1])
    } else {
        s.to_string()
    }
}

fn fmt_num(n: u64) -> String {
    super::fmt_num(n)
}
