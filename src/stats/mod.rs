pub mod tui;

use crate::cost::types::{CostRecord, TokenBreakdown};
use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate, Utc};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Resolved filter options for stats queries.
pub struct StatsFilter {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub room: Option<String>,
}

/// Aggregated stats result for display.
pub struct StatsResult {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cache_read: u64,
    pub total_cache_write: u64,
    pub total_cost: f64,
    pub request_count: usize,
    pub by_channel_room: Vec<ChannelRoomRow>,
    pub breakdown: Option<BreakdownResult>,
    pub records: Vec<CostRecord>,
}

pub struct ChannelRoomRow {
    pub label: String,
    pub requests: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
}

pub struct BreakdownResult {
    pub categories: Vec<(String, u64, f64)>, // (name, bytes, percent)
}

/// Resolve the costs.jsonl path from workspace dir.
pub fn costs_jsonl_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("state").join("costs.jsonl")
}

/// Read all cost records from the JSONL file.
pub fn read_records(path: &Path) -> Result<Vec<CostRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file =
        File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for line in reader.lines() {
        let raw = line?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<CostRecord>(trimmed) {
            Ok(record) => records.push(record),
            Err(_) => continue,
        }
    }

    Ok(records)
}

/// Build a StatsFilter from CLI args.
pub fn build_filter(
    date: Option<&str>,
    period: Option<&str>,
    room: Option<String>,
) -> Result<StatsFilter> {
    let today = Utc::now().date_naive();

    let (start_date, end_date) = match (date, period) {
        (Some(d), _) => {
            let parsed =
                NaiveDate::parse_from_str(d, "%Y-%m-%d").context("Invalid date format, expected YYYY-MM-DD")?;
            match period.unwrap_or("day") {
                "week" => {
                    let end = parsed + chrono::Duration::days(6);
                    (parsed, end)
                }
                "month" => {
                    let end = if parsed.month() == 12 {
                        NaiveDate::from_ymd_opt(parsed.year() + 1, 1, 1)
                    } else {
                        NaiveDate::from_ymd_opt(parsed.year(), parsed.month() + 1, 1)
                    }
                    .unwrap_or(parsed)
                        - chrono::Duration::days(1);
                    (
                        NaiveDate::from_ymd_opt(parsed.year(), parsed.month(), 1).unwrap_or(parsed),
                        end,
                    )
                }
                _ => (parsed, parsed),
            }
        }
        (None, Some("week")) => {
            let start = today - chrono::Duration::days(6);
            (start, today)
        }
        (None, Some("month")) => {
            let start =
                NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
            (start, today)
        }
        _ => (today, today),
    };

    Ok(StatsFilter {
        start_date,
        end_date,
        room,
    })
}

/// Aggregate records according to the filter.
pub fn aggregate(records: &[CostRecord], filter: &StatsFilter) -> StatsResult {
    let mut filtered: Vec<&CostRecord> = records
        .iter()
        .filter(|r| {
            let date = r.usage.timestamp.naive_utc().date();
            date >= filter.start_date && date <= filter.end_date
        })
        .filter(|r| {
            if let Some(ref room_filter) = filter.room {
                r.room.as_deref() == Some(room_filter.as_str())
            } else {
                true
            }
        })
        .collect();

    filtered.sort_by_key(|r| r.usage.timestamp);

    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_cache_read: u64 = 0;
    let mut total_cache_write: u64 = 0;
    let mut total_cost: f64 = 0.0;

    // channel/room -> aggregated row
    let mut channel_room_map: HashMap<String, (usize, u64, u64, f64)> = HashMap::new();

    // Breakdown aggregation
    let mut agg_breakdown = TokenBreakdown::default();
    let mut has_breakdown = false;

    for r in &filtered {
        total_input += r.usage.input_tokens;
        total_output += r.usage.output_tokens;
        total_cache_read += r.usage.cache_read_tokens.unwrap_or(0);
        total_cache_write += r.usage.cache_write_tokens.unwrap_or(0);
        total_cost += r.usage.cost_usd;

        let channel = r.channel.as_deref().unwrap_or("unknown");
        let room = r.room.as_deref().unwrap_or("default");
        let label = format!("{channel}/{room}");
        let entry = channel_room_map.entry(label).or_insert((0, 0, 0, 0.0));
        entry.0 += 1;
        entry.1 += r.usage.input_tokens;
        entry.2 += r.usage.output_tokens;
        entry.3 += r.usage.cost_usd;

        if let Some(ref bd) = r.breakdown {
            has_breakdown = true;
            agg_breakdown.tooling += bd.tooling;
            agg_breakdown.safety += bd.safety;
            agg_breakdown.skills += bd.skills;
            agg_breakdown.identity += bd.identity;
            agg_breakdown.workspace_files += bd.workspace_files;
            agg_breakdown.runtime += bd.runtime;
            agg_breakdown.conversation_history += bd.conversation_history;
            agg_breakdown.tool_results += bd.tool_results;
            agg_breakdown.memory_context += bd.memory_context;
            agg_breakdown.user_message += bd.user_message;
            agg_breakdown.channel_context += bd.channel_context;
            agg_breakdown.assistant_response += bd.assistant_response;
            agg_breakdown.tool_calls_output += bd.tool_calls_output;
            agg_breakdown.thinking += bd.thinking;
        }
    }

    let mut by_channel_room: Vec<ChannelRoomRow> = channel_room_map
        .into_iter()
        .map(|(label, (requests, input, output, cost))| ChannelRoomRow {
            label,
            requests,
            input_tokens: input,
            output_tokens: output,
            cost,
        })
        .collect();
    by_channel_room.sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal));

    let breakdown = if has_breakdown {
        let request_count = filtered.len().max(1) as u64;
        let categories = breakdown_categories(&agg_breakdown, request_count);
        Some(BreakdownResult { categories })
    } else {
        None
    };

    StatsResult {
        start_date: filter.start_date,
        end_date: filter.end_date,
        total_input,
        total_output,
        total_cache_read,
        total_cache_write,
        total_cost,
        request_count: filtered.len(),
        by_channel_room,
        breakdown,
        records: filtered.into_iter().cloned().collect(),
    }
}

fn breakdown_categories(bd: &TokenBreakdown, request_count: u64) -> Vec<(String, u64, f64)> {
    let items = [
        ("identity", bd.identity),
        ("workspace_files", bd.workspace_files),
        ("skills", bd.skills),
        ("conversation", bd.conversation_history),
        ("tooling", bd.tooling),
        ("tool_results", bd.tool_results),
        ("user_message", bd.user_message),
        ("memory_context", bd.memory_context),
        ("channel_context", bd.channel_context),
        ("safety", bd.safety),
        ("runtime", bd.runtime),
        ("assistant_response", bd.assistant_response),
        ("tool_calls_output", bd.tool_calls_output),
        ("thinking", bd.thinking),
    ];

    let total: u64 = items.iter().map(|(_, v)| v).sum();
    let total_f = total.max(1) as f64;

    let mut result: Vec<(String, u64, f64)> = items
        .iter()
        .filter(|(_, v)| *v > 0)
        .map(|(name, v)| {
            let avg = v / request_count.max(1);
            let pct = (*v as f64 / total_f) * 100.0;
            (name.to_string(), avg, pct)
        })
        .collect();

    result.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    result
}

/// Format and print stats to stdout.
pub fn print_stats(result: &StatsResult) {
    let date_label = if result.start_date == result.end_date {
        result.start_date.format("%Y-%m-%d").to_string()
    } else {
        format!(
            "{} to {}",
            result.start_date.format("%Y-%m-%d"),
            result.end_date.format("%Y-%m-%d")
        )
    };

    println!("Snowclaw Token Usage — {date_label}");
    println!();

    if result.request_count == 0 {
        println!("No usage data for this period.");
        return;
    }

    println!(
        "Total: {} input / {} output / {} cache_read / {} cache_write",
        fmt_num(result.total_input),
        fmt_num(result.total_output),
        fmt_num(result.total_cache_read),
        fmt_num(result.total_cache_write),
    );
    println!(
        "Cost: ${:.2} (estimated)    Requests: {}",
        result.total_cost, result.request_count
    );
    println!();

    if !result.by_channel_room.is_empty() {
        println!("By Channel/Room:");
        // Calculate column widths
        let max_label = result
            .by_channel_room
            .iter()
            .map(|r| r.label.len())
            .max()
            .unwrap_or(10)
            .max(10);

        for row in &result.by_channel_room {
            println!(
                "  {:<width$} {:>4} req  {:>8} in  {:>8} out  ${:.2}",
                row.label,
                row.requests,
                fmt_num(row.input_tokens),
                fmt_num(row.output_tokens),
                row.cost,
                width = max_label,
            );
        }
        println!();
    }

    if let Some(ref bd) = result.breakdown {
        println!("By Category (avg per request):");
        for (name, avg_bytes, pct) in &bd.categories {
            println!(
                "  {:<20} {:>8} bytes ({:.0}%)",
                name, fmt_num(*avg_bytes), pct
            );
        }
        println!();
    }
}

/// Print stats as JSON.
pub fn print_stats_json(result: &StatsResult) -> Result<()> {
    #[derive(serde::Serialize)]
    struct JsonOutput {
        start_date: String,
        end_date: String,
        total_input_tokens: u64,
        total_output_tokens: u64,
        total_cache_read_tokens: u64,
        total_cache_write_tokens: u64,
        total_cost_usd: f64,
        request_count: usize,
        by_channel_room: Vec<JsonChannelRoom>,
        #[serde(skip_serializing_if = "Option::is_none")]
        breakdown: Option<Vec<JsonCategory>>,
    }

    #[derive(serde::Serialize)]
    struct JsonChannelRoom {
        label: String,
        requests: usize,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
    }

    #[derive(serde::Serialize)]
    struct JsonCategory {
        name: String,
        avg_bytes: u64,
        percent: f64,
    }

    let output = JsonOutput {
        start_date: result.start_date.format("%Y-%m-%d").to_string(),
        end_date: result.end_date.format("%Y-%m-%d").to_string(),
        total_input_tokens: result.total_input,
        total_output_tokens: result.total_output,
        total_cache_read_tokens: result.total_cache_read,
        total_cache_write_tokens: result.total_cache_write,
        total_cost_usd: result.total_cost,
        request_count: result.request_count,
        by_channel_room: result
            .by_channel_room
            .iter()
            .map(|r| JsonChannelRoom {
                label: r.label.clone(),
                requests: r.requests,
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
                cost_usd: r.cost,
            })
            .collect(),
        breakdown: result.breakdown.as_ref().map(|bd| {
            bd.categories
                .iter()
                .map(|(name, avg, pct)| JsonCategory {
                    name: name.clone(),
                    avg_bytes: *avg,
                    percent: *pct,
                })
                .collect()
        }),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn fmt_num(n: u64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_num_formats_thousands() {
        assert_eq!(fmt_num(0), "0");
        assert_eq!(fmt_num(123), "123");
        assert_eq!(fmt_num(1234), "1,234");
        assert_eq!(fmt_num(1_234_567), "1,234,567");
    }

    #[test]
    fn build_filter_defaults_to_today() {
        let filter = build_filter(None, None, None).unwrap();
        let today = Utc::now().date_naive();
        assert_eq!(filter.start_date, today);
        assert_eq!(filter.end_date, today);
    }

    #[test]
    fn build_filter_specific_date() {
        let filter = build_filter(Some("2026-01-15"), None, None).unwrap();
        assert_eq!(
            filter.start_date,
            NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()
        );
    }

    #[test]
    fn build_filter_week_period() {
        let filter = build_filter(None, Some("week"), None).unwrap();
        let today = Utc::now().date_naive();
        assert_eq!(filter.end_date, today);
        assert_eq!(filter.start_date, today - chrono::Duration::days(6));
    }

    #[test]
    fn aggregate_empty_records() {
        let filter = build_filter(None, None, None).unwrap();
        let result = aggregate(&[], &filter);
        assert_eq!(result.request_count, 0);
        assert_eq!(result.total_cost, 0.0);
    }
}
