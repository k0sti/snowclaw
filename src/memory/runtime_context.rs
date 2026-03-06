use serde::{Deserialize, Serialize};

/// Minimal Snowclaw-side runtime context for canonical Nomen visibility/scope decisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryRuntimeContext {
    pub visibility: MemoryVisibility,
    pub scope: String,
    pub channel: Option<String>,
}

impl MemoryRuntimeContext {
    pub fn new(
        visibility: MemoryVisibility,
        scope: impl Into<String>,
        channel: Option<impl Into<String>>,
    ) -> Self {
        Self {
            visibility,
            scope: scope.into(),
            channel: channel.map(Into::into),
        }
    }

    pub fn nomen_tier(&self) -> String {
        self.visibility.to_nomen_tier(&self.scope)
    }

    pub fn nomen_base_tier(&self) -> &'static str {
        self.visibility.as_nomen_visibility()
    }

    pub fn allowed_nomen_scopes(&self) -> Option<Vec<String>> {
        let scope = self.scope.trim();
        if scope.is_empty() {
            None
        } else {
            Some(vec![scope.to_string()])
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryVisibility {
    Public,
    Group,
    Circle,
    Personal,
    Internal,
}

impl MemoryVisibility {
    pub fn as_nomen_visibility(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Group => "group",
            Self::Circle => "circle",
            Self::Personal => "personal",
            Self::Internal => "internal",
        }
    }

    pub fn to_nomen_tier(self, scope: &str) -> String {
        let scope = scope.trim();
        match self {
            Self::Public => "public".to_string(),
            Self::Group | Self::Circle if !scope.is_empty() => {
                format!("{}:{scope}", self.as_nomen_visibility())
            }
            Self::Group => "group".to_string(),
            Self::Circle => "circle".to_string(),
            Self::Personal => "personal".to_string(),
            Self::Internal => "internal".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_context_uses_public_tier_without_scope_suffix() {
        let ctx = MemoryRuntimeContext::new(
            MemoryVisibility::Public,
            "",
            Some("telegram:-1003821690204:694"),
        );
        assert_eq!(ctx.nomen_tier(), "public");
        assert_eq!(ctx.allowed_nomen_scopes(), None);
    }

    #[test]
    fn group_context_preserves_scope_and_channel_separately() {
        let ctx = MemoryRuntimeContext::new(
            MemoryVisibility::Group,
            "techteam",
            Some("telegram:-1003821690204:694"),
        );
        assert_eq!(ctx.nomen_tier(), "group:techteam");
        assert_eq!(
            ctx.allowed_nomen_scopes(),
            Some(vec!["techteam".to_string()])
        );
        assert_eq!(ctx.channel.as_deref(), Some("telegram:-1003821690204:694"));
    }
}
