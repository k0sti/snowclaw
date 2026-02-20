//! CLI subcommands for Nostr task management.
//!
//! Provides `snowclaw task create`, `task list`, `task status`, and `task update`
//! using the local task store (nostr_tasks.json in workspace).

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
        /// Task ID
        id: String,
    },
    /// Update a task's status
    Update {
        /// Task ID
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
fn status_to_kind(status: &str) -> Result<u16> {
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

pub fn handle_command(cmd: TaskCommands, config: &Config) -> Result<()> {
    match cmd {
        TaskCommands::Create {
            title,
            description,
            status,
        } => {
            let _kind = status_to_kind(&status)?;

            let task_id = format!("task-{}", chrono::Utc::now().timestamp_millis());
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

            let mut tasks = load_tasks(config)?;
            tasks.push(task);
            save_tasks(config, &tasks)?;

            println!("Created task {task_id}: {title}");
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
            let task = tasks
                .iter()
                .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(&id));

            match task {
                Some(task) => {
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

                    println!("Task: {id}");
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
                    println!("Task not found: {id}");
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

            let task = tasks
                .iter_mut()
                .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(&id));

            match task {
                Some(task) => {
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
                    println!("Updated task {id} to {status}");
                    Ok(())
                }
                None => {
                    bail!("Task not found: {id}")
                }
            }
        }
    }
}
