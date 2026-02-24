# Phase 0+1 Review: Memory Unification

**Reviewer:** Validator Agent
**Date:** 2026-02-22
**Verdict:** CONDITIONAL PASS

---

## Summary

Phase 0 (new files: `social.rs`, `message_index.rs`, `doc_index.rs`, `unified_search.rs`) and Phase 1 (`nostr_memory.rs` facade, `nostr.rs` wiring, `mod.rs` declarations) are well-implemented, match the design doc, and all 87 new tests pass. Code quality is high — consistent with existing `sqlite.rs`/`nostr_sqlite.rs` patterns. Two minor issues need attention before merge; the rest are nits.

## Build / Test Results

| Check | Result |
|-------|--------|
| `cargo check` | PASS (0 errors, warnings in unrelated code only) |
| `cargo test --lib` (Phase 0+1 modules) | **87/87 PASS** — social:26, message_index:20, doc_index:20, unified_search:11, nostr_memory:10 |
| `cargo test --lib` (full suite) | 2921 pass, 1 fail (pre-existing: `native_storage_path_contains_zeroclaw` — unrelated) |
| `cargo clippy` | Pre-existing errors in `crates/nostr-core/` only. **No clippy issues in Phase 0+1 code.** |

---

## Issues

### MAJOR-1: FTS5 query injection via double-quotes (all search functions)

**Files:** `social.rs:488-490`, `message_index.rs:177-180`, `doc_index.rs:236-239`, `unified_search.rs:135-138`

All four search functions use the same FTS5 query-building pattern:

```rust
let fts_query: String = query
    .split_whitespace()
    .map(|w| format!("\"{w}\""))
    .collect::<Vec<_>>()
    .join(" OR ");
```

If a user's search term contains a literal double-quote character (e.g., `she said "hello"`), this produces a malformed FTS5 expression like `"she" OR "said" OR ""hello"" ` which will cause an FTS5 syntax error (SQLite error, caught by `?` — not a crash, but returns an error to callers instead of partial results).

**Severity:** Major (functional breakage on valid user input). Not a SQL injection risk since FTS5 MATCH is sandboxed, but it will break searches containing quotes.

**Fix:** Strip or escape double-quotes from individual words before wrapping:
```rust
.map(|w| {
    let clean = w.replace('"', "");
    format!("\"{clean}\"")
})
.filter(|w| w != "\"\"") // skip empty after cleaning
```

### MINOR-1: `add_npub_note` column name via `format!` — theoretical injection surface

**File:** `social.rs:184,200`

```rust
let sql = format!("SELECT {column} FROM social_npubs WHERE hex_pubkey = ?1");
```

The `column` variable is derived from a boolean (`is_owner`), so it's always one of two hardcoded strings — not user-controlled. **No actual vulnerability.** But the pattern of interpolating identifiers via `format!` is a code smell. A nit-level concern.

**Fix (optional):** Use two separate prepared statements instead of string interpolation.

### MINOR-2: `parking_lot::Mutex` + tokio — blocking risk in `nostr_memory.rs`

**File:** `nostr_memory.rs:91` — `sqlite: Option<Arc<ParkingMutex<Connection>>>`

The code correctly drops the `ParkingMutex` lock before any `.await` point in every method (verified: `ensure_npub`, `ensure_group`, `record_group_member`, `add_npub_note`, `build_context`, etc.). This is the right pattern.

However, `with_sqlite()` constructor (line 119) does multiple SQLite operations under `conn.lock()` (lines 127-169) including a potential JSON→SQLite migration that iterates all npubs/groups. For large JSON stores, this blocks the tokio thread during construction. Acceptable since construction happens once at startup, but worth noting.

**Severity:** Minor (startup-only, bounded).

### NIT-1: `touch_npub` returns `Ok(true)` for "doesn't exist" — confusing API

**File:** `social.rs:284-333`

`touch_npub` returns `Ok(true)` to mean "this npub doesn't exist, caller should upsert" and `Ok(false)` for "updated successfully". The `true = new/missing` semantics are the opposite of what "touch" implies (usually "true = touched successfully"). The caller in `nostr_memory.rs` handles it correctly, but the API is confusing.

### NIT-2: Social score hardcoded to 1.0 in unified_search

**File:** `unified_search.rs:35`

```rust
Self::Social(_) => 1.0, // FTS5 results from social are pre-ranked
```

This means social results always rank below high-scoring message/doc hits and above low-scoring ones. The comment says "pre-ranked" but FTS5 bm25 scores are available — they're just not captured during `search_social()`. Not a bug for Phase 0, but worth carrying forward to Phase 2 when social FTS scores should be propagated.

### NIT-3: `IndexableMessage.kind` is `u32` but Nostr kinds are `u16` conventionally

**File:** `message_index.rs:24` — `pub kind: u32`

Nostr SDK uses `u16` for event kinds. Using `u32` is wider than needed but not incorrect (SQLite stores as INTEGER anyway). Minor inconsistency.

---

## Checklist Assessment

### 1. Correctness
- SQLite schemas match design doc exactly
- FTS5 triggers are correct (INSERT, DELETE, UPDATE all handled)
- `IF NOT EXISTS` on all CREATE statements — idempotent
- All parameterized queries use `params![]` — no SQL injection on data values
- **One concern:** FTS5 query building doesn't escape double-quotes (MAJOR-1)

### 2. API Compatibility
- `NostrMemory` public API is **100% preserved**: `ensure_npub`, `ensure_group`, `record_group_member`, `add_npub_note`, `add_npub_owner_note`, `add_group_note`, `set_group_purpose`, `update_profile`, `get_npub`, `get_group`, `list_npubs`, `build_context`, `record_agent_state`, `flush`, `force_flush` — all present with same signatures
- New `with_sqlite()` constructor is additive — `new()` still works for legacy/test use
- `nostr.rs` wiring (`open_social_db` + `with_sqlite`) falls back gracefully to JSON-only

### 3. Error Handling
- All SQLite failures in `nostr_memory.rs` are caught with `warn!()` and fall back to cache
- No panics in any runtime path (only `.expect()` is in `content_hash` for SHA-256 slice — infallible)
- `unified_recall` silently skips missing tables — correct graceful degradation

### 4. Style Consistency
- Follows existing patterns from `sqlite.rs` / `nostr_sqlite.rs`
- Uses `anyhow::Result`, `tracing::{debug,info,warn}`, `rusqlite::params!`
- Test structure matches existing: `test_conn()` helper, in-memory SQLite, descriptive test names
- Module docs with `//!` headers — consistent

### 5. Test Coverage
- **Strong:** 87 tests total covering:
  - Schema idempotency, CRUD, upsert conflict behavior
  - FTS5 search (match, no-match, empty query, limit, unicode, special chars)
  - `should_index_message` decision matrix
  - `build_social_context` output formatting
  - Document indexing with change detection (hash-based skip)
  - Unified search across all types + graceful degradation
  - Full SQLite-backed `NostrMemory` facade tests + JSON migration
- **Missing edge case:** Search with double-quote characters in query (relates to MAJOR-1)

### 6. Upstream Compatibility
- **Excellent.** All new code is in new files (zero conflict on rebase):
  - `src/memory/social.rs` — new file
  - `src/memory/message_index.rs` — new file
  - `src/memory/doc_index.rs` — new file
  - `src/memory/unified_search.rs` — new file
- Modified files (rebase risk):
  - `src/memory/mod.rs` — **LOW risk** (3 lines added: `pub mod` + `pub use`)
  - `src/channels/nostr_memory.rs` — **MEDIUM risk** (substantially rewritten, but this file is Snowclaw-only, not upstream)
  - `src/channels/nostr.rs` — **LOW risk** (only the `NostrMemory` construction block changed, ~20 lines)
- No schema.rs / config changes yet (deferred correctly per design doc)

### 7. Bugs
- No logic errors, off-by-ones, or data races found
- `parking_lot::Mutex` locks are correctly scoped — always dropped before `.await`
- The dual-write pattern (SQLite + in-memory cache) is consistent across all mutation methods
- `INSERT OR REPLACE` on `message_index` correctly triggers FTS5 update trigger (delete old + insert new)

### 8. Design Adherence
- Matches design doc faithfully:
  - Phase 0: new tables/files with no behavior change
  - Phase 1: `NostrMemory` as facade, dual-write, JSON migration, graceful fallback
  - Separate `social.db` file (design doc says `brain.db`, but using separate file is reasonable — keeps social data isolated from core memory)
- `IndexDecision` enum matches design doc's three-tier strategy
- `build_social_context` format matches existing `build_context` in `nostr_memory.rs`

---

## Design Deviation: `social.db` vs `brain.db`

The design doc places social tables in `brain.db` (same database as core memories). The implementation creates a separate `social.db` in `persist_dir`. This is a deliberate deviation — it keeps social data isolated, which is actually better for:
- Independent backup/restore of social vs core memory
- Avoiding schema conflicts with `nostr_sqlite.rs`'s brain.db
- Easier Phase 4 cleanup

However, this means `unified_search.rs` can't actually query across `brain.db` and `social.db` in a single connection — it needs the social tables to be in the same database it's querying. **This is fine for Phase 0 (standalone tests use a single in-memory connection), but will need resolution when wiring `unified_recall` into `NostrSqliteMemory` in Phase 2.**

---

## Recommendation

**Merge after fixing MAJOR-1** (FTS5 double-quote escaping). The MINOR issues are acceptable debt for Phase 0+1. Track NIT-2 (social FTS score propagation) for Phase 2.
