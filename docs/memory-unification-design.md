# Snowclaw Memory Unification Design

## Status: DRAFT
## Date: 2025-07-22

---

## Problem

Two disconnected memory systems:

1. **Social Memory** (`channels/nostr_memory.rs` → `NostrMemory`)
   - In-memory HashMap + JSON file (`nostr_memory.json`)
   - Tracks: npub profiles, display names, notes, group membership, agent state
   - Injected via `build_context()` as string into prompts
   - No search, no relay persistence, no embeddings

2. **Agent Memory** (`memory/nostr_sqlite.rs` → `NostrSqliteMemory`)
   - NIP-78 kind 30078 → relay + SQLite FTS5 + vector embeddings
   - Accessed via `memory_store`/`memory_recall` tools
   - Relay is source of truth, SQLite is cache
   - Has semantic search — but knows nothing about social context

They don't talk to each other. Social knowledge isn't searchable. Agent memories aren't socially aware.

## Design Goals

1. **Nostr relay = source of truth** for all memory
2. **SQLite = local search cache** with semantic indexing (FTS5 + optional vectors)
3. **Nostr messages semantically indexed** but not necessarily all cached
4. **Keep upstream-compatible** — regular rebases from zeroclaw-labs/zeroclaw
5. **Memory files + arbitrary files indexable** (workspace docs, room files, etc.)

## Architecture

### Single Unified Backend: `NostrSqliteMemory` (extended)

Don't create a third system. Extend `NostrSqliteMemory` to handle social data too.

```
┌─────────────────────────────────────────────────┐
│                  Agent (prompt)                  │
│                                                  │
│  memory_recall ──→ NostrSqliteMemory.recall()    │
│  build_context ──→ NostrSqliteMemory.social()    │
│  file_index    ──→ NostrSqliteMemory.recall()    │
└──────────────────────┬──────────────────────────┘
                       │
              ┌────────▼────────┐
              │  SQLite brain.db │
              │                  │
              │  memories (core) │  ← agent memory_store
              │  indexed_docs    │  ← file content chunks
              │  messages_fts    │  ← nostr message index
              └────────┬────────┘
              ┌────────▼────────┐
              │  SQLite social.db│  ← separate DB for isolation
              │                  │
              │  social_npubs    │  ← profiles, notes, names
              │  social_groups   │  ← group context, members
              │  social_fts      │  ← FTS5 over social data
              └────────┬────────┘
                       │
              ┌────────▼────────┐
              │   Nostr Relay    │
              │                  │
              │  kind 30078      │  ← agent memories (existing)
              │  kind 30078      │  ← social data (new d-tag namespace)
              │  kind 9/11/etc   │  ← raw messages (already on relay)
              └─────────────────┘
```

### What Changes

#### 1. New SQLite Tables (brain.db + social.db)

```sql
-- Social profiles (replaces nostr_memory.json npubs)
-- NOTE: social tables live in social.db (not brain.db) for better isolation.
CREATE TABLE IF NOT EXISTS social_npubs (
    hex_pubkey TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    first_seen INTEGER NOT NULL,
    first_seen_group TEXT,
    last_interaction INTEGER NOT NULL,
    profile_json TEXT,          -- full kind 0 metadata
    name_history_json TEXT,     -- [(timestamp, name)]
    notes_json TEXT,            -- agent observations
    owner_notes_json TEXT,      -- owner-provided notes
    preferences_json TEXT,      -- JSON HashMap<String, String> (language, verbosity, etc.)
    is_owner INTEGER DEFAULT 0  -- whether this contact is an agent owner
);

-- Social groups (replaces nostr_memory.json groups)  
CREATE TABLE IF NOT EXISTS social_groups (
    group_id TEXT PRIMARY KEY,
    purpose TEXT,
    members_json TEXT,          -- [hex_pubkeys]
    notes_json TEXT,
    last_activity INTEGER NOT NULL
);

-- FTS index over social data (searchable via memory_recall)
CREATE VIRTUAL TABLE IF NOT EXISTS social_fts USING fts5(
    hex_pubkey,
    display_name,
    notes,
    owner_notes,
    content='social_npubs',
    content_rowid='rowid'
);

-- Indexed documents (workspace files, room files, etc.)
CREATE TABLE IF NOT EXISTS indexed_docs (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    chunk_index INTEGER NOT NULL DEFAULT 0,
    content TEXT NOT NULL,
    category TEXT NOT NULL DEFAULT 'document',
    updated_at TEXT NOT NULL,
    hash TEXT,                  -- content hash for change detection
    embedding BLOB              -- optional vector
);

-- FTS for documents
CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
    path, content,
    content='indexed_docs',
    content_rowid='rowid'
);

-- Message index (Nostr messages, not all cached but searchable)
CREATE TABLE IF NOT EXISTS message_index (
    event_id TEXT PRIMARY KEY,
    sender_hex TEXT NOT NULL,
    group_id TEXT,
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    indexed_at TEXT NOT NULL,
    embedding BLOB
);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content, sender_hex, group_id,
    content='message_index',
    content_rowid='rowid'
);
```

#### 2. Extended `NostrSqliteMemory` API

```rust
impl NostrSqliteMemory {
    // === Existing (unchanged) ===
    // store(), recall(), get(), list(), forget(), count()
    
    // === New: Social Memory ===
    
    /// Upsert an npub profile. Replaces NostrMemory::ensure_npub + update_profile
    pub async fn upsert_npub(&self, npub: &SocialNpub) -> Result<()>;
    
    /// Add a note to an npub (agent observation or owner note)
    pub async fn add_npub_note(&self, hex: &str, note: &str, is_owner: bool) -> Result<()>;
    
    /// Get npub data
    pub async fn get_npub(&self, hex: &str) -> Result<Option<SocialNpub>>;
    
    /// Upsert group data. Replaces NostrMemory::ensure_group
    pub async fn upsert_group(&self, group: &SocialGroup) -> Result<()>;
    
    /// Build context for prompt injection (replaces NostrMemory::build_context)
    pub async fn build_social_context(&self, sender_hex: &str, group_id: &str) -> String;
    
    // === New: Message Indexing ===
    
    /// Index a Nostr message for semantic search (selective, not all messages)
    pub async fn index_message(&self, event: &IndexableMessage) -> Result<()>;
    
    /// Search messages semantically
    pub async fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<MessageHit>>;
    
    // === New: Document Indexing ===
    
    /// Index a file's content (chunked, with change detection)
    pub async fn index_file(&self, path: &Path) -> Result<usize>; // returns chunk count
    
    /// Remove a file from the index
    pub async fn unindex_file(&self, path: &Path) -> Result<()>;
    
    /// Unified search across all memory types
    pub async fn unified_recall(&self, query: &str, limit: usize) -> Result<Vec<UnifiedHit>>;
}
```

#### 3. Relay Persistence for Social Data

Social data gets published to the relay using the existing NIP-78 pattern with a new d-tag namespace:

```
d-tag format:
  Agent memory:  snowclaw:core:<key>           (existing)
  Social npub:   snowclaw:memory:npub:<npub1...>   (bech32 npub, NOT hex)
  Social group:  snowclaw:memory:group:<group_id>
```

**Decision: bech32 npub in d-tags.** We use the full bech32 `npub1...` encoding in d-tags rather than hex pubkeys or truncated hex. This is human-readable, unambiguous, and avoids collision risk from truncation. Hex truncation is NOT used.

**Group-scoped memory events** must include an `h` tag with the group ID to enable relay-side filtering by group context:
```json
["h", "<group_id>"]
```

This means social data syncs across devices via the relay, just like agent memory already does.

#### 4. `NostrMemory` (channels/nostr_memory.rs) → Thin Wrapper

Don't delete `NostrMemory` immediately (upstream compat). Instead:

```rust
// Phase 1: NostrMemory becomes a facade over NostrSqliteMemory
pub struct NostrMemory {
    backend: Arc<NostrSqliteMemory>,  // shared with agent memory
    // Remove: store, persist_path, dirty
}

impl NostrMemory {
    pub fn new(backend: Arc<NostrSqliteMemory>) -> Self { ... }
    
    // All methods delegate to backend.upsert_npub(), backend.build_social_context(), etc.
    pub async fn ensure_npub(...) -> bool { self.backend.upsert_npub(...).await }
    pub async fn build_context(...) -> String { self.backend.build_social_context(...).await }
}
```

Phase 2 (later): Inline the facade, have `nostr.rs` call `NostrSqliteMemory` directly.

#### 5. Message Indexing Strategy

Not all messages get indexed — that would be noisy and expensive. Strategy:

```rust
/// Determines if a message should be semantically indexed
fn should_index_message(event: &Event, group_id: &str) -> IndexDecision {
    // Always index: messages mentioning the bot, DMs, messages with substantial content
    // Selectively index: messages from known contacts, messages in active conversations
    // Skip: reactions, very short messages, reposts, bot's own messages
}

enum IndexDecision {
    Index,           // Full index with embedding
    CacheOnly,       // Store in message_index but skip embedding (keyword search only)
    Skip,            // Don't store
}
```

#### 6. File Indexing

Room files, MEMORY.md, workspace docs — anything the agent should be able to recall:

```rust
// In config.toml:
[memory]
indexed_paths = [
    "rooms/*.md",
    "MEMORY.md",
    "TOOLS.md",
]
index_interval_minutes = 30  // re-index changed files periodically
```

Files are chunked (512 tokens), hashed for change detection, and stored in `indexed_docs`. On heartbeat or periodic timer, changed files get re-indexed.

### Upstream Compatibility

**Key constraint:** k0 does regular rebases from `zeroclaw-labs/zeroclaw`.

**Strategy: Additive changes only, feature-gated**

1. New tables are created with `IF NOT EXISTS` — no migration conflicts
2. New methods on `NostrSqliteMemory` are additive — existing API unchanged
3. `NostrMemory` facade preserves the existing interface
4. New config fields have defaults — missing fields = disabled
5. Feature gate: `#[cfg(feature = "social-memory")]` for new code paths if needed (but prefer runtime config)

**Rebase risk areas:**
- `memory/mod.rs` factory function (low risk — we just add a branch)
- `channels/nostr.rs` where `NostrMemory` is constructed (medium risk — constructor changes)
- `memory/traits.rs` if upstream adds methods (low risk — we extend, not modify)

**Mitigation:** Keep changes in separate files where possible. New files never conflict.

### New Files (no conflict risk)

```
src/memory/social.rs          — SocialNpub, SocialGroup structs + SQLite ops
src/memory/message_index.rs   — message indexing logic
src/memory/doc_index.rs       — file indexing logic  
src/memory/unified_search.rs  — cross-type search merger
```

### Modified Files (rebase risk, keep minimal)

```
src/memory/nostr_sqlite.rs    — add social/message/doc methods (or delegate to new modules)
src/memory/mod.rs              — wire up new modules
src/channels/nostr_memory.rs   — facade over NostrSqliteMemory (Phase 1)
src/channels/nostr.rs          — pass shared NostrSqliteMemory to NostrMemory
config/schema.rs               — new config fields
```

## Migration Path

1. **Phase 0:** Create new tables, new files. No behavior change yet. Ship it, rebase-safe.
2. **Phase 1:** Wire `NostrMemory` as facade → `NostrSqliteMemory`. Social data flows to SQLite + relay. `nostr_memory.json` still written for rollback safety.
3. **Phase 2:** Add message indexing. Agent can `memory_recall` and find relevant past conversations.
4. **Phase 3:** Add file indexing. Room files, workspace docs searchable.
5. **Phase 4:** Remove `nostr_memory.json` fallback. `NostrMemory` facade optional.

## Config Changes

```toml
[memory]
backend = "nostr"  # or "sqlite" — social features work with either

# Social memory (new)
social_memory = true           # enable social data in SQLite
social_relay_persist = true    # also publish social data to relay

# Message indexing (new)  
index_messages = true
index_messages_min_length = 20  # skip very short messages
index_messages_bot_mentions = true  # always index bot mentions

# Document indexing (new)
indexed_paths = ["rooms/*.md", "MEMORY.md"]
index_interval_minutes = 30
```

## Open Questions

1. **Embedding provider:** Current config has `embedding_provider = "none"`. Social search works with FTS5 alone, but vectors would improve recall quality. Worth enabling? Cost?
2. **Message retention:** How long to keep indexed messages? Should they expire? Follow `archive_after_days`/`purge_after_days`?
3. **Cross-agent memory:** If Zep and Snowclaw share a relay, should social memory be shared? (Probably yes — same `d`-tag namespace, different agent tags.)
4. **Room files vs social_groups:** Room files are markdown, groups are structured data. Keep both? Room files could become the "rendered view" of social_groups data.

## Claude Code Integration Note

For implementation, use Claude Code extensively with agent teams:
- **Implementer agent:** writes the Rust code
- **Validator agent:** reviews each module before merge, runs `cargo test`, checks upstream compat
- Break into small PRs per phase
