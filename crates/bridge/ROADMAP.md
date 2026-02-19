# Nostr Bridge for OpenClaw — Status & Roadmap

> Standalone Rust Nostr bridge extracted from Snowclaw's native Nostr channel.
> Goal: bring Snowclaw-grade Nostr features to any webhook-based agent (OpenClaw, etc.)

## Architecture

```
┌─────────────┐     NIP-29/NIP-17      ┌──────────────┐
│ Nostr Relay  │◄──────────────────────►│  nostr-bridge │
│ (zooid etc.) │                        │  (this crate) │
└─────────────┘                        └──────┬───────┘
                                              │ HTTP webhook (POST)
                                              │ + HTTP API (:3847)
                                              ▼
                                       ┌──────────────┐
                                       │   OpenClaw    │
                                       │  (or any agent)│
                                       └──────────────┘
```

The bridge connects to Nostr relays, subscribes to groups/DMs, and forwards events
as compact webhook payloads. The agent posts back via the bridge's HTTP API.

## Current State (Rust bridge)

| Module | Lines | Status | Notes |
|--------|-------|--------|-------|
| `main.rs` | 165 | ✅ Done | CLI, config loading, startup |
| `config.rs` | 186 | ✅ Done | bridge.toml parsing, validation |
| `relay.rs` | 195 | ⚠️ Basic | Connect, subscribe groups/DMs, send. No NIP-29 kind 11/12 |
| `bridge.rs` | 278 | ⚠️ Basic | Event loop, dedup via SQLite, webhook dispatch |
| `webhook.rs` | 259 | ⚠️ Basic | Group + DM delivery, raw payloads only |
| `api.rs` | 364 | ⚠️ Basic | Status, send, query endpoints |
| `cache.rs` | 296 | ✅ Done | SQLite event cache with retention cleanup |
| `profiles.rs` | 272 | ✅ Done | Profile resolution + in-memory cache |

**Builds clean** (13 dead-code warnings, zero errors).

## Feature Gap: Rust Bridge vs Snowclaw Native

Snowclaw's `channels/nostr.rs` (1,681 lines) is the reference. Here's what the bridge is missing:

### 🔴 Critical (needed for group chat)

| Feature | Snowclaw | Bridge | Gap |
|---------|----------|--------|-----|
| NIP-29 kind 9 (chat) | ✅ | ✅ | — |
| NIP-29 kind 11/12 (threads) | ✅ | ❌ | Bridge only handles kind 9 |
| Conversation context (ring buffer) | ✅ | ❌ | Snowclaw sends last N messages as context to LLM |
| Compact message headers | ✅ | ❌ | `[nostr:group=#x from=y ...]` format for efficient LLM context |
| Mention detection | ✅ | ❌ | Name/npub/p-tag matching for respond-mode filtering |
| Respond mode (all/mention/guardian/none) | ✅ | ❌ | Controls when agent should reply vs stay silent |
| Content sanitization (key filter) | ✅ | ❌ | Redacts nsec/private keys from messages before LLM |

### 🟡 Important (agent coordination)

| Feature | Snowclaw | Bridge | Gap |
|---------|----------|--------|-----|
| NIP-17 gift-wrapped DMs (kind 1059) | ✅ | ❌ | Bridge uses old kind 4, Snowclaw uses NIP-17 |
| NIP-04 DM decryption | ✅ | ❌ stub | Returns `[encrypted]` |
| Guardian controls (halt/stop/resume) | ✅ | ❌ | Text commands + dynamic config |
| Action protocol (kind 1121) | ✅ | ❌ | Remote control via Nostr events |
| Agent state (kind 31121) | ✅ | ❌ | Publish/read agent online status |
| NIP-78 dynamic config (kind 30078) | ✅ | ❌ | Runtime config changes via guardian |
| Task status events (kind 1630-1637) | ✅ | ❌ | Nostr-native task tracking |

### 🟢 Nice-to-have (memory & awareness)

| Feature | Snowclaw | Bridge | Gap |
|---------|----------|--------|-----|
| Per-npub memory (NostrMemory) | ✅ | ❌ | Track contacts, interaction history |
| Per-group memory | ✅ | ❌ | Group member tracking, context |
| Agent state awareness | ✅ | ❌ | Know which other agents are online |
| History backfill on startup | ✅ | ❌ | Fetch recent messages to populate context |
| Profile metadata persistence | ✅ partial | ✅ in-memory | Bridge caches but doesn't persist |

## Implementation Plan

### Phase 1: Group Chat MVP 🎯
**Goal:** OpenClaw can participate in NIP-29 group conversations with context.

1. **Conversation ring buffer** — Port Snowclaw's `HistoryMessage` + `push_history` + `format_history_context`. Store last N messages per group in memory. Include in webhook payload so agent has conversation context.

2. **Compact webhook format** — Change webhook payload from raw event data to Snowclaw's compact format:
   ```
   [nostr:group=#techteam from=k0sh npub=npub1zc6ts76... kind=9 id=abcdef12]
   message content here
   ```
   Plus `[Recent conversation context]` block with ring buffer.

3. **Kind 11/12 support** — Subscribe to thread root + reply kinds alongside kind 9.

4. **Mention detection** — Name list + npub + p-tag matching. Add `mentioned: bool` to webhook payload so agent can decide whether to respond.

5. **Respond mode filtering** — `bridge.toml` config for per-group respond mode. Bridge filters before forwarding to webhook (reduces unnecessary API calls).

6. **Content sanitization** — Port `KeyFilter` from Snowclaw. Redact nsec/private key material before forwarding.

### Phase 2: Agent Coordination
**Goal:** Multiple agents can coordinate through Nostr.

7. **Agent state publishing** (kind 31121) — Announce online/offline on connect/disconnect.

8. **Action protocol** (kind 1121) — Accept remote commands (ping, stop, resume, config).

9. **Guardian controls** — HALT/stop/resume commands from guardian pubkey.

10. **NIP-17 DMs** — Replace kind 4 with gift-wrapped DMs (kind 1059) using `nostr-sdk`'s `send_private_msg`.

11. **Dynamic config** (kind 30078) — Runtime config from guardian events.

### Phase 3: Memory & Persistence
**Goal:** Bridge maintains rich context across restarts.

12. **Per-npub/group memory** — Port `NostrMemory` from Snowclaw (JSON files on disk).

13. **Profile persistence** — Save profiles to SQLite alongside events.

14. **History backfill** — On startup, fetch recent events from relay to populate ring buffer.

15. **Task events** (kind 1630-1637) — Forward task status changes to webhook.

## Code Reuse Strategy

Snowclaw's Nostr code is in `src/channels/nostr.rs` (1,681 lines) and `src/channels/nostr_memory.rs` (454 lines). Key reusable pieces:

- **Ring buffer logic** — `HistoryMessage`, `push_history`, `format_history_context` (~80 lines)
- **Compact headers** — `compact_group_header`, `compact_task_content` (~20 lines)
- **Mention detection** — `is_mentioned` (~25 lines)
- **Respond modes** — `RespondMode` enum + `respond_mode_for_group` (~50 lines)
- **Config events** — `parse_config_event`, `apply_config_entry`, `DynamicConfig` (~80 lines)
- **Key filter** — `src/security/key_filter.rs` (separate module)
- **NostrMemory** — `nostr_memory.rs` (454 lines, mostly portable)

Most of this can be extracted with minimal changes — the bridge just delivers via webhook instead of an internal channel trait.

## Config Changes Needed (bridge.toml)

```toml
# New fields for Phase 1
[groups]
subscribe = ["techteam", "inner-circle"]
respond_mode = "mention"  # default: all | mention | guardian | none
context_history = 20       # messages to include as context

[groups.overrides.techteam]
respond_mode = "all"

[identity]
mention_names = ["clarity", "snowflake"]  # trigger mention detection
guardian = "npub1..."  # guardian pubkey for controls

# Phase 2
[agent]
publish_state = true       # announce online/offline
listen_actions = true      # accept kind 1121 commands
```

## Quick Start (current state)

```bash
cd ~/work/snowclaw/bridge/rust
cp ../bridge.toml .  # reuse Go bridge config
cargo build --release
./target/release/bridge --config bridge.toml test  # validate config
./target/release/bridge --config bridge.toml       # run
```

## Priority

**Phase 1 is the blocker.** Without conversation context and respond mode filtering, the bridge just dumps raw events to webhook — the agent has no conversational awareness and can't intelligently decide when to respond. This is what makes Snowclaw's Nostr support actually useful vs a dumb relay pipe.

Phase 1 estimated effort: ~400-500 lines of new code, mostly ported from Snowclaw.
