# Memory Unification — Summary

**Date:** 2025-07-22
**Status:** Phase 0-4 Complete

## What Was Built

Unified Snowclaw's two disconnected memory systems (social HashMap + agent SQLite) into a single SQLite-backed architecture with the Nostr relay as eventual source of truth.

```
┌──────────────────────────────────────────┐
│            NostrMemory (facade)           │
│                                          │
│  Social CRUD ──→ social.rs ──→ SQLite    │
│  Message index ─→ message_index.rs ──→   │
│  Doc index ────→ doc_index.rs ──→        │
│  Unified search → unified_search.rs ──→  │
│                                          │
│            social.db (single file)        │
│  ┌────────────┬──────────────┬────────┐  │
│  │social_npubs│message_index │indexed │  │
│  │social_groups│messages_fts │_docs   │  │
│  │social_fts  │             │docs_fts│  │
│  └────────────┴──────────────┴────────┘  │
└──────────────────────────────────────────┘
```

## Files Created (7)

| File | Size | Purpose |
|------|------|---------|
| `src/memory/social.rs` | 26KB | Social npub/group SQLite CRUD + FTS5 |
| `src/memory/message_index.rs` | 15KB | Selective message indexing + search |
| `src/memory/doc_index.rs` | 18KB | File chunking + hash change detection |
| `src/memory/unified_search.rs` | 17KB | Cross-type search merger |
| `src/memory/file_indexer.rs` | 9KB | Periodic file indexing service |
| `docs/memory-unification-design.md` | 12KB | Architecture design doc |
| `docs/review-phase-0-1.md` | — | Validator review |

## Files Modified (6)

| File | Changes | Risk |
|------|---------|------|
| `src/channels/nostr_memory.rs` | Rewritten as SQLite facade | Low (snowclaw-only) |
| `src/channels/nostr.rs` | Wiring (~100 lines) | Low |
| `src/memory/mod.rs` | 5 pub mod lines | Trivial |
| `src/config/schema.rs` | 2 new fields | Trivial |
| `src/channels/mod.rs` | 2 lines | Trivial |
| `src/onboard/wizard.rs` | 2 lines | Trivial |

## Test Count

- **Phase 0:** 314 memory tests (base + 87 new)
- **Phase 1:** 319 (+5 facade tests)
- **Phase 2:** 329 (+10 message indexing)
- **Phase 3:** 341 (+12 file indexing + unified)
- **Phase 4:** 341 (cleanup, no new tests needed)
- **Pre-existing failure:** `native_storage_path_contains_zeroclaw` (unrelated)

## Timing

| Phase | Duration | What |
|-------|----------|------|
| 0 | 18 min | 4 new modules + schema |
| 1 | 7 min | NostrMemory facade |
| Fixes | 6 min | FTS5 escaping, missing fields |
| 2 | 8 min | Message indexing wired |
| 3 | 9 min | File indexing + unified search |
| 4 | ~12 min | Cleanup (agent killed, compiles clean) |
| **Total** | **~60 min** | |

## What's NOT Done (Future Work)

1. **Relay persistence for social data** — Social memory is SQLite-only. Needs NIP-78 publish with `snowclaw:memory:npub:<npub1...>` d-tags and `h` group scoping tags.
2. **memory_recall tool integration** — Agent memory (brain.db) and social memory (social.db) are separate databases. `memory_recall` only searches brain.db. Options: ATTACH database, dedicated `social_search` tool, or merge into single DB.
3. **Periodic file re-indexing** — FileIndexer exists but needs to be called from heartbeat/cron timer.
4. **NIP-44 encrypted memory** — Private memory entries encrypted to agent's own pubkey.
5. **Kind 4129 Agent Lessons** — Behavioral learnings published to relay, not implemented.

## Config

```toml
[memory]
# File indexing (new)
indexed_paths = ["rooms/*.md", "MEMORY.md"]
index_interval_minutes = 30
```

## Spec Alignment

- D-tags use bech32 npub (not hex) per updated spec
- social_npubs has preferences_json + is_owner per spec
- themes/decisions on groups deferred (generic notes sufficient)
- `h` tag for relay scoping noted as Phase 2+ requirement
