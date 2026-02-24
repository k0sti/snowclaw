//! CLI subcommands for Nostr task management.
//!
//! Provides `snowclaw task create`, `task list`, `task status`, and `task update`
//! using the local task store (nostr_tasks.json in workspace).
//! Tasks use sequential SNOW-N IDs (e.g. SNOW-1, SNOW-2).

use anyhow::{bail, Result};
use clap::Subcommand;

use crate::config::Config;

#[derive(Subcommand, Debug)]
pub enum TaskCommands {
    /// Create a new task
    Create {
        /// Task title
        title: String,
        /// Task description
        #[arg(short, long)]
        description: Option<String>,
        /// Initial status (default: draft)
        #[arg(short, long, default_value = "draft")]
        status: String,
    },
    /// List tasks
    List {
        /// Filter by status
        #[arg(short, long)]
        status: Option<String>,
    },
    /// Show task status and history
    Status {
        /// Task ID (e.g. SNOW-1 or just 1)
        id: String,
    },
    /// Update a task's status
    Update {
        /// Task ID (e.g. SNOW-1 or just 1)
        id: String,
        /// New status: draft, queued, executing, blocked, review, done, failed, cancelled
        #[arg(short, long)]
        status: String,
        /// Optional detail/note
        #[arg(short, long)]
        detail: Option<String>,
    },
}

/// Resolve the task store path from config.
fn task_store_path(config: &Config) -> std::path::PathBuf {
    config.workspace_dir.join("nostr_tasks.json")
}

/// Load tasks from the JSON store.
fn load_tasks(config: &Config) -> Result<Vec<serde_json::Value>> {
    let path = task_store_path(config);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(&path)?;
    let tasks: Vec<serde_json::Value> = serde_json::from_str(&data)?;
    Ok(tasks)
}

/// Save tasks to the JSON store.
fn save_tasks(config: &Config, tasks: &[serde_json::Value]) -> Result<()> {
    let path = task_store_path(config);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(tasks)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Map status string to Nostr event kind.
pub fn status_to_kind(status: &str) -> Result<u16> {
    match status {
        "queued" => Ok(1630),
        "done" => Ok(1631),
        "cancelled" => Ok(1632),
        "draft" => Ok(1633),
        "executing" => Ok(1634),
        "blocked" => Ok(1635),
        "review" => Ok(1636),
        "failed" => Ok(1637),
        other => bail!("Unknown status: {other}. Valid: draft, queued, executing, blocked, review, done, failed, cancelled"),
    }
}

/// Extract the numeric part from a SNOW-N task ID.
fn parse_snow_number(id: &str) -> Option<u64> {
    id.strip_prefix("SNOW-")
        .and_then(|n| n.parse::<u64>().ok())
}

/// Derive the next SNOW-N number from existing tasks.
fn next_task_number(tasks: &[serde_json::Value]) -> u64 {
    let max = tasks
        .iter()
        .filter_map(|t| t.get("id").and_then(|v| v.as_str()))
        .filter_map(parse_snow_number)
        .max()
        .unwrap_or(0);
    max + 1
}

/// Normalize a user-provided task ID to SNOW-N format.
/// Accepts "SNOW-1", "snow-1", or just "1".
fn normalize_task_id(input: &str) -> String {
    let trimmed = input.trim();
    if let Ok(n) = trimmed.parse::<u64>() {
        return format!("SNOW-{n}");
    }
    if let Some(n) = trimmed
        .to_uppercase()
        .strip_prefix("SNOW-")
        .and_then(|s| s.parse::<u64>().ok())
    {
        return format!("SNOW-{n}");
    }
    // Legacy: support old task-{timestamp} IDs
    trimmed.to_string()
}

/// Find a task by normalized ID.
fn find_task<'a>(
    tasks: &'a [serde_json::Value],
    id: &str,
) -> Option<&'a serde_json::Value> {
    let normalized = normalize_task_id(id);
    tasks
        .iter()
        .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(&normalized))
}

/// Find a task by normalized ID (mutable).
fn find_task_mut<'a>(
    tasks: &'a mut [serde_json::Value],
    id: &str,
) -> Option<&'a mut serde_json::Value> {
    let normalized = normalize_task_id(id);
    tasks
        .iter_mut()
        .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(&normalized))
}

pub fn handle_command(cmd: TaskCommands, config: &Config) -> Result<()> {
    match cmd {
        TaskCommands::Create {
            title,
            description,
            status,
        } => {
            let _kind = status_to_kind(&status)?;

            let mut tasks = load_tasks(config)?;
            let number = next_task_number(&tasks);
            let task_id = format!("SNOW-{number}");
            let now = chrono::Utc::now().to_rfc3339();

            let task = serde_json::json!({
                "id": task_id,
                "title": title,
                "description": description.unwrap_or_default(),
                "status": status,
                "created_at": now,
                "updated_at": now,
                "kind": 1621,
                "status_history": [{
                    "status": status,
                    "kind": _kind,
                    "timestamp": now,
                }]
            });

            tasks.push(task);
            save_tasks(config, &tasks)?;

            println!("Created {task_id}: {title}");
            Ok(())
        }

        TaskCommands::List { status } => {
            let tasks = load_tasks(config)?;
            let filtered: Vec<&serde_json::Value> = if let Some(ref s) = status {
                tasks
                    .iter()
                    .filter(|t| t.get("status").and_then(|v| v.as_str()) == Some(s))
                    .collect()
            } else {
                tasks.iter().collect()
            };

            if filtered.is_empty() {
                println!("No tasks found.");
                return Ok(());
            }

            println!("{} task(s):", filtered.len());
            for task in &filtered {
                let id = task.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let title = task.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                let st = task.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let updated = task
                    .get("updated_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                println!("  [{st}] {id}: {title} (updated: {updated})");
            }

            Ok(())
        }

        TaskCommands::Status { id } => {
            let tasks = load_tasks(config)?;
            let task = find_task(&tasks, &id);

            match task {
                Some(task) => {
                    let task_id = task.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                    let title = task.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                    let status = task.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    let desc = task
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let created = task
                        .get("created_at")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let updated = task
                        .get("updated_at")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");

                    println!("Task: {task_id}");
                    println!("  Title:   {title}");
                    println!("  Status:  {status}");
                    if !desc.is_empty() {
                        println!("  Desc:    {desc}");
                    }
                    println!("  Created: {created}");
                    println!("  Updated: {updated}");

                    if let Some(history) = task.get("status_history").and_then(|v| v.as_array()) {
                        if !history.is_empty() {
                            println!("  History:");
                            for entry in history {
                                let st = entry
                                    .get("status")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?");
                                let ts = entry
                                    .get("timestamp")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?");
                                let detail = entry
                                    .get("detail")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if detail.is_empty() {
                                    println!("    {ts}: {st}");
                                } else {
                                    println!("    {ts}: {st} — {detail}");
                                }
                            }
                        }
                    }
                    Ok(())
                }
                None => {
                    let normalized = normalize_task_id(&id);
                    println!("Task not found: {normalized}");
                    Ok(())
                }
            }
        }

        TaskCommands::Update {
            id,
            status,
            detail,
        } => {
            let kind = status_to_kind(&status)?;
            let mut tasks = load_tasks(config)?;
            let now = chrono::Utc::now().to_rfc3339();

            let task = find_task_mut(&mut tasks, &id);

            match task {
                Some(task) => {
                    let task_id = task
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    task["status"] = serde_json::json!(status);
                    task["updated_at"] = serde_json::json!(now);
                    if let Some(history) = task.get_mut("status_history") {
                        if let Some(arr) = history.as_array_mut() {
                            arr.push(serde_json::json!({
                                "status": status,
                                "kind": kind,
                                "detail": detail.as_deref().unwrap_or(""),
                                "timestamp": now,
                            }));
                        }
                    }
                    save_tasks(config, &tasks)?;
                    println!("Updated {task_id} to {status}");
                    Ok(())
                }
                None => {
                    let normalized = normalize_task_id(&id);
                    bail!("Task not found: {normalized}")
                }
            }
        }
    }
}

