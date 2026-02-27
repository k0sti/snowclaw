//! Snowclaw-specific CLI command handlers.
//!
//! Extracted from `main.rs` to minimize upstream diff. Stats and other
//! Snowclaw-only CLI subcommand logic lives here.

use crate::config::Config;
use crate::stats;
use anyhow::Result;

/// Handle the `stats` CLI subcommand.
pub fn handle_stats(
    config: &Config,
    date: Option<String>,
    period: Option<String>,
    room: Option<String>,
    json: bool,
    live: bool,
) -> Result<()> {
    if live {
        return stats::tui::run(&config.workspace_dir);
    }
    let jsonl_path = stats::costs_jsonl_path(&config.workspace_dir);
    let records = stats::read_records(&jsonl_path)?;
    let filter = stats::build_filter(date.as_deref(), period.as_deref(), room)?;
    let result = stats::aggregate(&records, &filter);
    if json {
        stats::print_stats_json(&result)?;
    } else {
        stats::print_stats(&result);
    }
    Ok(())
}
