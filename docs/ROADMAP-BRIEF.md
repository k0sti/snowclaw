# Snowclaw Roadmap Brief

Write a comprehensive roadmap document at `~/work/snowclaw/docs/roadmap/ROADMAP.md`.

## Source material
- Architecture doc: `~/openclaw/obsidian/Research/nostr-agent-system-architecture.md` 
- TASKS.md: `~/work/snowclaw/TASKS.md`
- Current source code: `~/work/snowclaw/src/`
- This brief

## Current state (Phase 1 — mostly complete)
- Nostr channel: NIP-29 groups, NIP-17 DMs, NIP-42 AUTH, profile cache
- Task state machine: 8 states with validated transitions
- Task events: kind 1621, 1630-1637, 31923
- NixOS service deployed (snowclaw.service, port 3200)
- Telegram + Nostr channels active
- Integration tests with nak serve
- Justfile for build/deploy workflow
- Binary being renamed from `zeroclaw` to `snowclaw`

## Phase 1 completion (in progress)
- Owner system (owner pubkey, 4-mode respond: none/owner/mention/all)
- Dynamic config via NIP-78 events from owner
- Message ring buffer with context history
- Secret key security filter (nsec redaction, hex flagging)
- Per-npub and per-group memory (auto-created on first interaction)
- npub identification in all LLM context
- Binary rename to `snowclaw`

## Phase 2: Dashboard + Memory persistence
- Flotilla task view (subscribe to kind 1621 + 31923 events in browser)
- NIP-78 memory persistence to relay (currently in-memory only)
- Memory CLI: `snowclaw nostr memory show/note/list`
- Task CLI: `snowclaw task create/list/status/update`
- Task creation from group messages (owner says "create task: ...")
- Task status updates from agent during execution
- Standalone dashboard prototype (beyond Flotilla)

## Phase 3: Multi-agent + ContextVM
- Sub-agent spawning with parent npub + agent tag
- ContextVM integration for structured tool execution
- Agent-to-agent communication via NIP-29 groups
- Capability delegation (owner grants tools to sub-agents)
- Parallel task execution with status tracking
- Agent registry (kind 31990 or similar)

## Phase 4: Bridges, Blossom, Zaps
- Nostr bridge for external agents (OpenClaw, other frameworks)
  - WebSocket relay → webhook translation
  - Identity mapping (agent gets npub)
  - Bidirectional: Nostr ↔ agent framework messages
- Blossom file storage (images, documents, artifacts)
- Zap integration (Lightning payments for task bounties)
- Cashu token support
- Mostr bridge compatibility (Mastodon/ActivityPub)

## Phase 5: Ecosystem
- Public agent directory on Nostr
- Skill marketplace (agents publish capabilities)
- Cross-relay federation
- Agent reputation system (based on task completion, zaps received)
- Template system for common agent patterns
- SDK/library for building Snowclaw-compatible agents in other languages

## Format
- Timeline estimates (weeks/months)
- Dependencies between phases
- MVP markers (what's the minimum for each phase to be useful)
- Risk factors
- Mermaid timeline/gantt if appropriate
