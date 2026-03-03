# ❄️ Snowclaw

Nostr-native AI assistant built on [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw). Extends it with first-class Nostr protocol support, collective memory, and decentralized identity — turning a general-purpose AI assistant into one that lives natively on the Nostr network.

→ [Manifesto](docs/MANIFESTO.md) · [Collective Memory Design](docs/collective-memory.md) · [Snow UI Design](docs/snow-ui.md)

## What Snowclaw Adds

### 🌐 Native Nostr Channel (~2400 LOC)
Full NIP-29 group chat support as a first-class channel — not a bridge, not a webhook, a native integration:
- **NIP-04 & NIP-17 DMs** — dual-protocol direct messages with automatic protocol detection
- **NIP-29 group chat** — relay-based group conversations with mention gating
- **NIP-42 AUTH** — relay authentication for access-controlled relays
- **Key-based allow/deny lists** — granular pubkey filtering per group
- **Event deduplication** — LRU cache preventing duplicate processing
- **Seen events persistence** — SQLite-backed event tracking across restarts

### 🧠 Social Memory System
Per-npub memory that builds understanding of people over time:
- **Social profiles** (`memory/social.rs`, ~950 LOC) — per-pubkey metadata, interaction history, relationship context
- **Nostr-native persistence** — memories stored as NIP-78 events on relays, optionally encrypted with NIP-44
- **Encrypted semantic search** — vector embeddings + keyword search over NIP-44 encrypted memory events
- **SQLite local cache** (`memory/nostr_sqlite.rs`) — fast local storage synced with relay
- **Unified search** (`memory/unified_search.rs`) — hybrid vector + keyword search across all memory backends
- **Document indexing** (`memory/doc_index.rs`, `memory/file_indexer.rs`) — file and message indexing for RAG
- **Embeddings** (`memory/vector.rs`, `memory/embeddings.rs`) — vector similarity search for semantic recall

### 🔧 Nostr Core Library (`crates/nostr-core/`, ~2000 LOC)
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
- **Social search** — search across social memory by npub, group, or content
- **Agent lessons** — self-improving knowledge base from interactions
- **Enhanced browser automation** — extended browser tool capabilities
- **Security key filtering** (`security/key_filter.rs`) — pubkey-based access control

### 📋 CLI Extensions
- `snowclaw nostr` — relay management, group listing, message sending
- `snowclaw memory` — memory search, inspect, and management
- `snowclaw tasks` — Nostr-native task tracking

## Architecture

```
snowclaw (binary)
├── src/                         # Fork of zeroclaw + Snowclaw additions
│   ├── channels/
│   │   ├── nostr.rs             # Native Nostr channel (NIP-04/17/29/42)
│   │   └── nostr_memory.rs      # Nostr-specific memory layer
│   ├── memory/
│   │   ├── social.rs            # Per-npub social memory
│   │   ├── nostr.rs             # NIP-78 relay persistence
│   │   ├── nostr_sqlite.rs      # Local SQLite cache
│   │   ├── vector.rs            # Vector similarity search
│   │   ├── unified_search.rs    # Hybrid search orchestrator
│   │   ├── doc_index.rs         # Document indexing
│   │   └── embeddings.rs        # Embedding generation
│   ├── tools/
│   │   ├── nostr_tasks.rs       # Nostr task management
│   │   ├── social_search.rs     # Social memory search
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

Snowclaw uses `~/.snowclaw/config.toml`. Key sections beyond upstream ZeroClaw:

```toml
[nostr]
secret_key = "nsec1..."
relays = ["wss://relay.example.com"]

[nostr.groups.mygroup]
group_id = "my-group"
respond_mode = "mention"  # "all" | "mention" | "owner" | "none"
allowed_pubkeys = ["npub1..."]

[memory]
encrypted_memory = true  # NIP-44 encryption for relay-stored memory
```

## Relationship to ZeroClaw

Snowclaw is a **fork** that tracks upstream ZeroClaw via periodic rebases. All upstream features (Telegram, Discord, Signal, WhatsApp, Matrix, MCP, cron, skills, hooks, hardware support) are available. Snowclaw adds the Nostr-native layer on top.

- **Upstream:** [zeroclaw-labs/zeroclaw](https://github.com/zeroclaw-labs/zeroclaw)
- **Fork:** [k0sti/snowclaw](https://github.com/k0sti/snowclaw)

## Vision: Collective Memory

Every Snowclaw instance learns. The good ones share what they learn — as signed Nostr events. The network gets smarter together.

Memory is a graph, not a tree. Events link via `supersedes` chains, topic tags, and source relationships. Agents self-report their backing LLM, and a configurable trust ranking handles quality differences across the network.

Three tiers: **public** (open relays), **group** (access-controlled or NIP-44 encrypted), **private** (agent↔human, always encrypted). Knowledge flows upward with consent, never automatically.

**Snow UI** — a Rust/WASM + TypeScript web app (using [applesauce](https://github.com/hzrd149/applesauce) for Nostr plumbing) for inspecting memories, debugging search ranking, and configuring trust. Built early to aid development.

See the design docs:
- [Manifesto](docs/MANIFESTO.md) — why Snowclaw exists
- [Collective Memory](docs/collective-memory.md) — memory tiers, quality ranking, conflict resolution
- [Snow UI](docs/snow-ui.md) — web UI architecture and components

## License

MIT OR Apache-2.0 (same as upstream ZeroClaw)
