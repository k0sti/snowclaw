# Collective Memory & Snow UI — Implementation Tasks

## Phase 1: snow-memory crate (Foundation)

### 1.1 Create crate skeleton
- [ ] Create `crates/snow-memory/Cargo.toml` with deps: serde, serde_json, nostr-core (workspace dep)
- [ ] Add to workspace members in root Cargo.toml
- [ ] Basic lib.rs with module structure

### 1.2 Core types
- [ ] `Memory` struct: id, tier, topic, summary, detail, source (pubkey), model, confidence, supersedes, tags, created_at
- [ ] `MemoryTier` enum: Public, Group(GroupId), Private(Pubkey)
- [ ] `MemoryEvent` — conversion to/from Nostr kind 30078 events with snow: tag schema
- [ ] `AgentProfile` — agent metadata (name, model, version, capabilities, operator npub)
- [ ] Serde serialization for all types

### 1.3 Event schema
- [ ] Define tag schema: `snow:tier`, `snow:model`, `snow:confidence`, `snow:source`, `snow:version`, `snow:supersedes`
- [ ] `Memory::to_nostr_event()` — serialize to NIP-78 event with proper tags
- [ ] `Memory::from_nostr_event()` — parse from NIP-78 event
- [ ] `AgentProfile::to_kind0()` / `from_kind0()` — agent profile events with snow: fields
- [ ] Validation: reject events with missing required tags

### 1.4 Source preference & ranking
- [ ] `SourcePreference` struct: npub/group, trust weight (0.0-1.0)
- [ ] `ModelTier` config: tier1-4 model lists (configurable)
- [ ] `SearchResult` struct: memory, relevance, effective_score, source_rank, model_tier
- [ ] `rank_memories()` — sort by: source preference → model tier → recency
- [ ] `detect_conflicts()` — find memories on same topic from different sources that disagree
- [ ] `resolve_conflict()` — pick winner based on ranking rules

### 1.5 Config
- [ ] `MemoryConfig` in TOML: source preference list, model tier list, relay URLs per tier
- [ ] Load/save from snowclaw config.toml (integrate with snowclaw_schema.rs)

## Phase 2: Agent profile publishing

### 2.1 Profile events
- [ ] On agent startup, publish kind 0 event with snow:model, snow:version, snow:capabilities
- [ ] Update profile when model changes
- [ ] Query other agent profiles from relays

### 2.2 Memory publishing
- [ ] Publish public memories as NIP-78 events to configured public relays
- [ ] Publish group memories to group relay (with NIP-42 AUTH)
- [ ] Publish private memories as NIP-44 encrypted events
- [ ] Supersedes chain: when updating a memory, link to previous version

## Phase 3: Layered search

### 3.1 Local search integration
- [ ] Extend existing SQLite memory backend to store tier metadata
- [ ] Index incoming relay memories into local SQLite cache
- [ ] Search local cache with tier-awareness (never return private memories in group context)

### 3.2 Remote search
- [ ] Query public relays for NIP-78 events matching search tags
- [ ] Query group relays with NIP-42 AUTH
- [ ] NIP-50 SEARCH support for relays that have it
- [ ] Cache remote results locally with TTL

### 3.3 Unified search pipeline
- [ ] `search()` → query all tiers → merge by relevance × trust weight → dedup (supersedes) → return ranked
- [ ] Expose via Snow HTTP API (:3847) for UI consumption
- [ ] Respect visibility: private stays private, group stays in group

## Phase 4: Group memory

### 4.1 Relay-scoped groups
- [ ] Publish unencrypted memories to access-controlled group relay
- [ ] NIP-42 AUTH for group relay access

### 4.2 Encrypted groups
- [ ] NIP-44 encryption with shared group key
- [ ] Key distribution via NIP-17 DM to group members
- [ ] Decrypt group memories on receipt

### 4.3 Memory promotion
- [ ] API to promote private → group or group → public
- [ ] Publish new event at higher tier, link to original

## Phase 5: Snow UI

### 5.1 Project setup
- [ ] Create `crates/snow-ui/` — Rust WASM crate
- [ ] wasm-bindgen exports for: rank_memories, detect_conflicts, resolve_conflict, parse_memory_event
- [ ] TypeScript project in `ui/` with applesauce dependency
- [ ] Trunk build pipeline (dev server + hot reload)
- [ ] Minimal dark theme CSS (no framework)

### 5.2 Debug Console (first component)
- [ ] Relay connection panel: connect to relays, show status/latency
- [ ] Subscription monitor: active subs, incoming events counter
- [ ] Event inspector: paste/click event → show parsed JSON + snow: tags
- [ ] Publish test memory: form to create and sign a memory event
- [ ] NIP-07 login (nos2x/Alby) + nsec paste for debug

### 5.3 Memory Stream
- [ ] Real-time feed of incoming memory events via relay subscriptions
- [ ] Memory card component: topic, summary, source agent, model badge, confidence bar, tier color
- [ ] Click to expand: full detail, version history
- [ ] Filter by: tier, source agent, model, topic tags, time range

### 5.4 Memory Search
- [ ] Search box → POST to Snow HTTP API → display ranked results
- [ ] Show ranking breakdown: relevance, trust weight, model tier, effective score
- [ ] "Show ranking math" toggle
- [ ] NIP-50 fallback when no agent backend available

### 5.5 Agent Directory
- [ ] Query kind 0 events with snow: fields
- [ ] Grid view: avatar, name, model badge, trust score, memory count
- [ ] Click → agent detail page

### 5.6 Trust Configuration
- [ ] Drag-and-drop source preference list
- [ ] Model tier configuration
- [ ] Export/import as TOML
- [ ] Live preview of conflict resolution with current settings

### 5.7 Conflict Inspector
- [ ] List conflicting memories (same topic, different sources)
- [ ] Side-by-side diff view
- [ ] Resolution actions: pick A/B, merge, dismiss → publish resolution event

### 5.8 Version Timeline
- [ ] Supersedes chain visualization
- [ ] Diff between versions
- [ ] Branch detection (two agents supersede same memory independently)
