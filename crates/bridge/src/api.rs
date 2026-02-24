use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use anyhow::{Result, Context};
use nostr_sdk::{EventId, PublicKey};
use tokio::net::TcpListener;
use tracing::{info, error, debug};
use std::sync::Arc;

use crate::bridge::BridgeState;

#[derive(Debug, Clone)]
pub struct ApiServer {
    bind_address: String,
}

#[derive(Debug, Deserialize)]
pub struct SendRequest {
    pub group: Option<String>,
    pub recipient: Option<String>,
    pub content: String,
    #[serde(default = "default_kind")]
    pub kind: u16,
}

#[derive(Debug, Serialize)]
pub struct SendResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub group: Option<String>,
    pub author: Option<String>,
    pub since: Option<i64>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

#[derive(Debug, Serialize)]
pub struct EventResponse {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u16,
    pub tags: serde_json::Value,
    pub content: String,
    pub sig: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decrypted_content: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EventsResponse {
    pub events: Vec<EventResponse>,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub cache: CacheStatsResponse,
    pub bridge: BridgeStatsResponse,
}

#[derive(Debug, Serialize)]
pub struct CacheStatsResponse {
    pub total_events: i64,
    pub by_kind: HashMap<u16, i64>,
    pub by_group: HashMap<String, i64>,
    pub recent_24h: i64,
}

#[derive(Debug, Serialize)]
pub struct BridgeStatsResponse {
    pub public_key: String,
    pub uptime: String,
    pub connected: bool,
    pub subscribed_groups: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub time: i64,
}

fn default_kind() -> u16 {
    9 // Group message by default
}

fn default_limit() -> i64 {
    50
}

impl ApiServer {
    pub fn new(bind_address: String) -> Self {
        Self { bind_address }
    }

    pub async fn start(&self, bridge_state: Arc<BridgeState>) -> Result<()> {
        let app = Router::new()
            .route("/send", post(handle_send))
            .route("/events", get(handle_events))
            .route("/events/:id", get(handle_event_by_id))
            .route("/stats", get(handle_stats))
            .route("/health", get(handle_health))
            .with_state(bridge_state);

        let listener = TcpListener::bind(&self.bind_address).await
            .with_context(|| format!("Failed to bind to {}", self.bind_address))?;

        info!("API server listening on {}", self.bind_address);
        
        axum::serve(listener, app).await
            .with_context(|| "API server error")?;

        Ok(())
    }
}

async fn handle_send(
    State(bridge): State<Arc<BridgeState>>,
    Json(request): Json<SendRequest>,
) -> Result<Json<SendResponse>, StatusCode> {
    debug!("Send request: {:?}", request);

    // Validate request
    if request.content.trim().is_empty() {
        return Ok(Json(SendResponse {
            success: false,
            event_id: None,
            error: Some("Content cannot be empty".to_string()),
        }));
    }

    let result = if request.kind == 4 {
        // Direct message
        let recipient_str = match request.recipient {
            Some(r) => r,
            None => {
                return Ok(Json(SendResponse {
                    success: false,
                    event_id: None,
                    error: Some("Recipient required for direct messages".to_string()),
                }));
            }
        };

        let recipient = match PublicKey::from_hex(&recipient_str) {
            Ok(pk) => pk,
            Err(_) => {
                return Ok(Json(SendResponse {
                    success: false,
                    event_id: None,
                    error: Some("Invalid recipient public key".to_string()),
                }));
            }
        };

        bridge.send_direct_message(&recipient, &request.content).await
    } else if request.kind == 9 {
        // Group message
        let group = match request.group {
            Some(g) => g,
            None => {
                return Ok(Json(SendResponse {
                    success: false,
                    event_id: None,
                    error: Some("Group required for group messages".to_string()),
                }));
            }
        };

        bridge.send_group_message(&group, &request.content).await
    } else {
        return Ok(Json(SendResponse {
            success: false,
            event_id: None,
            error: Some(format!("Unsupported message kind: {}", request.kind)),
        }));
    };

    match result {
        Ok(event_id) => Ok(Json(SendResponse {
            success: true,
            event_id: Some(event_id.to_hex()),
            error: None,
        })),
        Err(e) => {
            error!("Failed to send message: {}", e);
            Ok(Json(SendResponse {
                success: false,
                event_id: None,
                error: Some(e.to_string()),
            }))
        }
    }
}

async fn handle_events(
    State(bridge): State<Arc<BridgeState>>,
    Query(params): Query<EventsQuery>,
) -> Result<Json<EventsResponse>, StatusCode> {
    debug!("Events query: {:?}", params);

    let author_pubkey = if let Some(author_str) = &params.author {
        match PublicKey::from_hex(author_str) {
            Ok(pk) => Some(pk),
            Err(_) => {
                return Err(StatusCode::BAD_REQUEST);
            }
        }
    } else {
        None
    };

    match bridge.query_events(
        params.group.as_deref(),
        author_pubkey.as_ref(),
        params.since,
        Some(params.limit),
    ).await {
        Ok(cached_events) => {
            let mut events = Vec::new();
            
            for cached_event in cached_events {
                let mut event_response = EventResponse {
                    id: cached_event.id,
                    pubkey: cached_event.pubkey.clone(),
                    created_at: cached_event.created_at,
                    kind: cached_event.kind,
                    tags: serde_json::from_str(&cached_event.tags).unwrap_or_default(),
                    content: cached_event.content.clone(),
                    sig: cached_event.sig,
                    group_name: cached_event.group_name,
                    author_name: None,
                    decrypted_content: None,
                };

                // Get author name from profile cache
                if let Ok(pubkey) = PublicKey::from_hex(&cached_event.pubkey) {
                    event_response.author_name = Some(bridge.get_display_name(&pubkey).await);
                }

                // Decrypt DM content if it's a kind 4 event
                if cached_event.kind == 4 {
                    if let Ok(decrypted) = bridge.decrypt_dm_content(&cached_event.content, &cached_event.pubkey).await {
                        event_response.decrypted_content = Some(decrypted);
                    }
                }

                events.push(event_response);
            }

            Ok(Json(EventsResponse {
                count: events.len(),
                events,
            }))
        },
        Err(e) => {
            error!("Failed to query events: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn handle_event_by_id(
    State(bridge): State<Arc<BridgeState>>,
    Path(id): Path<String>,
) -> Result<Json<EventResponse>, StatusCode> {
    debug!("Get event by id: {}", id);

    let event_id = match EventId::from_hex(&id) {
        Ok(id) => id,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    match bridge.get_event(&event_id).await {
        Ok(Some(cached_event)) => {
            let mut event_response = EventResponse {
                id: cached_event.id,
                pubkey: cached_event.pubkey.clone(),
                created_at: cached_event.created_at,
                kind: cached_event.kind,
                tags: serde_json::from_str(&cached_event.tags).unwrap_or_default(),
                content: cached_event.content.clone(),
                sig: cached_event.sig,
                group_name: cached_event.group_name,
                author_name: None,
                decrypted_content: None,
            };

            // Get author name
            if let Ok(pubkey) = PublicKey::from_hex(&cached_event.pubkey) {
                event_response.author_name = Some(bridge.get_display_name(&pubkey).await);
            }

            // Decrypt DM content if it's a kind 4 event
            if cached_event.kind == 4 {
                if let Ok(decrypted) = bridge.decrypt_dm_content(&cached_event.content, &cached_event.pubkey).await {
                    event_response.decrypted_content = Some(decrypted);
                }
            }

            Ok(Json(event_response))
        },
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            error!("Failed to get event {}: {}", id, e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn handle_stats(
    State(bridge): State<Arc<BridgeState>>,
) -> Result<Json<StatsResponse>, StatusCode> {
    debug!("Stats request");

    match bridge.get_stats().await {
        Ok((cache_stats, uptime, connected, subscribed_groups, our_pubkey)) => {
            Ok(Json(StatsResponse {
                cache: CacheStatsResponse {
                    total_events: cache_stats.total_events,
                    by_kind: cache_stats.by_kind,
                    by_group: cache_stats.by_group,
                    recent_24h: cache_stats.recent_24h,
                },
                bridge: BridgeStatsResponse {
                    public_key: our_pubkey.to_hex(),
                    uptime: format!("{}s", uptime.as_secs()),
                    connected,
                    subscribed_groups: subscribed_groups.into_iter().collect(),
                },
            }))
        },
        Err(e) => {
            error!("Failed to get stats: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn handle_health(
    State(_bridge): State<Arc<BridgeState>>,
) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        time: chrono::Utc::now().timestamp(),
    })
}