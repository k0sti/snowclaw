use reqwest::Client;
use serde::{Serialize, Deserialize};
use anyhow::{Result, Context};
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error, debug};
use nostr_sdk::Event;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RETRIES: u32 = 3;
const BASE_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct WebhookDeliverer {
    client: Client,
    group_url: String,
    dm_url: Option<String>,
    token: Option<String>,
    preview_length: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub author: String,
    pub preview: String,
    pub event_id: String,
    pub created_at: i64,
}

impl WebhookDeliverer {
    pub fn new(
        group_url: String,
        dm_url: Option<String>,
        token: Option<String>,
        preview_length: usize,
    ) -> Self {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            group_url,
            dm_url,
            token,
            preview_length,
        }
    }

    pub async fn deliver_group_message(
        &self,
        event: &Event,
        group: &str,
        author_name: &str,
    ) -> Result<()> {
        let payload = WebhookPayload {
            r#type: "group_message".to_string(),
            group: Some(group.to_string()),
            author: author_name.to_string(),
            preview: self.create_preview(&event.content),
            event_id: event.id.to_hex(),
            created_at: event.created_at.as_secs() as i64,
        };

        self.deliver_payload(&self.group_url, &payload).await
            .with_context(|| format!("Failed to deliver group message for event {}", event.id))
    }

    pub async fn deliver_direct_message(
        &self,
        event: &Event,
        author_name: &str,
        decrypted_content: Option<&str>,
    ) -> Result<()> {
        let dm_url = match &self.dm_url {
            Some(url) => url,
            None => {
                debug!("No DM webhook URL configured, using group URL");
                &self.group_url
            }
        };

        // Use decrypted content for preview if available, otherwise use raw content
        let content_for_preview = decrypted_content.unwrap_or(&event.content);

        let payload = WebhookPayload {
            r#type: "direct_message".to_string(),
            group: None,
            author: author_name.to_string(),
            preview: self.create_preview(content_for_preview),
            event_id: event.id.to_hex(),
            created_at: event.created_at.as_secs() as i64,
        };

        self.deliver_payload(dm_url, &payload).await
            .with_context(|| format!("Failed to deliver direct message for event {}", event.id))
    }

    pub async fn test_webhook(&self) -> Result<()> {
        let test_payload = WebhookPayload {
            r#type: "test".to_string(),
            group: Some("test".to_string()),
            author: "bridge".to_string(),
            preview: "Test webhook connectivity".to_string(),
            event_id: "test".to_string(),
            created_at: chrono::Utc::now().timestamp(),
        };

        info!("Testing group webhook: {}", self.group_url);
        self.deliver_payload(&self.group_url, &test_payload).await
            .with_context(|| "Group webhook test failed")?;

        if let Some(dm_url) = &self.dm_url {
            info!("Testing DM webhook: {}", dm_url);
            let mut dm_test_payload = test_payload.clone();
            dm_test_payload.r#type = "test_dm".to_string();
            dm_test_payload.group = None;
            
            self.deliver_payload(dm_url, &dm_test_payload).await
                .with_context(|| "DM webhook test failed")?;
        }

        info!("All webhook tests passed");
        Ok(())
    }

    async fn deliver_payload(&self, url: &str, payload: &WebhookPayload) -> Result<()> {
        let mut attempt = 0;

        loop {
            attempt += 1;
            debug!("Webhook delivery attempt {} to {}", attempt, url);

            let mut request = self.client.post(url).json(payload);

            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }

            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    
                    if status.is_success() {
                        debug!("Webhook delivered successfully to {} (attempt {})", url, attempt);
                        return Ok(());
                    } else if status.is_client_error() {
                        // 4xx errors - don't retry
                        let error_text = response.text().await.unwrap_or_default();
                        error!("Webhook delivery failed with client error {}: {}", status, error_text);
                        return Err(anyhow::anyhow!(
                            "Webhook delivery failed with status {}: {}",
                            status,
                            error_text
                        ));
                    } else {
                        // 5xx errors - retry
                        let error_text = response.text().await.unwrap_or_default();
                        warn!(
                            "Webhook delivery failed with server error {} (attempt {}): {}",
                            status, attempt, error_text
                        );

                        if attempt >= MAX_RETRIES {
                            return Err(anyhow::anyhow!(
                                "Webhook delivery failed after {} attempts, last status: {}, error: {}",
                                MAX_RETRIES,
                                status,
                                error_text
                            ));
                        }
                    }
                }
                Err(e) => {
                    warn!("Webhook request error (attempt {}): {}", attempt, e);
                    
                    if attempt >= MAX_RETRIES {
                        return Err(anyhow::anyhow!(
                            "Webhook delivery failed after {} attempts: {}",
                            MAX_RETRIES,
                            e
                        ));
                    }
                }
            }

            // Wait before retry with exponential backoff
            let delay = BASE_RETRY_DELAY * attempt;
            debug!("Retrying webhook in {:?}", delay);
            sleep(delay).await;
        }
    }

    fn create_preview(&self, content: &str) -> String {
        let trimmed = content.trim();
        
        if trimmed.len() <= self.preview_length {
            trimmed.to_string()
        } else {
            // Try to break at word boundary near the limit
            let mut end = self.preview_length;
            
            // Look for a space within the last 20 characters
            if let Some(space_pos) = trimmed[..self.preview_length]
                .rfind(' ')
                .filter(|&pos| pos > self.preview_length.saturating_sub(20))
            {
                end = space_pos;
            }

            format!("{}...", &trimmed[..end])
        }
    }

    pub fn set_preview_length(&mut self, length: usize) {
        self.preview_length = length;
    }

    pub fn preview_length(&self) -> usize {
        self.preview_length
    }

    pub fn has_dm_url(&self) -> bool {
        self.dm_url.is_some()
    }

    /// Deliver group message from pre-extracted fields (no nostr_sdk types needed)
    pub async fn deliver_group_message_raw(
        &self, event_id: &str, group: &str, author: &str, preview: &str, created_at: i64,
    ) -> Result<()> {
        let payload = WebhookPayload {
            r#type: "group_message".to_string(),
            group: Some(group.to_string()),
            author: author.to_string(),
            preview: preview.to_string(),
            event_id: event_id.to_string(),
            created_at,
        };
        self.deliver_payload(&self.group_url, &payload).await
    }

    /// Deliver DM from pre-extracted fields
    pub async fn deliver_dm_raw(
        &self, event_id: &str, author: &str, preview: &str, created_at: i64,
    ) -> Result<()> {
        let url = self.dm_url.as_deref().unwrap_or(&self.group_url);
        let payload = WebhookPayload {
            r#type: "direct_message".to_string(),
            group: None,
            author: author.to_string(),
            preview: preview.to_string(),
            event_id: event_id.to_string(),
            created_at,
        };
        self.deliver_payload(url, &payload).await
    }
}