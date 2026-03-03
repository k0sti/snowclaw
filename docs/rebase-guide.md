# Rebase from Upstream Guide

## Quick Command

```bash
just rebase-upstream
```

This will:
1. Back up current branch
2. Squash local commits into logical groups
3. Fetch and rebase onto upstream/main
4. Run cargo check to verify

## Lessons Learned (2026-02-21)

### What causes pain
- **Many small commits** touching the same files → each is a conflict point
- **Forking shared files** like `schema.rs` → guaranteed conflicts every rebase
- **Long gaps between rebases** → 230 commits behind = 50 errors to fix

### How to minimize pain
1. **Rebase frequently** — weekly or bi-weekly, not monthly
2. **Keep local changes isolated** — use separate files, not modifications to upstream files
3. **Extend config via feature gates** — `#[cfg(feature = "snowclaw")]` blocks in schema.rs
4. **Squash before rebase** — group related commits to reduce conflict surface
5. **Never take `--theirs` blindly** on files we've modified — merge manually

### High-conflict files (handle carefully)
- `src/config/schema.rs` — config struct definitions, most-modified upstream file
- `src/channels/mod.rs` — channel wiring, grows with every new channel
- `Cargo.toml` / `Cargo.lock` — dependency changes
- `src/main.rs` — CLI subcommand registration

### Our local additions to track
- `src/channels/nostr.rs` — extended Nostr channel (groups + DMs)
- `src/channels/nostr_memory.rs` — Nostr memory channel
- `src/memory/nostr.rs` — NIP-78 memory backend
- `src/memory/nostr_sqlite.rs` — composite Nostr+SQLite backend
- `crates/nostr-core/` — shared Nostr protocol code
- `crates/bridge/` — Nostr bridge binary
- Config additions in `schema.rs` (NostrConfig extensions, MemoryConfig nostr fields)

## Commit Hygiene During Development

When working on a feature, it's fine to make many small commits. But before rebasing:

```bash
# Interactive rebase to squash related commits
git rebase -i $(git merge-base main upstream/main)
```

Group commits by logical unit:
- All bridge work → 1 commit
- All memory work → 1 commit  
- All config changes → 1 commit
- Bug fixes for the above → squash into parent

Target: ≤10 clean commits on top of upstream.
