use serde::{Deserialize, Serialize};

/// Token usage information from a single API call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Model identifier (e.g., "anthropic/claude-sonnet-4-20250514")
    pub model: String,
    /// Input/prompt tokens
    pub input_tokens: u64,
    /// Output/completion tokens
    pub output_tokens: u64,
    /// Total tokens
    pub total_tokens: u64,
    /// Calculated cost in USD
    pub cost_usd: f64,
    /// Timestamp of the request
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Tokens read from prompt cache (e.g. Anthropic cache_read_input_tokens)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// Tokens written to prompt cache (e.g. Anthropic cache_creation_input_tokens)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
}

impl TokenUsage {
    fn sanitize_price(value: f64) -> f64 {
        if value.is_finite() && value > 0.0 {
            value
        } else {
            0.0
        }
    }

    /// Create a new token usage record.
    pub fn new(
        model: impl Into<String>,
        input_tokens: u64,
        output_tokens: u64,
        input_price_per_million: f64,
        output_price_per_million: f64,
    ) -> Self {
        Self::with_cache(
            model,
            input_tokens,
            output_tokens,
            input_price_per_million,
            output_price_per_million,
            None,
            None,
        )
    }

    /// Create a new token usage record with cache token counts.
    pub fn with_cache(
        model: impl Into<String>,
        input_tokens: u64,
        output_tokens: u64,
        input_price_per_million: f64,
        output_price_per_million: f64,
        cache_read_tokens: Option<u64>,
        cache_write_tokens: Option<u64>,
    ) -> Self {
        let model = model.into();
        let input_price_per_million = Self::sanitize_price(input_price_per_million);
        let output_price_per_million = Self::sanitize_price(output_price_per_million);
        let total_tokens = input_tokens.saturating_add(output_tokens);

        // Calculate cost: (tokens / 1M) * price_per_million
        let input_cost = (input_tokens as f64 / 1_000_000.0) * input_price_per_million;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * output_price_per_million;
        let cost_usd = input_cost + output_cost;

        Self {
            model,
            input_tokens,
            output_tokens,
            total_tokens,
            cost_usd,
            timestamp: chrono::Utc::now(),
            cache_read_tokens,
            cache_write_tokens,
        }
    }

    /// Get the total cost.
    pub fn cost(&self) -> f64 {
        self.cost_usd
    }
}

/// Breakdown of where tokens are spent, measured in bytes (~4 bytes per token).
///
/// Categorizes input/output into system prompt components, conversation context,
/// current turn content, and output sections. All fields default to 0 for
/// backwards compatibility with existing JSONL records.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenBreakdown {
    // ── System prompt components (bytes) ──
    /// Tool descriptions, hardware instructions
    #[serde(default)]
    pub tooling: u64,
    /// Guardrails, security rules
    #[serde(default)]
    pub safety: u64,
    /// Skill instructions
    #[serde(default)]
    pub skills: u64,
    /// AGENTS.md, SOUL.md, IDENTITY.md, USER.md content
    #[serde(default)]
    pub identity: u64,
    /// TOOLS.md, MEMORY.md, BOOTSTRAP.md content
    #[serde(default)]
    pub workspace_files: u64,
    /// Date/time, model info, workspace path, channel capabilities
    #[serde(default)]
    pub runtime: u64,

    // ── Conversation context (bytes) ──
    /// Prior assistant + user message turns
    #[serde(default)]
    pub conversation_history: u64,
    /// Tool call outputs from previous turns
    #[serde(default)]
    pub tool_results: u64,
    /// RAG/memory search results injected into context
    #[serde(default)]
    pub memory_context: u64,

    // ── Current turn (bytes) ──
    /// The incoming user message content
    #[serde(default)]
    pub user_message: u64,
    /// Group chat history, participant info, metadata envelope
    #[serde(default)]
    pub channel_context: u64,

    // ── Output (bytes) ──
    /// Reply text output tokens
    #[serde(default)]
    pub assistant_response: u64,
    /// Tool invocation output tokens
    #[serde(default)]
    pub tool_calls_output: u64,
    /// Reasoning/thinking tokens
    #[serde(default)]
    pub thinking: u64,
}

impl TokenBreakdown {
    /// Total estimated input bytes (system + context + current turn).
    pub fn total_input_bytes(&self) -> u64 {
        self.tooling
            + self.safety
            + self.skills
            + self.identity
            + self.workspace_files
            + self.runtime
            + self.conversation_history
            + self.tool_results
            + self.memory_context
            + self.user_message
            + self.channel_context
    }

    /// Total estimated output bytes.
    pub fn total_output_bytes(&self) -> u64 {
        self.assistant_response + self.tool_calls_output + self.thinking
    }
}

/// Breakdown of system prompt section sizes (bytes), returned alongside the prompt string.
#[derive(Debug, Clone, Default)]
pub struct PromptBreakdown {
    pub tooling: u64,
    pub safety: u64,
    pub skills: u64,
    pub identity: u64,
    pub workspace_files: u64,
    pub runtime: u64,
}

/// Time period for cost aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsagePeriod {
    Session,
    Day,
    Month,
}

/// A single cost record for persistent storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRecord {
    /// Unique identifier
    pub id: String,
    /// Token usage details
    pub usage: TokenUsage,
    /// Session identifier (for grouping)
    pub session_id: String,
    /// Channel name (e.g. "telegram", "nostr", "discord")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Room/chat/conversation identifier within the channel
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    /// Message type context (e.g. "user_message", "heartbeat", "gateway")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    /// Token breakdown by category
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<TokenBreakdown>,
}

impl CostRecord {
    /// Create a new cost record.
    pub fn new(session_id: impl Into<String>, usage: TokenUsage) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            usage,
            session_id: session_id.into(),
            channel: None,
            room: None,
            message_type: None,
            breakdown: None,
        }
    }

    /// Create a new cost record with channel context.
    pub fn with_context(
        session_id: impl Into<String>,
        usage: TokenUsage,
        channel: Option<String>,
        room: Option<String>,
        message_type: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            usage,
            session_id: session_id.into(),
            channel,
            room,
            message_type,
            breakdown: None,
        }
    }

    /// Create a new cost record with channel context and token breakdown.
    pub fn with_breakdown(
        session_id: impl Into<String>,
        usage: TokenUsage,
        channel: Option<String>,
        room: Option<String>,
        message_type: Option<String>,
        breakdown: Option<TokenBreakdown>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            usage,
            session_id: session_id.into(),
            channel,
            room,
            message_type,
            breakdown,
        }
    }
}

/// Budget enforcement result.
#[derive(Debug, Clone)]
pub enum BudgetCheck {
    /// Within budget, request can proceed
    Allowed,
    /// Warning threshold exceeded but request can proceed
    Warning {
        current_usd: f64,
        limit_usd: f64,
        period: UsagePeriod,
    },
    /// Budget exceeded, request blocked
    Exceeded {
        current_usd: f64,
        limit_usd: f64,
        period: UsagePeriod,
    },
}

/// Cost summary for reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostSummary {
    /// Total cost for the session
    pub session_cost_usd: f64,
    /// Total cost for the day
    pub daily_cost_usd: f64,
    /// Total cost for the month
    pub monthly_cost_usd: f64,
    /// Total tokens used
    pub total_tokens: u64,
    /// Number of requests
    pub request_count: usize,
    /// Breakdown by model
    pub by_model: std::collections::HashMap<String, ModelStats>,
}

/// Statistics for a specific model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStats {
    /// Model name
    pub model: String,
    /// Total cost for this model
    pub cost_usd: f64,
    /// Total tokens for this model
    pub total_tokens: u64,
    /// Number of requests for this model
    pub request_count: usize,
}

/// Usage breakdown for a specific time period with channel/room/model detail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageBreakdown {
    /// Total cost in USD
    pub total_cost_usd: f64,
    /// Total input tokens
    pub total_input_tokens: u64,
    /// Total output tokens
    pub total_output_tokens: u64,
    /// Total cache read tokens
    pub total_cache_read_tokens: u64,
    /// Total cache write tokens
    pub total_cache_write_tokens: u64,
    /// Number of requests
    pub request_count: usize,
    /// Breakdown by model
    pub by_model: std::collections::HashMap<String, ModelStats>,
    /// Breakdown by channel
    pub by_channel: std::collections::HashMap<String, ChannelStats>,
    /// Aggregated token breakdown across all records in the period
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_breakdown: Option<TokenBreakdown>,
}

/// Statistics for a specific channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStats {
    pub channel: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub request_count: usize,
    /// Per-room breakdown within this channel
    pub by_room: std::collections::HashMap<String, RoomStats>,
}

/// Statistics for a specific room/chat within a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomStats {
    pub room: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub request_count: usize,
}

impl Default for CostSummary {
    fn default() -> Self {
        Self {
            session_cost_usd: 0.0,
            daily_cost_usd: 0.0,
            monthly_cost_usd: 0.0,
            total_tokens: 0,
            request_count: 0,
            by_model: std::collections::HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_calculation() {
        let usage = TokenUsage::new("test/model", 1000, 500, 3.0, 15.0);

        // Expected: (1000/1M)*3 + (500/1M)*15 = 0.003 + 0.0075 = 0.0105
        assert!((usage.cost_usd - 0.0105).abs() < 0.0001);
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.total_tokens, 1500);
    }

    #[test]
    fn token_usage_zero_tokens() {
        let usage = TokenUsage::new("test/model", 0, 0, 3.0, 15.0);
        assert!(usage.cost_usd.abs() < f64::EPSILON);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn token_usage_negative_or_non_finite_prices_are_clamped() {
        let usage = TokenUsage::new("test/model", 1000, 1000, -3.0, f64::NAN);
        assert!(usage.cost_usd.abs() < f64::EPSILON);
        assert_eq!(usage.total_tokens, 2000);
    }

    #[test]
    fn cost_record_creation() {
        let usage = TokenUsage::new("test/model", 100, 50, 1.0, 2.0);
        let record = CostRecord::new("session-123", usage);

        assert_eq!(record.session_id, "session-123");
        assert!(!record.id.is_empty());
        assert_eq!(record.usage.model, "test/model");
    }
}
