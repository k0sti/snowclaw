use super::traits::{Tool, ToolResult};
use crate::memory::{Memory, MemoryCategory};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use snow_memory::types::MemoryTier;
use std::sync::Arc;

/// Let the agent store memories — its own brain writes
pub struct MemoryStoreTool {
    memory: Arc<dyn Memory>,
    security: Arc<SecurityPolicy>,
}

impl MemoryStoreTool {
    pub fn new(memory: Arc<dyn Memory>, security: Arc<SecurityPolicy>) -> Self {
        Self { memory, security }
    }
}

#[async_trait]
impl Tool for MemoryStoreTool {
    fn name(&self) -> &str {
        "memory_store"
    }

    fn description(&self) -> &str {
        "Store a fact, preference, or note in long-term memory. Use category 'core' for permanent facts, 'daily' for session notes, 'conversation' for chat context, or a custom category name."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Unique key for this memory (e.g. 'core:timezone', 'pref:language', 'contact:a1b2c3')"
                },
                "content": {
                    "type": "string",
                    "description": "The information to remember"
                },
                "category": {
                    "type": "string",
                    "description": "Memory category: 'core' (permanent), 'daily' (session), 'conversation' (chat), or a custom category name. Defaults to 'core'."
                },
                "tier": {
                    "type": "string",
                    "description": "Privacy tier: 'public', 'private', or 'group:<id>'. If omitted, tier is inferred from the key scope prefix."
                }
            },
            "required": ["key", "content"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'key' parameter"))?;

        // Key validation
        if key.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Key must be non-empty".to_string()),
            });
        }
        if key.len() > 256 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Key too long ({} chars, max 256)", key.len())),
            });
        }

        // Validate scope prefix if key is scoped (contains ':')
        let mut scope_warning = None;
        if let Some(scope) = key.split(':').next() {
            if key.contains(':') {
                const KNOWN_SCOPES: &[&str] =
                    &["core", "pref", "contact", "conv", "group", "lesson"];
                if !KNOWN_SCOPES.contains(&scope) {
                    scope_warning =
                        Some(format!("Unknown scope prefix '{scope}' — memory stored but tier inference may be inaccurate"));
                }
            }
        }

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' parameter"))?;

        let category = match args.get("category").and_then(|v| v.as_str()) {
            Some("core") | None => MemoryCategory::Core,
            Some("daily") => MemoryCategory::Daily,
            Some("conversation") => MemoryCategory::Conversation,
            Some(other) => MemoryCategory::Custom(other.to_string()),
        };

        // Parse optional tier hint: "public", "private", or "group:<id>".
        // The collective backend determines the actual tier from the key scope prefix;
        // the tier parameter is advisory and logged for transparency.
        let tier_hint = args.get("tier").and_then(|v| v.as_str()).map(|t| match t {
            "public" => MemoryTier::Public,
            "private" => MemoryTier::Private("self".to_string()),
            s if s.starts_with("group:") => {
                MemoryTier::Group(s.strip_prefix("group:").unwrap_or("default").to_string())
            }
            _ => MemoryTier::Public,
        });

        if let Some(ref tier) = tier_hint {
            tracing::debug!("memory_store: tier hint = {tier:?} for key '{key}'");
        }

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "memory_store")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let store_result = if let Some(tier) = tier_hint {
            self.memory
                .store_with_tier(key, content, category, tier)
                .await
        } else {
            self.memory.store(key, content, category, None).await
        };

        match store_result {
            Ok(()) => {
                let mut output = format!("Stored memory: {key}");
                if let Some(warning) = scope_warning {
                    output.push_str(&format!("\nWarning: {warning}"));
                }
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to store memory: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::SqliteMemory;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use tempfile::TempDir;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn test_mem() -> (TempDir, Arc<dyn Memory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, Arc::new(mem))
    }

    #[test]
    fn name_and_schema() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryStoreTool::new(mem, test_security());
        assert_eq!(tool.name(), "memory_store");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["key"].is_object());
        assert!(schema["properties"]["content"].is_object());
    }

    #[tokio::test]
    async fn store_core() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryStoreTool::new(mem.clone(), test_security());
        let result = tool
            .execute(json!({"key": "lang", "content": "Prefers Rust"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("lang"));

        let entry = mem.get("lang").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "Prefers Rust");
    }

    #[tokio::test]
    async fn store_with_category() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryStoreTool::new(mem.clone(), test_security());
        let result = tool
            .execute(json!({"key": "note", "content": "Fixed bug", "category": "daily"}))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn store_with_custom_category() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryStoreTool::new(mem.clone(), test_security());
        let result = tool
            .execute(
                json!({"key": "proj_note", "content": "Uses async runtime", "category": "project"}),
            )
            .await
            .unwrap();
        assert!(result.success);

        let entry = mem.get("proj_note").await.unwrap().unwrap();
        assert_eq!(entry.content, "Uses async runtime");
        assert_eq!(entry.category, MemoryCategory::Custom("project".into()));
    }

    #[tokio::test]
    async fn store_missing_key() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryStoreTool::new(mem, test_security());
        let result = tool.execute(json!({"content": "no key"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn store_missing_content() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryStoreTool::new(mem, test_security());
        let result = tool.execute(json!({"key": "no_content"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn store_blocked_in_readonly_mode() {
        let (_tmp, mem) = test_mem();
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = MemoryStoreTool::new(mem.clone(), readonly);
        let result = tool
            .execute(json!({"key": "lang", "content": "Prefers Rust"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("read-only mode"));
        assert!(mem.get("lang").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn store_blocked_when_rate_limited() {
        let (_tmp, mem) = test_mem();
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = MemoryStoreTool::new(mem.clone(), limited);
        let result = tool
            .execute(json!({"key": "lang", "content": "Prefers Rust"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Rate limit exceeded"));
        assert!(mem.get("lang").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn store_rejects_empty_key() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryStoreTool::new(mem, test_security());
        let result = tool
            .execute(json!({"key": "", "content": "data"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("non-empty"));
    }

    #[tokio::test]
    async fn store_rejects_key_over_256_chars() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryStoreTool::new(mem, test_security());
        let long_key = "x".repeat(257);
        let result = tool
            .execute(json!({"key": long_key, "content": "data"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("too long"));
    }

    #[tokio::test]
    async fn store_accepts_valid_scoped_key() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryStoreTool::new(mem, test_security());
        let result = tool
            .execute(json!({"key": "core:timezone", "content": "UTC+3"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(!result.output.contains("Warning"));
    }

    #[tokio::test]
    async fn store_warns_on_unknown_scope() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryStoreTool::new(mem, test_security());
        let result = tool
            .execute(json!({"key": "weird:stuff", "content": "data"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Unknown scope"));
    }

    #[tokio::test]
    async fn store_with_tier_hint() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryStoreTool::new(mem.clone(), test_security());
        let result = tool
            .execute(json!({"key": "pref:lang", "content": "Rust", "tier": "private"}))
            .await
            .unwrap();
        assert!(result.success);
        let entry = mem.get("pref:lang").await.unwrap();
        assert!(entry.is_some());
    }
}
