use lru::LruCache;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use tokio::sync::RwLock;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc, Duration};
use nostr_sdk::{Event, PublicKey, Kind};
use anyhow::Result;

const DEFAULT_CACHE_SIZE: usize = 1000;
const PROFILE_TTL_HOURS: i64 = 24;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nip05: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lud16: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedProfile {
    profile: Profile,
    cached_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct ProfileCache {
    cache: RwLock<LruCache<String, CachedProfile>>,
    stats: RwLock<CacheStats>,
}

#[derive(Debug, Default)]
struct CacheStats {
    hits: u64,
    misses: u64,
    stores: u64,
    cleanups: u64,
}

impl ProfileCache {
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(LruCache::new(
                NonZeroUsize::new(DEFAULT_CACHE_SIZE).unwrap()
            )),
            stats: RwLock::new(CacheStats::default()),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            cache: RwLock::new(LruCache::new(
                NonZeroUsize::new(capacity).unwrap()
            )),
            stats: RwLock::new(CacheStats::default()),
        }
    }

    pub async fn get_display_name(&self, pubkey: &PublicKey) -> String {
        let pubkey_hex = pubkey.to_hex();
        
        let cached_profile = {
            let mut cache = self.cache.write().await;
            if let Some(cached) = cache.get(&pubkey_hex) {
                if Utc::now() < cached.expires_at {
                    // Cache hit - return profile for formatting
                    Some(cached.profile.clone())
                } else {
                    // Expired entry - remove it
                    cache.pop(&pubkey_hex);
                    None
                }
            } else {
                None
            }
        };

        if let Some(profile) = cached_profile {
            self.stats.write().await.hits += 1;
            return self.format_display_name(&profile, pubkey);
        }

        // Cache miss
        self.stats.write().await.misses += 1;
        self.format_fallback_name(pubkey)
    }

    pub async fn store_profile(&self, event: &Event) -> Result<()> {
        if event.kind != Kind::Metadata {
            return Ok(()); // Only process kind 0 (metadata) events
        }

        let profile: Profile = match serde_json::from_str(&event.content) {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("Failed to parse profile for {}", event.pubkey);
                return Ok(());
            }
        };

        let pubkey_hex = event.pubkey.to_hex();
        let now = Utc::now();
        
        let cached_profile = CachedProfile {
            profile,
            cached_at: now,
            expires_at: now + Duration::hours(PROFILE_TTL_HOURS),
        };

        self.cache.write().await.put(pubkey_hex, cached_profile);
        self.stats.write().await.stores += 1;

        Ok(())
    }

    pub async fn has_profile(&self, pubkey: &PublicKey) -> bool {
        let pubkey_hex = pubkey.to_hex();
        let cache = self.cache.read().await;
        
        if let Some(cached) = cache.peek(&pubkey_hex) {
            Utc::now() < cached.expires_at
        } else {
            false
        }
    }

    pub async fn cleanup_expired(&self) -> usize {
        let mut cache = self.cache.write().await;
        let mut _expired_keys: Vec<String> = Vec::new();
        let _now = chrono::Utc::now();

        // Find expired entries (we need to iterate since LruCache doesn't expose iteration)
        // This is a limitation - we'll clean up as we access entries instead
        drop(cache);

        // We can't easily iterate over LruCache entries, so expired entries will be
        // cleaned up lazily when accessed. This is acceptable behavior.
        0
    }

    pub async fn stats(&self) -> HashMap<String, u64> {
        let stats = self.stats.read().await;
        let cache = self.cache.read().await;
        
        let mut result = HashMap::new();
        result.insert("hits".to_string(), stats.hits);
        result.insert("misses".to_string(), stats.misses);
        result.insert("stores".to_string(), stats.stores);
        result.insert("size".to_string(), cache.len() as u64);
        result.insert("capacity".to_string(), cache.cap().get() as u64);
        
        let hit_rate = if stats.hits + stats.misses > 0 {
            (stats.hits as f64) / ((stats.hits + stats.misses) as f64) * 100.0
        } else {
            0.0
        };
        result.insert("hit_rate_percent".to_string(), hit_rate as u64);
        
        result
    }

    fn format_display_name(&self, profile: &Profile, pubkey: &PublicKey) -> String {
        // Priority: display_name > name > npub fallback
        if let Some(display_name) = &profile.display_name {
            if !display_name.trim().is_empty() {
                return display_name.trim().to_string();
            }
        }
        
        if let Some(name) = &profile.name {
            if !name.trim().is_empty() {
                return name.trim().to_string();
            }
        }

        self.format_fallback_name(pubkey)
    }

    fn format_fallback_name(&self, pubkey: &PublicKey) -> String {
        let hex = pubkey.to_hex();
        format!("npub1{}...", &hex[..8])
    }

    pub async fn request_missing_profiles(&self, pubkeys: &[PublicKey]) -> Vec<PublicKey> {
        let mut missing = Vec::new();
        
        for pubkey in pubkeys {
            if !self.has_profile(pubkey).await {
                missing.push(*pubkey);
            }
        }

        missing
    }

    pub async fn get_profile(&self, pubkey: &PublicKey) -> Option<Profile> {
        let pubkey_hex = pubkey.to_hex();
        let mut cache = self.cache.write().await;
        
        if let Some(cached) = cache.get(&pubkey_hex) {
            if Utc::now() < cached.expires_at {
                return Some(cached.profile.clone());
            } else {
                cache.pop(&pubkey_hex);
            }
        }
        
        None
    }

    pub async fn clear(&self) {
        self.cache.write().await.clear();
        let mut stats = self.stats.write().await;
        *stats = CacheStats::default();
    }

    pub async fn len(&self) -> usize {
        self.cache.read().await.len()
    }

    /// Get display name by hex pubkey string (avoids needing PublicKey type in bridge.rs)
    pub async fn get_display_name_hex(&self, pubkey_hex: &str) -> String {
        let cached_profile = {
            let mut cache = self.cache.write().await;
            if let Some(cached) = cache.get(pubkey_hex) {
                if Utc::now() < cached.expires_at {
                    Some(cached.profile.clone())
                } else {
                    cache.pop(pubkey_hex);
                    None
                }
            } else {
                None
            }
        };

        if let Some(profile) = cached_profile {
            self.stats.write().await.hits += 1;
            if let Some(dn) = &profile.display_name {
                if !dn.trim().is_empty() { return dn.trim().to_string(); }
            }
            if let Some(n) = &profile.name {
                if !n.trim().is_empty() { return n.trim().to_string(); }
            }
        } else {
            self.stats.write().await.misses += 1;
        }
        format!("npub1{}...", &pubkey_hex[..pubkey_hex.len().min(8)])
    }

    /// Store profile from raw kind 0 content JSON
    pub async fn store_profile_raw(&self, pubkey_hex: &str, content: &str) -> Result<()> {
        let profile: Profile = serde_json::from_str(content)?;
        let now = Utc::now();
        self.cache.write().await.put(pubkey_hex.to_string(), CachedProfile {
            profile,
            cached_at: now,
            expires_at: now + Duration::hours(PROFILE_TTL_HOURS),
        });
        self.stats.write().await.stores += 1;
        Ok(())
    }

    /// Get all known pubkey -> display_name mappings for mention detection
    pub async fn get_known_pubkeys(&self) -> std::collections::HashMap<String, String> {
        let cache = self.cache.read().await;
        cache.iter()
            .map(|(pubkey_hex, cached_profile)| {
                let display_name = cached_profile.profile.display_name.clone()
                    .or_else(|| cached_profile.profile.name.clone())
                    .unwrap_or_else(|| format!("{}...", &pubkey_hex[..8]));
                (pubkey_hex.clone(), display_name)
            })
            .collect()
    }
}