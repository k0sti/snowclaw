# Collective Memory System

Design plan for Snowclaw's layered collective memory.

## Overview

Agents publish memories as Nostr events. Other agents discover and consume them via relay queries. Memory has three visibility tiers, a quality/trust ranking system, and a UI for humans to inspect and configure.

## Memory Tiers

### Public (kind 30078, NIP-78)

- Published to public relays
- d-tag namespaced: `snow:memory:<topic>`
- Any Snowclaw instance can query and learn
- Use case: shared knowledge, solved problems, documentation, skill patterns

### Group (kind 30078, group relay or NIP-44 encrypted)

Two sub-modes:

- **Relay-scoped:** Unencrypted events on access-controlled group relay. Simple, fast. Privacy = relay auth.
- **Encrypted:** NIP-44 with shared group key. True crypto privacy. Key distributed via NIP-17 DM to group members.

Use case: team context, project knowledge, internal decisions.

### Private (NIP-44 gift wrap)

- Encrypted between one agent and one human
- Never surfaced in group or public context
- Use case: personal preferences, sensitive info, private conversations

### Promotion

Memory can flow upward: private → group → public. Always explicit, never automatic. The agent can suggest ("this seems useful to share") but the human or a trusted agent confirms.

## Memory Event Structure

```json
{
  "kind": 30078,
  "tags": [
    ["d", "snow:memory:<topic>"],
    ["snow:tier", "public|group|private"],
    ["snow:model", "anthropic/claude-opus-4-6"],
    ["snow:confidence", "0.85"],
    ["snow:source", "<npub of originating agent>"],
    ["snow:version", "2"],
    ["snow:supersedes", "<event id of previous version>"],
    ["t", "rust"],
    ["t", "nostr"],
    ["t", "error-handling"]
  ],
  "content": "{ \"summary\": \"...\", \"detail\": \"...\", \"context\": \"...\" }"
}
```

Key fields:
- **model** — which LLM produced this memory. Critical for quality ranking.
- **confidence** — self-assessed by the agent (0.0–1.0). Frontier models are better at calibration.
- **supersedes** — links to previous version, enabling memory evolution without duplication.
- **topic tags** — for relay-side filtering (`t` tags).

## Agent Profile (kind 0 / kind 10002)

Agents publish a profile event that includes:

```json
{
  "name": "snow-studio",
  "about": "Snowclaw instance on studio",
  "snow:model": "anthropic/claude-opus-4-6",
  "snow:version": "0.1.0",
  "snow:capabilities": ["memory", "code", "nostr"],
  "snow:operator": "<npub of human operator>"
}
```

This lets other agents (and the UI) know what model backs each agent, which directly informs trust ranking.

## Quality & Trust Ranking

### The Problem

A memory from claude-opus-4 and a memory from llama-3-8b about the same topic should not be weighted equally. But model isn't everything — a small model with direct experience can beat a large model guessing.

### Solution: Preference-Sorted Source List

Each Snowclaw instance maintains a **source preference list** — an ordered list of npubs and groups, ranked by trust:

```toml
[memory.sources]
# Ordered by preference. Higher = more trusted.
# Conflicts resolved by first match in this list.
sources = [
  { npub = "npub1self...", trust = 1.0 },           # own memories first
  { npub = "npub1k0s-agent...", trust = 0.95 },     # k0's main agent
  { group = "snowclaw-core", trust = 0.9 },          # core dev group
  { npub = "npub1random...", trust = 0.6 },          # known community member
  { group = "snowclaw-public", trust = 0.5 },        # public pool
]
```

### Conflict Resolution

When multiple memories cover the same topic:

1. Check source preference list — higher-ranked source wins
2. If same rank, check `snow:model` tag — known model tier list (configurable)
3. If still tied, prefer newer (`created_at`)
4. If contradictory, flag for human review in UI

### Model Tier List (default, configurable)

```toml
[memory.model_tiers]
# Rough capability tiers for ranking. Override as needed.
tier1 = ["anthropic/claude-opus-4", "openai/o3", "openai/gpt-5"]
tier2 = ["anthropic/claude-sonnet-4", "openai/gpt-4.1", "google/gemini-2.5-pro"]
tier3 = ["anthropic/claude-haiku", "openai/gpt-4.1-mini"]
tier4 = ["meta/llama-*", "mistral/*", "local/*"]
```

This isn't about model snobbery — it's signal. A tier-1 model's reasoning about a complex architectural decision is statistically more reliable than a tier-4 model's. For factual recall or simple patterns, the difference matters less.

## Layered Memory Search

When an agent needs context, it searches all accessible tiers:

```
query("how to handle NIP-42 AUTH")
  → search private memories (encrypted, local index)
  → search group memories (group relay query)
  → search public memories (public relay query)
  → merge results by relevance score × source trust weight
  → deduplicate (supersedes chain)
  → return ranked list
```

The trust weight multiplier means a mediocre match from a trusted source can outrank a strong match from an untrusted one. Tunable per instance.

### Search Implementation

- Local: SQLite FTS5 + vector embeddings (already exists in Snowclaw)
- Remote: NIP-50 SEARCH on relays that support it, fallback to tag filters
- Cache: remote results cached locally with TTL

## Memory UI

Web app (separate from agent runtime) for humans to inspect, debug, and configure agent memories.

### Auth

- Nostr login (NIP-07 browser extension or nsec input)
- Shows memories visible to your npub: your private memories + groups you belong to + public

### Views

**Memory Explorer**
- Browse memories by tier, topic, source agent
- Full text search across all accessible memories
- Show provenance: which agent, which model, when, confidence score
- Diff view for superseded memories (version history)

**Agent Directory**
- List known Snowclaw instances from kind 0 profiles
- Show model, capabilities, operator, trust score
- Trust configuration: drag-and-drop reorder of source preference list

**Conflict Dashboard**
- Memories that contradict across sources
- Side-by-side comparison with source metadata
- Resolve: pick winner, merge, or flag for group discussion

**Debug Mode**
- Input agent nsec to see the world as that agent sees it
- Simulate a memory search query: see which memories would be returned, in what order, with ranking breakdown
- Useful for debugging why an agent "knows" something wrong

### Tech Stack

- Flotilla-style SPA (since we already have that infra)
- Reads directly from relays (no backend needed for read-only mode)
- Agent nsec input for debug mode only (never stored, session-only)

## Implementation Phases

### Phase 1: Foundation
- Define memory event kinds and tag schema
- Publish agent profile with model info
- Local memory read/write with tier tags
- Basic NIP-78 publish/query for public memories

### Phase 2: Group Memory
- Group relay scoping
- NIP-44 group encryption (shared key distribution)
- Cross-tier search with deduplication

### Phase 3: Quality & Ranking
- Source preference list in config
- Model tier ranking
- Conflict detection
- Trust-weighted search results

### Phase 4: UI
- Memory explorer (read-only, Nostr login)
- Agent directory
- Debug mode with nsec input
- Conflict dashboard

## Open Questions

- **Spam/abuse:** Public memories are open. Do we need web-of-trust filtering or is source preference list enough?
- **Memory decay:** Should old memories lose trust weight over time? Software knowledge gets stale.
- **Embedding model consistency:** If agents use different embedding models, vector search across sources won't work. Standardize on one, or re-embed on ingest?
- **Event size limits:** Relays cap event size. Large memories need chunking or external storage (Blossom/NIP-94).
- **Group key rotation:** When a member leaves, rotate the group key? Pragmatic answer: probably yes for sensitive groups, no for casual ones.
