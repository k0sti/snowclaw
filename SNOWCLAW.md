# ❄️ Snowclaw

Nostr-native AI assistant built on [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw). Snowclaw adds first-class Nostr support and uses **Nomen** as its primary memory backend.

## What Snowclaw Adds

### 🌐 Native Nostr Channel
Full NIP-29 group chat support as a first-class channel — not a bridge, not a webhook, a native integration:
- **NIP-04 & NIP-17 DMs** — dual-protocol direct messages with automatic protocol detection
- **NIP-29 group chat** — relay-based group conversations with mention gating
- **NIP-42 AUTH** — relay authentication for access-controlled relays
- **Key-based allow/deny lists** — granular pubkey filtering per group
- **Event deduplication** — LRU cache preventing duplicate processing
- **Seen events persistence** — SQLite-backed event tracking across restarts

### 🧠 Memory via Nomen
Snowclaw uses **Nomen** for agent memory, with two transport modes:
- **Socket transport** (default) — `src/memory/nomen_socket.rs` — communicates with a running Nomen daemon over Unix domain socket via `nomen-wire` protocol (~0.2ms latency, bidirectional, supports push events)
- **Direct library** (fallback) — `src/memory/nomen_adapter.rs` — embeds Nomen directly via `nomen` crate (0ms latency, no push events, tighter coupling)
- **Visibility/scope mapping** — `src/memory/nomen_policy.rs`
- **Runtime memory context** — `src/memory/runtime_context.rs`
- **Migration from legacy Snowclaw memory** — `src/memory/nomen_migrate.rs`

Transport is selected automatically by config: when `[memory] socket_path` is set (default: `$XDG_RUNTIME_DIR/nomen/nomen.sock`), socket transport is used. Set `socket_path = ""` to force direct library mode.

Legacy/compatibility memory code still exists in the tree, but it is not the main Snowclaw memory story anymore.

### 🔧 Nostr Core Library (`crates/nostr-core/`)
Extracted shared Nostr protocol primitives:
- Key filtering and content sanitization (redacts nsec before LLM)
- Mention detection (npub, hex, NIP-05, @name, broadcast)
- Conversation ring buffer for group history context
- Respond mode configuration (all/mention/owner/none) via NIP-78
- Action protocol parsing (kind 1121), task status events (kind 1630-1637)
- Context formatting with compact headers

### 📊 Cost Tracking & Observability
- **TokenBreakdown** — per-room, per-channel usage stats
- **Stats TUI** (`stats/tui.rs`) — terminal dashboard for real-time monitoring
- **Stats CLI** (`stats/mod.rs`) — command-line cost and usage queries

### 🛠️ Additional Tools
- **Nostr task management** — create and track tasks in group contexts
- **Agent lessons** — self-improving knowledge base from interactions
- **Enhanced browser automation** — extended browser tool capabilities
- **Security key filtering** (`src/security/key_filter.rs`) — pubkey-based access control

### 📋 CLI Extensions
- `snowclaw nostr` — relay management, group listing, message sending
- `snowclaw memory` — memory search, inspect, and migration workflows
- `snowclaw tasks` — Nostr-native task tracking

## Architecture

```
snowclaw (binary)
├── src/                         # Main application code: upstream ZeroClaw base + Snowclaw extensions
│   ├── channels/
│   │   ├── nostr.rs             # Native Nostr channel (NIP-04/17/29/42)
│   │   └── nostr_memory.rs      # Nostr social/context layer and legacy compatibility code
│   ├── memory/
│   │   ├── nomen_socket.rs      # Socket transport (nomen-wire over UDS, default)
│   │   ├── nomen_adapter.rs     # Direct library transport (fallback)
│   │   ├── nomen_policy.rs      # Visibility/scope compatibility mapping
│   │   ├── runtime_context.rs   # Canonical runtime memory context
│   │   └── nomen_migrate.rs     # Migration from legacy memory into Nomen
│   ├── tools/
│   │   ├── nostr_tasks.rs       # Nostr task management
│   │   └── agent_lesson.rs      # Self-improving knowledge
│   ├── stats/                   # Cost tracking & TUI
│   └── security/key_filter.rs   # Pubkey-based access control
└── crates/
    └── nostr-core/              # Shared Nostr protocol library
```

## Quick Start

```bash
# Build
cargo build --release

# Configure
cp config.example.toml ~/.snowclaw/config.toml
# Edit with your Nostr keys, relay URLs, and AI provider

# Run
./target/release/snowclaw
```

## Configuration

Snowclaw uses `~/.snowclaw/config.toml`. The exact memory configuration is still evolving, but the important current distinction is:

- **Nostr channel/config** lives in Snowclaw config
- **Agent memory** is backed by **Nomen**
- Legacy Snowclaw memory backends and migration code still exist for compatibility

## Relationship to ZeroClaw

Snowclaw is a **fork** that tracks upstream ZeroClaw via periodic rebases. All upstream features (Telegram, Discord, Signal, WhatsApp, Matrix, MCP, cron, skills, hooks, hardware support) are available. Snowclaw adds the Nostr-native layer on top.

- **Upstream:** [zeroclaw-labs/zeroclaw](https://github.com/zeroclaw-labs/zeroclaw)
- **Fork:** [k0sti/snowclaw](https://github.com/k0sti/snowclaw)

## Memory Direction

Snowclaw's memory direction is intentionally simpler now:
- Snowclaw handles the Nostr-native agent/runtime side
- **Nomen** handles the primary memory backend, accessed via Unix domain socket (IPC)
- Socket transport decouples Snowclaw from Nomen's internals — Nomen can be upgraded, restarted, or shared between agents independently
- Direct library fallback remains available for single-process deployments
- Older collective/social memory components remain in the repo mainly for compatibility, migration, or transitional use

So Snowclaw should not be read as exposing a separate graph-memory product of its own. If richer internal memory structures exist underneath, they belong to Nomen rather than Snowclaw's public surface.

## License

MIT OR Apache-2.0 (same as upstream ZeroClaw)
