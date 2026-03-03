//! Integration tests for collective memory v2: Phase 4 + 6.
//!
//! Tests scope-based tier classification, RecallContext filtering,
//! and relay round-trip (store → publish → sync → recall).
//!
//! Run with: `cargo test --test collective_memory_v2`
//! Relay tests: `cargo test --test collective_memory_v2 -- --ignored`

use snow_memory::types::MemoryTier;
use zeroclaw::memory::collective::{scope_to_tier, CollectiveMemory};
use zeroclaw::memory::snowclaw_ext::RecallContext;
use zeroclaw::memory::traits::{Memory, MemoryCategory};

// ── Scope-to-tier classification tests ────────────────────────────

#[test]
fn scope_to_tier_core_is_public() {
    assert_eq!(scope_to_tier("core:timezone"), MemoryTier::Public);
    assert_eq!(scope_to_tier("core:guardian"), MemoryTier::Public);
}

#[test]
fn scope_to_tier_lesson_is_public() {
    assert_eq!(scope_to_tier("lesson:a3f2b1"), MemoryTier::Public);
}

#[test]
fn scope_to_tier_pref_is_private() {
    match scope_to_tier("pref:language") {
        MemoryTier::Private(_) => {} // expected
        other => panic!("expected Private, got {other:?}"),
    }
}

#[test]
fn scope_to_tier_contact_is_private() {
    match scope_to_tier("contact:1zc6ts76") {
        MemoryTier::Private(_) => {}
        other => panic!("expected Private, got {other:?}"),
    }
}

#[test]
fn scope_to_tier_conv_is_private() {
    match scope_to_tier("conv:telegram:60996061:42") {
        MemoryTier::Private(_) => {}
        other => panic!("expected Private, got {other:?}"),
    }
}

#[test]
fn scope_to_tier_group_extracts_id() {
    match scope_to_tier("group:techteam:relay_config") {
        MemoryTier::Group(id) => assert_eq!(id, "techteam"),
        other => panic!("expected Group(techteam), got {other:?}"),
    }
}

#[test]
fn scope_to_tier_unscoped_defaults_to_public() {
    assert_eq!(scope_to_tier("some_legacy_key"), MemoryTier::Public);
}

// ── RecallContext filtering tests ─────────────────────────────────

fn test_config() -> zeroclaw::config::snowclaw_schema::CollectiveMemoryConfig {
    let mut cfg = zeroclaw::config::snowclaw_schema::CollectiveMemoryConfig::default();
    cfg.source_preferences.push(
        zeroclaw::config::snowclaw_schema::CollectiveSourceEntry {
            npub: Some("self".to_string()),
            group: None,
            trust: 1.0,
        },
    );
    cfg
}

#[tokio::test]
async fn recall_without_context_returns_all_tiers() {
    let cfg = test_config();
    let mem = CollectiveMemory::new_in_memory(&cfg).unwrap();

    // Store memories with a common keyword for FTS matching
    mem.store("core:fact", "Snowclaw tier test public", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("pref:lang", "Snowclaw tier test private", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("group:techteam:config", "Snowclaw tier test group", MemoryCategory::Core, None)
        .await
        .unwrap();

    // No context → no filtering → all returned
    let results = mem.recall("Snowclaw tier test", 10, None).await.unwrap();
    assert_eq!(results.len(), 3, "Without context, all tiers should be visible");
}

#[tokio::test]
async fn recall_main_session_sees_all_tiers() {
    let cfg = test_config();
    let mem = CollectiveMemory::new_in_memory(&cfg).unwrap();

    mem.store("core:fact", "Snowclaw session test public", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("pref:lang", "Snowclaw session test private", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("group:techteam:config", "Snowclaw session test group", MemoryCategory::Core, None)
        .await
        .unwrap();

    let ctx = RecallContext {
        is_main_session: true,
        channel: Some("nostr".into()),
        group_id: None,
    };

    let results = mem.recall_with_context("Snowclaw session test", 10, None, Some(&ctx)).await.unwrap();
    assert_eq!(results.len(), 3, "Main session should see all tiers");
}

#[tokio::test]
async fn recall_group_sees_public_and_matching_group() {
    let cfg = test_config();
    let mem = CollectiveMemory::new_in_memory(&cfg).unwrap();

    mem.store("core:fact", "Snowclaw grouptest public data", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("pref:lang", "Snowclaw grouptest private data", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("group:techteam:config", "Snowclaw grouptest techteam data", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("group:other:secret", "Snowclaw grouptest other data", MemoryCategory::Core, None)
        .await
        .unwrap();

    let ctx = RecallContext {
        is_main_session: false,
        channel: Some("nostr".into()),
        group_id: Some("techteam".into()),
    };

    let results = mem
        .recall_with_context("Snowclaw grouptest", 10, None, Some(&ctx))
        .await
        .unwrap();

    let keys: Vec<&str> = results.iter().map(|r| r.key.as_str()).collect();

    assert!(keys.contains(&"core:fact"), "Should see public memory");
    assert!(
        keys.contains(&"group:techteam:config"),
        "Should see matching group memory"
    );
    assert!(
        !keys.contains(&"pref:lang"),
        "Should NOT see private memory in group context"
    );
    assert!(
        !keys.contains(&"group:other:secret"),
        "Should NOT see other group's memory"
    );
}

#[tokio::test]
async fn recall_other_channel_sees_public_only() {
    let cfg = test_config();
    let mem = CollectiveMemory::new_in_memory(&cfg).unwrap();

    mem.store("core:fact", "Snowclaw chantest public info", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("pref:lang", "Snowclaw chantest private info", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("group:techteam:config", "Snowclaw chantest group info", MemoryCategory::Core, None)
        .await
        .unwrap();

    let ctx = RecallContext {
        is_main_session: false,
        channel: Some("telegram".into()),
        group_id: None,
    };

    let results = mem
        .recall_with_context("Snowclaw chantest", 10, None, Some(&ctx))
        .await
        .unwrap();

    let keys: Vec<&str> = results.iter().map(|r| r.key.as_str()).collect();

    assert!(keys.contains(&"core:fact"), "Should see public memory");
    assert!(
        !keys.contains(&"pref:lang"),
        "Should NOT see private memory"
    );
    assert!(
        !keys.contains(&"group:techteam:config"),
        "Should NOT see group memory without group_id"
    );
}

// ── Relay round-trip test (requires real relay) ───────────────────

#[tokio::test]
#[ignore] // requires real Nostr relay + nsec — run with --ignored
async fn relay_store_sync_roundtrip() {
    let nsec = read_nsec_from_config();
    let relay_url = "wss://zooid.atlantislabs.space".to_string();
    let test_prefix = format!("test:cmv2:{}", uuid::Uuid::new_v4().to_string()[..8].to_string());

    let mut cfg = test_config();
    cfg.relay_urls = vec![relay_url.clone()];

    let mem = CollectiveMemory::new_in_memory_with_relay(&cfg, &nsec).unwrap();
    mem.connect_relays().await;

    // Allow relay connection to establish
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Store with test-specific keys
    let key = format!("{test_prefix}:roundtrip");
    mem.store(&key, "Relay roundtrip test content", MemoryCategory::Core, None)
        .await
        .unwrap();

    // Wait for relay publish (fire-and-forget, needs time)
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // Create a second instance and sync from relay
    let mem2 = CollectiveMemory::new_in_memory_with_relay(&cfg, &nsec).unwrap();
    mem2.connect_relays().await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let synced = mem2.sync_from_relay().await.unwrap();
    assert!(synced > 0, "Should have synced at least one event from relay");

    // Verify the synced memory is retrievable
    let entry = mem2.get(&key).await.unwrap();
    assert!(
        entry.is_some(),
        "Synced memory should be retrievable by key"
    );
    let entry = entry.unwrap();
    assert!(entry.content.contains("Relay roundtrip test content"));
}

#[tokio::test]
#[ignore] // requires real Nostr relay + nsec — run with --ignored
async fn relay_tier_filtering_with_sync() {
    let nsec = read_nsec_from_config();
    let relay_url = "wss://zooid.atlantislabs.space".to_string();
    let test_prefix = format!("test:cmv2:{}", uuid::Uuid::new_v4().to_string()[..8].to_string());

    let mut cfg = test_config();
    cfg.relay_urls = vec![relay_url.clone()];

    let mem = CollectiveMemory::new_in_memory_with_relay(&cfg, &nsec).unwrap();
    mem.connect_relays().await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Store memories with different tiers (via key scope)
    mem.store(
        &format!("core:{test_prefix}:public_fact"),
        "This is public knowledge",
        MemoryCategory::Core,
        None,
    )
    .await
    .unwrap();

    mem.store(
        &format!("pref:{test_prefix}:private_pref"),
        "This is a private preference",
        MemoryCategory::Core,
        None,
    )
    .await
    .unwrap();

    mem.store(
        &format!("group:testgrp:{test_prefix}:group_note"),
        "This is group-specific",
        MemoryCategory::Core,
        None,
    )
    .await
    .unwrap();

    // Wait for relay publish
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // Sync into a fresh instance
    let mem2 = CollectiveMemory::new_in_memory_with_relay(&cfg, &nsec).unwrap();
    mem2.connect_relays().await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    mem2.sync_from_relay().await.unwrap();

    // Test filtering: main session sees all
    let ctx_main = RecallContext {
        is_main_session: true,
        channel: Some("nostr".into()),
        group_id: None,
    };
    let results = mem2
        .recall_with_context(&test_prefix, 20, None, Some(&ctx_main))
        .await
        .unwrap();
    assert!(
        results.len() >= 3,
        "Main session should see all 3 memories, got {}",
        results.len()
    );

    // Test filtering: group chat sees public + matching group only
    let ctx_group = RecallContext {
        is_main_session: false,
        channel: Some("nostr".into()),
        group_id: Some("testgrp".into()),
    };
    let results = mem2
        .recall_with_context(&test_prefix, 20, None, Some(&ctx_group))
        .await
        .unwrap();
    let keys: Vec<&str> = results.iter().map(|r| r.key.as_str()).collect();
    assert!(
        keys.iter().any(|k: &&str| k.starts_with("core:")),
        "Group context should see public"
    );
    assert!(
        keys.iter().any(|k: &&str| k.starts_with("group:testgrp:")),
        "Group context should see matching group"
    );
    assert!(
        !keys.iter().any(|k: &&str| k.starts_with("pref:")),
        "Group context should NOT see private"
    );

    // Test filtering: other channel sees public only
    let ctx_other = RecallContext {
        is_main_session: false,
        channel: Some("telegram".into()),
        group_id: None,
    };
    let results = mem2
        .recall_with_context(&test_prefix, 20, None, Some(&ctx_other))
        .await
        .unwrap();
    let keys: Vec<&str> = results.iter().map(|r| r.key.as_str()).collect();
    assert!(
        keys.iter().any(|k: &&str| k.starts_with("core:")),
        "Other channel should see public"
    );
    assert!(
        !keys.iter().any(|k: &&str| k.starts_with("pref:")),
        "Other channel should NOT see private"
    );
    assert!(
        !keys.iter().any(|k: &&str| k.starts_with("group:")),
        "Other channel should NOT see group"
    );
}

// ── Helpers ───────────────────────────────────────────────────────

/// Read the nsec from ~/.snowclaw/config.toml or SNOWCLAW_NSEC env var.
fn read_nsec_from_config() -> String {
    // Try env var first
    if let Ok(nsec) = std::env::var("SNOWCLAW_NSEC") {
        return nsec;
    }

    let home = std::env::var("HOME").expect("HOME env var");
    let config_path = std::path::PathBuf::from(home)
        .join(".snowclaw")
        .join("config.toml");
    let content =
        std::fs::read_to_string(&config_path).expect("Failed to read ~/.snowclaw/config.toml");
    let table: toml::Table = content.parse().expect("Failed to parse config.toml");

    // Navigate: [channels_config.nostr].nsec
    table
        .get("channels_config")
        .and_then(|v| v.get("nostr"))
        .and_then(|v| v.get("nsec"))
        .and_then(|v| v.as_str())
        .expect("No nsec found in config — set [channels_config.nostr].nsec or SNOWCLAW_NSEC env")
        .to_string()
}
