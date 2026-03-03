//! Nostr task management tool for the agent.
//!
//! Exposes task creation, status updates, and listing as an agent-callable tool
//! via the Nostr event protocol (kind 1621 for tasks, 1630-1637 for status).
//! Tasks use sequential SNOW-N IDs (e.g. SNOW-1, SNOW-2).

use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;

/// Tool for managing Nostr-native tasks.
///
/// Supports creating tasks (kind 1621), updating task status (kinds 1630-1637),
/// and listing tasks from the local task store.
pub struct NostrTaskTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
}

impl NostrTaskTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: &Path) -> Self {
        Self {
            security,
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    /// Load tasks from the local JSON store.
    fn load_tasks(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        let path = self.workspace_dir.join("nostr_tasks.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let data = std::fs::read_to_string(&path)?;
        let tasks: Vec<serde_json::Value> = serde_json::from_str(&data)?;
        Ok(tasks)
    }

    /// Save tasks to the local JSON store.
    fn save_tasks(&self, tasks: &[serde_json::Value]) -> anyhow::Result<()> {
        let path = self.workspace_dir.join("nostr_tasks.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(tasks)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Derive the next SNOW-N number from existing tasks.
    fn next_task_number(&self, tasks: &[serde_json::Value]) -> u64 {
        let max = tasks
            .iter()
            .filter_map(|t| t.get("id").and_then(|v| v.as_str()))
            .filter_map(|id| id.strip_prefix("SNOW-"))
            .filter_map(|n| n.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        max + 1
    }

    /// Normalize a task ID to SNOW-N format.
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
}

#[async_trait]
impl Tool for NostrTaskTool {
    fn name(&self) -> &str {
        "nostr_task"
    }

    fn description(&self) -> &str {
        "Manage Nostr-native tasks. Actions: create (new task), update (change status), list (show tasks). \
         Task IDs use SNOW-N format (e.g. SNOW-1). \
         Task statuses: draft, queued, executing, blocked, review, done, failed, cancelled."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "update", "list"],
                    "description": "Action to perform"
                },
                "title": {
                    "type": "string",
                    "description": "Task title (for create)"
                },
                "description": {
                    "type": "string",
                    "description": "Task description (for create)"
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID, e.g. SNOW-1 (for update)"
                },
                "status": {
                    "type": "string",
                    "enum": ["draft", "queued", "executing", "blocked", "review", "done", "failed", "cancelled"],
                    "description": "New status (for update)"
                },
                "detail": {
                    "type": "string",
                    "description": "Status detail/note (for update)"
                },
                "filter_status": {
                    "type": "string",
                    "description": "Filter by status (for list)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match action {
            "create" => {
                let title = args
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Untitled task");
                let description = args
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let mut tasks = self.load_tasks()?;
                let number = self.next_task_number(&tasks);
                let task_id = format!("SNOW-{number}");
                let now = chrono::Utc::now().to_rfc3339();

                let task = json!({
                    "id": task_id,
                    "title": title,
                    "description": description,
                    "status": "draft",
                    "created_at": now,
                    "updated_at": now,
                    "kind": 1621,
                    "status_history": [{
                        "status": "draft",
                        "kind": 1633,
                        "timestamp": now,
                    }]
                });

                tasks.push(task);
                self.save_tasks(&tasks)?;

                Ok(ToolResult {
                    success: true,
                    output: format!("Created {task_id}: {title}"),
                    error: None,
                })
            }

            "update" => {
                let raw_id = match args.get("task_id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("task_id is required for update".into()),
                        })
                    }
                };
                let task_id = Self::normalize_task_id(raw_id);

                let new_status = match args.get("status").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("status is required for update".into()),
                        })
                    }
                };

                let detail = args
                    .get("detail")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let status_kind = match new_status {
                    "queued" => 1630,
                    "done" => 1631,
                    "cancelled" => 1632,
                    "draft" => 1633,
                    "executing" => 1634,
                    "blocked" => 1635,
                    "review" => 1636,
                    "failed" => 1637,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("Unknown status: {new_status}")),
                        })
                    }
                };

                let mut tasks = self.load_tasks()?;
                let now = chrono::Utc::now().to_rfc3339();

                let task = tasks.iter_mut().find(|t| {
                    t.get("id").and_then(|v| v.as_str()) == Some(&task_id)
                });

                match task {
                    Some(task) => {
                        task["status"] = json!(new_status);
                        task["updated_at"] = json!(now);
                        if let Some(history) = task.get_mut("status_history") {
                            if let Some(arr) = history.as_array_mut() {
                                arr.push(json!({
                                    "status": new_status,
                                    "kind": status_kind,
                                    "detail": detail,
                                    "timestamp": now,
                                }));
                            }
                        }
                        self.save_tasks(&tasks)?;
                        Ok(ToolResult {
                            success: true,
                            output: format!("Updated {task_id} to {new_status}"),
                            error: None,
                        })
                    }
                    None => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Task not found: {task_id}")),
                    }),
                }
            }

            "list" => {
                let filter_status = args
                    .get("filter_status")
                    .and_then(|v| v.as_str());

                let tasks = self.load_tasks()?;
                let filtered: Vec<&serde_json::Value> = if let Some(status) = filter_status {
                    tasks
                        .iter()
                        .filter(|t| t.get("status").and_then(|v| v.as_str()) == Some(status))
                        .collect()
                } else {
                    tasks.iter().collect()
                };

                if filtered.is_empty() {
                    return Ok(ToolResult {
                        success: true,
                        output: "No tasks found.".into(),
                        error: None,
                    });
                }

                let mut output = format!("{} task(s):\n", filtered.len());
                for task in &filtered {
                    let id = task.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                    let title = task.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                    let status = task.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    let updated = task.get("updated_at").and_then(|v| v.as_str()).unwrap_or("?");
                    output.push_str(&format!("  [{status}] {id}: {title} (updated: {updated})\n"));
                }

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }

            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: {action}. Use create, update, or list.")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (NostrTaskTool, TempDir) {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let tool = NostrTaskTool::new(security, tmp.path());
        (tool, tmp)
    }

    #[test]
    fn tool_metadata() {
        let (tool, _tmp) = setup();
        assert_eq!(tool.name(), "nostr_task");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters_schema()["properties"]["action"].is_object());
    }

    #[tokio::test]
    async fn create_and_list() {
        let (tool, _tmp) = setup();

        let result = tool
            .execute(json!({"action": "create", "title": "Test task", "description": "A test"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("SNOW-1"));
        assert!(result.output.contains("Test task"));

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("SNOW-1"));
        assert!(result.output.contains("Test task"));
        assert!(result.output.contains("draft"));
    }

    #[tokio::test]
    async fn create_sequential_ids() {
        let (tool, _tmp) = setup();

        tool.execute(json!({"action": "create", "title": "First"}))
            .await
            .unwrap();
        let result = tool
            .execute(json!({"action": "create", "title": "Second"}))
            .await
            .unwrap();
        assert!(result.output.contains("SNOW-2"));
    }

    #[tokio::test]
    async fn update_status() {
        let (tool, _tmp) = setup();

        tool.execute(json!({"action": "create", "title": "Update me"}))
            .await
            .unwrap();

        let result = tool
            .execute(json!({"action": "update", "task_id": "SNOW-1", "status": "executing"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("executing"));
    }

    #[tokio::test]
    async fn update_with_bare_number() {
        let (tool, _tmp) = setup();

        tool.execute(json!({"action": "create", "title": "Bare ID test"}))
            .await
            .unwrap();

        let result = tool
            .execute(json!({"action": "update", "task_id": "1", "status": "done"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("SNOW-1"));
    }

    #[tokio::test]
    async fn update_missing_task() {
        let (tool, _tmp) = setup();

        let result = tool
            .execute(json!({"action": "update", "task_id": "SNOW-999", "status": "done"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn list_with_filter() {
        let (tool, _tmp) = setup();

        tool.execute(json!({"action": "create", "title": "Task A"}))
            .await
            .unwrap();

        let result = tool
            .execute(json!({"action": "list", "filter_status": "executing"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No tasks found"));
    }
}
