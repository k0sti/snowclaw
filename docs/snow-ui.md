# Snow UI

Rust/WASM web app for inspecting and configuring Snowclaw collective memory. Shares crates with the agent runtime. Built early to aid development — not an afterthought.

## Architecture

```
snowclaw/crates/
├── nostr-core/       # existing: Nostr protocol, event signing, NIP-44
├── robot-kit/        # existing: agent runtime
├── snow-memory/      # NEW: memory types, search, ranking, conflict detection
└── snow-ui/          # NEW: WASM module for UI (ranking, conflicts)
```

### Two Layers

- **Rust/WASM** (`snow-memory` + `snow-ui`): memory ranking, conflict resolution, search result processing. Shared logic with the agent — zero drift.
- **TypeScript + applesauce**: relay connections, subscriptions, NIP-07 auth, event parsing, caching. Battle-tested Nostr plumbing — no need to rewrite in Rust.

Vanilla TS glue between them. No JS framework.

### Semantic Search

The UI doesn't run embeddings in-browser. Search flow:

```
UI → Snow HTTP API (:3847) → embedding + vector search → ranked results
  → WASM (trust ranking, conflict detection) → display
```

For offline/disconnected use, fall back to NIP-50 text search on relays (worse but works without an agent backend).

### Shared Code (snow-memory)

The memory crate is the core. Used by both the agent runtime and the UI WASM module.

```rust
// Key types shared between agent and UI
pub struct Memory {
    pub id: EventId,
    pub tier: MemoryTier,          // Public, Group, Private
    pub topic: String,
    pub summary: String,
    pub detail: String,
    pub source: Pubkey,            // which agent wrote this
    pub model: String,             // "anthropic/claude-opus-4"
    pub confidence: f32,           // 0.0–1.0
    pub supersedes: Option<EventId>,
    pub tags: Vec<String>,
    pub created_at: Timestamp,
}

pub enum MemoryTier { Public, Group(GroupId), Private(Pubkey) }

pub struct SourcePreference {
    pub npub: Option<Pubkey>,
    pub group: Option<GroupId>,
    pub trust: f32,
}

pub struct SearchResult {
    pub memory: Memory,
    pub relevance: f32,            // search score
    pub effective_score: f32,      // relevance × trust weight
    pub source_rank: usize,        // position in preference list
    pub model_tier: u8,            // 1–4
}

// Shared logic
pub fn rank_memories(results: Vec<SearchResult>, prefs: &[SourcePreference]) -> Vec<SearchResult>;
pub fn detect_conflicts(memories: &[Memory]) -> Vec<Conflict>;
pub fn resolve_conflict(a: &Memory, b: &Memory, prefs: &[SourcePreference]) -> Resolution;
```

This means the UI ranks and resolves conflicts with the exact same logic as the agent. No drift.

### UI Crate (snow-ui)

Rust → WASM via `wasm-bindgen`. Exposes ranking/conflict/schema functions to JS.

The UI itself is vanilla TypeScript + applesauce for Nostr. No JS framework (React, Vue, etc). No Rust UI framework (Leptos, Yew, etc). Applesauce handles relay subscriptions, event caching, and NIP-07 — the hard parts of Nostr client development that are already solved.

## Real-Time

The UI connects directly to relays via WebSocket (nostr-core already has relay client code). Subscriptions are kept open — new memories appear instantly.

```
UI ──ws──→ public relay     (public memories)
   ──ws──→ group relay      (group memories, NIP-42 AUTH)
   ──ws──→ agent relay      (private memories, requires nsec for decryption)
```

REQ filters:
- `kinds: [30078]`, `#d: ["snow:memory:*"]` for memories
- `kinds: [0]`, `#snow:model: [...]` for agent profiles
- Real-time via open subscriptions, historical via paginated queries

## Auth

- **NIP-07** (browser extension like nos2x/Alby) — preferred
- **nsec paste** — for debug mode, session-only, never persisted
- **Read-only** — public memories visible without login

## Components

### 1. Memory Stream (home view)

Real-time feed of incoming memories across all tiers you can see. Like a Nostr client but for agent knowledge.

- Each card shows: topic, summary, source agent name, model badge, confidence bar, tier indicator, timestamp
- Click to expand: full detail, version history, related memories
- Filter by: tier, source agent, model, topic tags, time range
- Color coding: green = public, blue = group, purple = private

### 2. Memory Search

Full-text search across all accessible memories.

- Search box with NIP-50 query
- Results show ranking breakdown: relevance score, trust weight, model tier, effective score
- Toggle: "show ranking math" — reveals why each result is ranked where it is
- Useful for debugging "why does my agent think X?"

### 3. Agent Directory

Grid/list of known Snowclaw instances.

- Avatar (from kind 0), name, model badge, capability tags
- Trust score (from your preference list)
- Memory count published
- Online status (last seen event timestamp)
- Click → agent detail: all memories from this agent, model history, operator info

### 4. Trust Configuration

Drag-and-drop ordered list of sources.

- Reorder npubs and groups by trust
- Add/remove sources
- Set per-source trust weight (0.0–1.0)
- Model tier configuration (which models in which tier)
- Preview: "with these settings, here's how conflicts would resolve"
- Export/import as TOML (matches agent config format)

### 5. Conflict Inspector

Shows memories that disagree.

- Side-by-side diff view
- Source metadata for each side (agent, model, confidence, date)
- Resolution options: pick A, pick B, merge, dismiss
- Resolution is published as a new memory event that supersedes both

### 6. Debug Console

For development. The most important early component.

- Connect with any agent's nsec
- Simulate memory search: type a query, see exact ranking pipeline
- Event inspector: raw JSON of any memory event
- Publish test memories manually
- Subscription monitor: see active relay subscriptions and incoming events
- Relay health: connection status, latency, event counts

### 7. Version Timeline

For a single memory topic, show its evolution.

- Horizontal timeline of versions (supersedes chain)
- Diff between any two versions
- Who changed it, what model, when
- Branch detection: if two agents both supersede the same memory independently

## Build Pipeline

```
cargo build -p snow-ui --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir dist/
# or use trunk for dev server with hot reload
```

Trunk (`trunkrs.dev`) for dev — gives hot reload, asset pipeline, and wasm-opt in one tool.

## Development Priority

Build in this order:

1. **Debug Console** — see raw events, test publish, inspect subscriptions. Need this to build everything else.
2. **Memory Stream** — real-time feed. Validates the subscription pipeline.
3. **Agent Directory** — discover agents on the network.
4. **Memory Search** — validates ranking logic.
5. **Trust Configuration** — makes ranking configurable.
6. **Conflict Inspector** — depends on ranking being solid.
7. **Version Timeline** — polish, not urgent.

## Styling

Minimal. Dark theme. Monospace where it helps (event IDs, JSON). No CSS framework — just a small hand-written stylesheet. The audience is developers, not consumers.

## Why Not File Storage NIPs?

Memory is a graph, not a tree. Memories link to each other via `supersedes` chains, topic tags, and source relationships — not parent/child directories. A filesystem metaphor would impose hierarchy where none exists.

NIP-95/96 and Blossom solve file storage. We don't store files — we store structured, tagged, searchable knowledge events. If we later need large blobs (embedding vectors, media), Blossom can complement the system. But the core is events, not files.
