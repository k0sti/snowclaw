//! Nomen socket memory backend adapter.
//!
//! Bridges Snowclaw's `Memory` trait to Nomen via its Unix domain socket
//! wire protocol (`nomen-wire`), replacing the in-process SurrealDB dependency
//! with an out-of-process Nomen daemon.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    nomen_policy::resolve_store_policy,
    runtime_context::MemoryRuntimeContext,
    traits::{Memory, MemoryCategory, MemoryEntry},
};

/// Default socket path: `$XDG_RUNTIME_DIR/nomen/nomen.sock`.
fn default_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("nomen/nomen.sock");
    }
    let user = std::env::var("USER").unwrap_or_else(|_| std::process::id().to_string());
    PathBuf::from(format!("/tmp/nomen-{user}/nomen.sock"))
}

/// Memory backend that communicates with a Nomen daemon over a Unix socket.
pub struct NomenSocketMemory {
    client: nomen_wire::ReconnectingClient,
}

impl NomenSocketMemory {
    /// Create a new socket-backed Nomen memory adapter.
    ///
    /// `socket_path` defaults to `$XDG_RUNTIME_DIR/nomen/nomen.sock` when `None`.
    pub fn new(socket_path: Option<&Path>) -> Self {
        let path = socket_path
            .map(PathBuf::from)
            .unwrap_or_else(default_socket_path);
        tracing::info!("Nomen socket memory adapter: {}", path.display());
        Self {
            client: nomen_wire::ReconnectingClient::new(path, 3),
        }
    }

    /// Store with optional explicit runtime context for visibility/scope.
    pub async fn store_with_context(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        ctx: Option<&MemoryRuntimeContext>,
    ) -> anyhow::Result<()> {
        let summary: String = content
            .lines()
            .next()
            .unwrap_or(content)
            .chars()
            .take(200)
            .collect();
        let policy = resolve_store_policy(&category, session_id);
        let tier = ctx
            .map(MemoryRuntimeContext::nomen_tier)
            .unwrap_or_else(|| policy.tier.clone());

        // Derive visibility/scope from tier string.
        let (visibility, scope) = split_tier(&tier);

        if ctx.is_none() {
            if let Some(limitation) = policy.limitation {
                tracing::debug!(
                    key,
                    category = %category,
                    session_id,
                    tier = %policy.tier,
                    channel_hint = ?policy.channel_hint,
                    limitation,
                    "Nomen socket adapter fell back to compatibility store policy"
                );
            }
        } else {
            tracing::debug!(
                key,
                category = %category,
                session_id,
                tier = %tier,
                channel = ctx.and_then(|c| c.channel.as_deref()),
                "Nomen socket adapter using explicit runtime memory context"
            );
        }

        let mut params = json!({
            "topic": key,
            "summary": summary,
            "detail": content,
            "confidence": 0.8,
            "visibility": visibility,
        });
        if !scope.is_empty() {
            params["scope"] = Value::String(scope.to_string());
        }

        let resp = self.client.request("memory.put", params).await?;
        if !resp.ok {
            let msg = resp
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into());
            anyhow::bail!("memory.put failed: {msg}");
        }
        Ok(())
    }

    /// Recall with optional explicit runtime context.
    pub async fn recall_with_context(
        &self,
        query: &str,
        limit: usize,
        _session_id: Option<&str>,
        ctx: Option<&MemoryRuntimeContext>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let mut params = json!({
            "query": query,
            "limit": limit,
        });
        if let Some(ctx) = ctx {
            params["visibility"] = Value::String(ctx.nomen_base_tier().to_string());
            if let Some(scopes) = ctx.allowed_nomen_scopes() {
                if let Some(scope) = scopes.first() {
                    params["scope"] = Value::String(scope.clone());
                }
            }
        }

        let resp = self.client.request("memory.search", params).await?;
        if !resp.ok {
            let msg = resp
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into());
            anyhow::bail!("memory.search failed: {msg}");
        }

        let result = resp.result.unwrap_or(json!({"results": []}));
        parse_search_results(&result)
    }

    /// List with optional explicit runtime context.
    pub async fn list_with_context(
        &self,
        category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
        ctx: Option<&MemoryRuntimeContext>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let mut params = json!({"limit": 1000});

        if let Some(ctx) = ctx {
            params["visibility"] = Value::String(ctx.nomen_base_tier().to_string());
            let scope = ctx.scope.trim();
            if !scope.is_empty() {
                params["scope"] = Value::String(scope.to_string());
            }
        } else if let Some(cat) = category {
            let policy = resolve_store_policy(cat, None);
            params["visibility"] = Value::String(policy.tier);
        }

        let resp = self.client.request("memory.list", params).await?;
        if !resp.ok {
            let msg = resp
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into());
            anyhow::bail!("memory.list failed: {msg}");
        }

        let result = resp.result.unwrap_or(json!({"memories": []}));
        parse_list_results(&result)
    }
}

/// Split a Nomen tier string like "group:techteam" into ("group", "techteam").
fn split_tier(tier: &str) -> (&str, &str) {
    match tier.split_once(':') {
        Some((vis, scope)) => (vis, scope),
        None => (tier, ""),
    }
}

/// Map a Nomen visibility string back to a Snowclaw `MemoryCategory`.
fn tier_to_category(visibility: &str) -> MemoryCategory {
    match visibility {
        "personal" | "internal" => MemoryCategory::Core,
        other => MemoryCategory::Custom(other.to_string()),
    }
}

fn parse_search_results(result: &Value) -> anyhow::Result<Vec<MemoryEntry>> {
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(results
        .iter()
        .map(|r| {
            let topic = r["topic"].as_str().unwrap_or_default();
            let summary = r["summary"].as_str().unwrap_or_default();
            let detail = r["detail"].as_str().unwrap_or_default();
            let visibility = r["visibility"].as_str().unwrap_or("personal");
            let scope = r["scope"].as_str().unwrap_or_default();
            let confidence = r["confidence"].as_f64();
            let created_at = r["created_at"].as_str().unwrap_or_default();
            let d_tag = r["d_tag"].as_str().unwrap_or_default();

            let tier = if scope.is_empty() {
                visibility.to_string()
            } else {
                format!("{visibility}:{scope}")
            };

            MemoryEntry {
                id: d_tag.to_string(),
                key: topic.to_string(),
                content: if detail.is_empty() {
                    summary.to_string()
                } else {
                    detail.to_string()
                },
                category: tier_to_category(&tier),
                timestamp: created_at.to_string(),
                session_id: None,
                score: confidence,
            }
        })
        .collect())
}

fn parse_list_results(result: &Value) -> anyhow::Result<Vec<MemoryEntry>> {
    let memories = result
        .get("memories")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(memories
        .iter()
        .map(|r| {
            let topic = r["topic"].as_str().unwrap_or_default();
            let summary = r["summary"].as_str().unwrap_or_default();
            let content = r["content"].as_str().unwrap_or(summary);
            let visibility = r["visibility"].as_str().unwrap_or("personal");
            let scope = r["scope"].as_str().unwrap_or_default();
            let confidence = r["confidence"].as_f64();
            let created_at = r["created_at"].as_str().unwrap_or_default();
            let d_tag = r["d_tag"].as_str().unwrap_or_default();

            let tier = if scope.is_empty() {
                visibility.to_string()
            } else {
                format!("{visibility}:{scope}")
            };

            MemoryEntry {
                id: d_tag.to_string(),
                key: topic.to_string(),
                content: if summary.is_empty() {
                    content.to_string()
                } else {
                    summary.to_string()
                },
                category: tier_to_category(&tier),
                timestamp: created_at.to_string(),
                session_id: None,
                score: confidence,
            }
        })
        .collect())
}

#[async_trait]
impl Memory for NomenSocketMemory {
    fn name(&self) -> &str {
        "nomen-socket"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.store_with_context(key, content, category, session_id, None)
            .await
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.recall_with_context(query, limit, session_id, None)
            .await
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let resp = self
            .client
            .request("memory.get", json!({"topic": key}))
            .await?;
        if !resp.ok {
            let msg = resp
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into());
            anyhow::bail!("memory.get failed: {msg}");
        }

        let result = match resp.result {
            Some(v) if !v.is_null() => v,
            _ => return Ok(None),
        };

        // Single-record response — parse directly.
        let topic = result["topic"].as_str().unwrap_or_default();
        if topic.is_empty() {
            return Ok(None);
        }

        let summary = result["summary"].as_str().unwrap_or_default();
        let content = result["content"].as_str().unwrap_or(summary);
        let visibility = result["visibility"].as_str().unwrap_or("personal");
        let scope = result["scope"].as_str().unwrap_or_default();
        let confidence = result["confidence"].as_f64();
        let created_at = result["created_at"].as_str().unwrap_or_default();
        let d_tag = result["d_tag"].as_str().unwrap_or_default();

        let tier = if scope.is_empty() {
            visibility.to_string()
        } else {
            format!("{visibility}:{scope}")
        };

        Ok(Some(MemoryEntry {
            id: d_tag.to_string(),
            key: topic.to_string(),
            content: if summary.is_empty() {
                content.to_string()
            } else {
                summary.to_string()
            },
            category: tier_to_category(&tier),
            timestamp: created_at.to_string(),
            session_id: None,
            score: confidence,
        }))
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.list_with_context(category, session_id, None).await
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let resp = self
            .client
            .request("memory.delete", json!({"topic": key}))
            .await?;
        if !resp.ok {
            let msg = resp
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into());
            anyhow::bail!("memory.delete failed: {msg}");
        }

        let deleted = resp
            .result
            .as_ref()
            .and_then(|r| r["deleted"].as_bool())
            .unwrap_or(false);
        Ok(deleted)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let resp = self
            .client
            .request("memory.list", json!({"limit": 0, "stats": true}))
            .await?;
        if !resp.ok {
            let msg = resp
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into());
            anyhow::bail!("memory.list (count) failed: {msg}");
        }

        let count = resp
            .result
            .as_ref()
            .and_then(|r| r["count"].as_u64())
            .unwrap_or(0) as usize;
        Ok(count)
    }

    async fn health_check(&self) -> bool {
        self.client
            .request("memory.list", json!({"limit": 0, "stats": true}))
            .await
            .map(|r| r.ok)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_tier_handles_scoped_and_unscoped() {
        assert_eq!(split_tier("personal"), ("personal", ""));
        assert_eq!(split_tier("group:techteam"), ("group", "techteam"));
        assert_eq!(split_tier("circle:inner"), ("circle", "inner"));
    }

    #[test]
    fn tier_to_category_maps_personal_to_core() {
        assert_eq!(tier_to_category("personal"), MemoryCategory::Core);
        assert_eq!(tier_to_category("internal"), MemoryCategory::Core);
        assert_eq!(
            tier_to_category("public"),
            MemoryCategory::Custom("public".into())
        );
    }

    #[test]
    fn parse_search_results_handles_empty() {
        let result = json!({"count": 0, "results": []});
        let entries = parse_search_results(&result).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_search_results_maps_fields() {
        let result = json!({
            "count": 1,
            "results": [{
                "topic": "favorite_lang",
                "summary": "Rust is great",
                "detail": "Rust is a systems language",
                "visibility": "personal",
                "scope": "",
                "confidence": 0.95,
                "created_at": "2026-03-13T00:00:00Z",
                "d_tag": "dtag-1"
            }]
        });
        let entries = parse_search_results(&result).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "favorite_lang");
        assert_eq!(entries[0].content, "Rust is a systems language");
        assert_eq!(entries[0].category, MemoryCategory::Core);
        assert_eq!(entries[0].score, Some(0.95));
        assert_eq!(entries[0].id, "dtag-1");
    }

    #[test]
    fn parse_list_results_handles_empty() {
        let result = json!({"count": 0, "memories": []});
        let entries = parse_list_results(&result).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn nomen_socket_memory_reports_correct_name() {
        let mem = NomenSocketMemory::new(Some(Path::new("/tmp/test-nomen.sock")));
        assert_eq!(mem.name(), "nomen-socket");
    }

    #[test]
    fn default_socket_path_is_reasonable() {
        let path = default_socket_path();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("nomen") || path_str.ends_with("nomen.sock"),
            "default path should reference nomen: {path_str}"
        );
    }
}
