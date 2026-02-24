# Snowclaw Specification Brief

Write comprehensive specification documents for the Snowclaw Nostr-native AI agent system. The codebase is at ~/work/snowclaw. Read the existing architecture doc at ~/openclaw/obsidian/Research/nostr-agent-system-architecture.md and the source code for reference.

## Documents to produce

### 1. `nostr-events.md` — Nostr Event Specification
Define all Nostr event kinds used by Snowclaw:

**Group messaging (NIP-29):**
- Kind 9: Group chat messages (send/receive)
- Kind 11: Group thread root
- Kind 12: Group thread reply
- Kind 9000-9020: Group admin events (put-user, remove-user, etc.)

**Tasks (NIP-34 extended):**
- Kind 1621: Task definition event
  - Tags: title, description, priority (p1-p4), due date, assignee (npub), parent task, depends-on, project, labels
  - Numbering: sequential per-project (like CRO-18)
- Kind 1630-1637: Task status events (open, applied, closed, draft, in-progress, blocked, review, done)
  - Extended from NIP-34's 1630-1633 range
- Kind 31923: Live task run state (addressable/replaceable)
  - Agent updates during execution, relay keeps only latest

**Memory (NIP-78):**
- Kind 30078: App-specific data (addressable)
  - `d` tag namespaces:
    - `snowclaw:memory:npub:<hex>` — per-user memory
    - `snowclaw:memory:group:<group_id>` — per-group memory
    - `snowclaw:config:group:<group_id>` — dynamic group config
    - `snowclaw:config:npub:<hex>` — dynamic per-user config
    - `snowclaw:config:global` — global config
  - Content: JSON (optionally NIP-44 encrypted)

**DMs (NIP-17):**
- Kind 1059: Gift-wrapped DMs
- Kind 1060: Sealed sender

**Identity:**
- Kind 0: Profile metadata (name, about, picture)
- Sub-agent identification via `["agent", "name"]` tag on events

**Authentication:**
- NIP-42: Relay authentication (automatic via nostr-sdk)

For each event kind, provide:
- JSON example with all tags
- Required vs optional tags
- Validation rules
- How Snowclaw creates and consumes the event

### 2. `security.md` — Security Specification
Cover:

**Key management:**
- Agent nsec storage (config.toml, encrypted at rest via SecretStore)
- Owner pubkey concept — elevated control pubkey
- SNOWCLAW_NSEC env var fallback
- Key rotation procedure

**LLM context security:**
- Content sanitization layer (runs before ALL LLM context)
- nsec detection: regex `nsec1[a-z0-9]{58}` → replace with `[REDACTED nsec → npub1<truncated>]`
- Hex secret key detection: 64-char hex not matching known pubkeys → flag
- Known pubkey allowlist from profile cache
- SecurityFlag reporting (WARN log + optional owner DM)
- No secrets ever in LLM context, period

**Access control:**
- Owner mode: only owner pubkey triggers responses
- Mention mode: any allowed pubkey when mentioning agent
- allowed_pubkeys whitelist (empty = allow all)
- Per-group and per-npub access levels

**Message integrity:**
- All messages carry npub (verifiable identity)
- Display names are cosmetic, npubs are proof
- Event signatures verified by relay/nostr-sdk

**Threat model:**
- Prompt injection via group messages
- Key exfiltration attempts
- Impersonation (mitigated by npub verification)
- Relay trust model

### 3. `memory-context.md` — Memory & Context Management
Cover:

**Per-npub memory:**
- Auto-created on first interaction
- Stores: display name history, first_seen, notes, preferences, owner annotations
- NpubMemory struct definition
- NIP-78 persistence (kind 30078, `snowclaw:memory:npub:<hex>`)

**Per-group memory:**
- Auto-created on first group activity
- Stores: purpose, member list, themes, decisions
- GroupMemory struct definition
- NIP-78 persistence (kind 30078, `snowclaw:memory:group:<id>`)

**Context ring buffer:**
- Per-group message history (configurable size, default 20)
- ALL messages cached regardless of respond mode
- Fed as conversation history when agent responds
- HistoryMessage struct: sender, content, timestamp, event_id, npub, is_owner

**LLM context assembly:**
- System prompt includes: agent identity, owner npub+name, current group context
- Per-message context: sender npub, owner flag, group memory summary, npub memory summary
- Context history: last N messages from ring buffer
- Token budget management

**Dynamic configuration:**
- Kind 30078 config events from owner
- Config priority: dynamic event > file config > defaults
- Live updates (subscription in event loop)
- CLI: `snowclaw nostr config set/get`

**Memory lifecycle:**
- In-memory HashMap on startup
- Periodic persistence to relay (NIP-78)
- Owner can view/edit via CLI
- No automatic deletion (owner-controlled)

### 4. `nostr-bridge.md` — Nostr Bridge for External Agents
Document the bridge project concept:
- Purpose: bridge Nostr NIP-29 groups to non-Nostr AI agents (OpenClaw, other frameworks)
- Existing bridge: `~/work/nostronautti/bridge/nostr-post.ts` (has NIP-42 auth)
- Architecture: WebSocket relay listener → webhook/API calls to agent frameworks
- Event translation: Nostr events ↔ agent message format
- Identity mapping: external agent gets a Nostr npub (posted on behalf)
- Authentication: NIP-42 for relay, webhook secrets for agents
- Use case: Clarity (OpenClaw) participating in Nostr groups via bridge
- Use case: Any AI agent framework joining Nostr groups without native Nostr support
- Protocol: define the webhook payload format and expected response format

## Style
- Technical, precise, with JSON examples throughout
- Mermaid diagrams for flows where helpful
- Cross-reference between documents
- Version each document (start at v0.1)
