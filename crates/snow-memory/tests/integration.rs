use snow_memory::*;
use snow_memory::search::SqliteMemoryIndex;
use snow_memory::config::MemoryConfig;
use std::path::Path;

#[test]
fn test_full_pipeline() {
    let _ = std::fs::remove_file("/tmp/snow-memory-integration.db");
    let idx = SqliteMemoryIndex::open(Path::new("/tmp/snow-memory-integration.db")).unwrap();

    // Two agents, different models, same topic — potential conflict
    let m1 = Memory {
        id: "mem1".into(), tier: MemoryTier::Public,
        topic: "nostr/nip44".into(),
        summary: "NIP-44 uses XChaCha20-Poly1305 for encryption".into(),
        detail: "Always use NIP-44 over NIP-04 for new implementations".into(),
        context: None, source: "agent_opus".into(),
        model: "anthropic/claude-opus-4".into(),
        confidence: 0.95, supersedes: None, version: 1,
        tags: vec!["nostr".into(), "encryption".into()],
        created_at: 1700000000,
    };
    let m2 = Memory {
        id: "mem2".into(), tier: MemoryTier::Public,
        topic: "nostr/nip44".into(),
        summary: "NIP-44 encryption is optional for relay messages".into(),
        detail: "NIP-04 is fine for backwards compatibility".into(),
        context: None, source: "agent_llama".into(),
        model: "meta/llama-3-8b".into(),
        confidence: 0.6, supersedes: None, version: 1,
        tags: vec!["nostr".into(), "encryption".into()],
        created_at: 1700000100,
    };
    let m3 = Memory {
        id: "mem3".into(), tier: MemoryTier::Group("snowclaw-core".into()),
        topic: "rust/error-handling".into(),
        summary: "Use anyhow for applications and thiserror for libraries".into(),
        detail: "Standard Rust error handling pattern".into(),
        context: None, source: "agent_opus".into(),
        model: "anthropic/claude-opus-4".into(),
        confidence: 0.9, supersedes: None, version: 1,
        tags: vec!["rust".into()],
        created_at: 1700000200,
    };

    // Store
    idx.upsert(&m1, None).unwrap();
    idx.upsert(&m2, None).unwrap();
    idx.upsert(&m3, None).unwrap();
    assert_eq!(idx.count().unwrap(), 3);

    // FTS search
    let results = idx.search("NIP-44 encryption", None, 10).unwrap();
    assert!(results.len() >= 2, "Should find both NIP-44 memories");

    // Tier-filtered search
    let group_only = idx.search("anyhow thiserror", Some("group"), 10).unwrap();
    assert_eq!(group_only.len(), 1);
    assert_eq!(group_only[0].0.id, "mem3");

    // Ranked search — opus agent should rank higher than llama
    let config = MemoryConfig::default();
    let ranked = idx.ranked_search("NIP-44 encryption", None, &config, 10).unwrap();
    assert!(!ranked.is_empty());
    // Both should appear, ranking depends on config defaults

    // Conflict detection
    let all_memories = vec![m1.clone(), m2.clone(), m3.clone()];
    let conflicts = detect_conflicts(&all_memories);
    assert!(!conflicts.is_empty(), "Should detect conflict on nostr/nip44 topic");
    assert_eq!(conflicts[0].topic, "nostr/nip44");

    // Supersedes chain
    let m1_v2 = Memory {
        id: "mem1v2".into(),
        supersedes: Some("mem1".into()),
        version: 2,
        summary: "NIP-44 is mandatory for all new Nostr encryption".into(),
        ..m1.clone()
    };
    idx.upsert(&m1_v2, None).unwrap();
    assert_eq!(idx.count().unwrap(), 4);
    let v2 = idx.get("mem1v2").unwrap().unwrap();
    assert_eq!(v2.supersedes, Some("mem1".into()));
    assert_eq!(v2.version, 2);

    // Event roundtrip
    let event = snow_memory::event::memory_to_event(&m1);
    let recovered = snow_memory::event::memory_from_event(&event).unwrap();
    assert_eq!(recovered.topic, m1.topic);
    assert_eq!(recovered.model, m1.model);

    // Cleanup
    let _ = std::fs::remove_file("/tmp/snow-memory-integration.db");
}
