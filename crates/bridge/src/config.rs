use serde::{Deserialize, Serialize};
use anyhow::{Result, Context};
use std::fs;
use serde_json::Value;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub relay: RelayConfig,
    pub identity: IdentityConfig,
    #[serde(default)]
    pub groups: GroupsConfig,
    pub webhook: WebhookConfig,
    #[serde(default)]
    pub api: ApiConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RelayConfig {
    pub url: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct IdentityConfig {
    pub nsec_file: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct GroupsConfig {
    #[serde(default)]
    pub subscribe: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WebhookConfig {
    pub url: String,
    pub dm_url: Option<String>,
    pub token: Option<String>,
    #[serde(default = "default_preview_length")]
    pub preview_length: usize,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ApiConfig {
    #[serde(default = "default_bind_address")]
    pub bind: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CacheConfig {
    #[serde(default = "default_db_path")]
    pub db_path: String,
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            bind: default_bind_address(),
        }
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            retention_days: default_retention_days(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

fn default_preview_length() -> usize {
    100
}

fn default_bind_address() -> String {
    "127.0.0.1:3847".to_string()
}

fn default_db_path() -> String {
    "bridge.db".to_string()
}

fn default_retention_days() -> u32 {
    30
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone)]
pub struct Identity {
    pub nsec: String,
}

impl Config {
    pub fn load_from_file(path: &str) -> Result<Self> {
        let expanded_path = shellexpand::tilde(path);
        let content = fs::read_to_string(expanded_path.as_ref())
            .with_context(|| format!("Failed to read config file: {}", path))?;
        
        let mut config: Config = toml::from_str(&content)
            .with_context(|| "Failed to parse TOML config")?;

        // Apply environment variable fallback for webhook token
        if config.webhook.token.is_none() {
            if let Ok(token) = std::env::var("WEBHOOK_TOKEN") {
                config.webhook.token = Some(token);
            }
        }

        Ok(config)
    }

    pub fn load_identity(&self) -> Result<Identity> {
        let expanded_path = shellexpand::tilde(&self.identity.nsec_file);
        let content = fs::read_to_string(expanded_path.as_ref())
            .with_context(|| format!("Failed to read identity file: {}", self.identity.nsec_file))?;
        
        let json: Value = serde_json::from_str(&content)
            .with_context(|| "Failed to parse identity JSON")?;
        
        let nsec = json.get("nsec")
            .and_then(|v| v.as_str())
            .with_context(|| "Identity file must contain 'nsec' field")?;

        Ok(Identity {
            nsec: nsec.to_string(),
        })
    }

    pub fn validate(&self) -> Result<()> {
        // Validate relay URL
        if !self.relay.url.starts_with("wss://") && !self.relay.url.starts_with("ws://") {
            anyhow::bail!("Relay URL must start with ws:// or wss://");
        }

        // Validate webhook URL
        if !self.webhook.url.starts_with("http://") && !self.webhook.url.starts_with("https://") {
            anyhow::bail!("Webhook URL must start with http:// or https://");
        }

        // Validate DM webhook URL if provided
        if let Some(dm_url) = &self.webhook.dm_url {
            if !dm_url.starts_with("http://") && !dm_url.starts_with("https://") {
                anyhow::bail!("DM webhook URL must start with http:// or https://");
            }
        }

        // Validate bind address
        if self.api.bind.parse::<std::net::SocketAddr>().is_err() {
            anyhow::bail!("Invalid bind address: {}", self.api.bind);
        }

        Ok(())
    }

    pub fn expand_paths(&mut self) -> Result<()> {
        // Expand identity file path
        self.identity.nsec_file = shellexpand::tilde(&self.identity.nsec_file).to_string();
        
        // Expand database path
        self.cache.db_path = shellexpand::tilde(&self.cache.db_path).to_string();
        
        Ok(())
    }
}