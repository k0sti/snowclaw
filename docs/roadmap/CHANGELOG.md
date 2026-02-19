# Snowclaw Changelog

All notable changes to the Snowclaw project, tracking what's been built.

## [Unreleased] — Phase 1 completion

### In Progress
- Owner system (owner pubkey, 4-mode respond: none/owner/mention/all)
- Dynamic config via NIP-78 events from owner
- Message ring buffer with context history
- Secret key security filter (nsec redaction, hex flagging) — `src/security/key_filter.rs`
- Per-npub and per-group memory (auto-created on first interaction)
- npub identification in all LLM context
- Binary rename from `zeroclaw` to `snowclaw`

### Planned
- Wire task tools into agent tool dispatch (`src/tools/nostr_tasks.rs` → tool system)
- Task CLI subcommands (`snowclaw task create/list/status/update`)
- Wire Nostr memory as selectable backend (`memory.backend = "nostr"`)
- Sub-agent event tagging (`["agent", "name"]`)

---

## [0.1.0] — 2026-02-12 — Core Nostr Agent

First working E2E: agent receives NIP-29 group messages, processes via LLM (Claude 4.6), replies via Nostr.

### Added

#### Nostr Channel (`src/channels/nostr.rs`)
- NIP-29 group message subscribe/publish (kinds 9, 11, 12)
- NIP-17 gift-wrapped DM support (kind 1059)
- NIP-42 AUTH automatic on relay connect
- Profile cache with LRU eviction
- Compact event headers for deduplication
- Reconnect with exponential backoff
- Channel routing fix (sub-addressing)

#### Task System (`src/tasks/`)
- 8-state machine with validated transitions — `state.rs`
  - Draft → Queued → Executing → Blocked → Review → Done / Failed / Cancelled
- Task events (kind 1621) — addressable/replaceable — `events.rs`
- Status transition events (kinds 1630-1637)
- Live run state (kind 31923) — replaceable with progress, tokens, elapsed time

#### Memory (`src/memory/`)
- Nostr memory backend (NIP-78, kind 30078) — `nostr.rs` (compiled, not yet default)
- Multiple backend support: markdown, sqlite, postgres, vector, lucid, none
- Memory traits with pluggable architecture — `traits.rs`
- Response cache — `response_cache.rs`
- Memory snapshot — `snapshot.rs`
- Memory hygiene — `hygiene.rs`

#### Nostr CLI (`src/nostr_cli.rs`)
- `snowclaw nostr keygen` — generate new keypair
- `snowclaw nostr whoami` — display agent npub
- `snowclaw nostr import` — import existing nsec
- `snowclaw nostr relays` — list configured relays

#### Agent Core (`src/agent/`)
- Main agent loop with message dispatch — `loop_.rs`
- Message classifier — `classifier.rs`
- Tool dispatcher — `dispatcher.rs`
- Prompt builder — `prompt.rs`
- Memory loader — `memory_loader.rs`

#### Tools (`src/tools/`)
- 28 tool implementations including:
  - Nostr task tools — `nostr_tasks.rs`
  - Shell execution — `shell.rs`
  - File read/write — `file_read.rs`, `file_write.rs`
  - Git operations — `git_operations.rs`
  - Web search — `web_search_tool.rs`
  - Browser automation — `browser.rs`, `browser_open.rs`
  - Cron scheduling — `cron_*.rs` (6 files)
  - Memory tools — `memory_store.rs`, `memory_recall.rs`, `memory_forget.rs`
  - HTTP requests — `http_request.rs`
  - Hardware tools — `hardware_*.rs`
  - Screenshot, image info, pushover, composio, delegate

#### Security (`src/security/`)
- Key filter for nsec redaction — `key_filter.rs`
- Bubblewrap sandboxing — `bubblewrap.rs`
- Firejail sandboxing — `firejail.rs`
- Landlock LSM — `landlock.rs`
- Docker isolation — `docker.rs`
- Audit logging — `audit.rs`
- Security policy — `policy.rs`
- Secrets management — `secrets.rs`
- Device pairing — `pairing.rs`

#### Providers (`src/providers/`)
- Anthropic (Claude) — `anthropic.rs`
- OpenAI — `openai.rs`
- OpenRouter — `openrouter.rs`
- Gemini — `gemini.rs`
- Ollama (local) — `ollama.rs`
- Copilot — `copilot.rs`
- GLM — `glm.rs`
- OpenAI Codex — `openai_codex.rs`
- Provider router with fallback — `router.rs`, `reliable.rs`

#### Channels (`src/channels/`)
- 15 channel implementations: Nostr, Telegram, Discord, Slack, Matrix, IRC, Signal, WhatsApp, iMessage, CLI, Email, DingTalk, Lark, QQ, Mattermost
- Nostr memory channel — `nostr_memory.rs`
- Channel traits — `traits.rs`

#### Infrastructure
- NixOS service (`snowclaw.service`) deployed on zooid, port 3200
- Config: `~/.snowclaw/config.toml` (relays, nsec, groups, listen_dms, allowed_pubkeys)
- Justfile: build, deploy, test, status, logs targets
- Integration tests with `nak serve`
- Zero compiler warnings
- Agent npub: `npub1jn7stcra3kr60epl05k66hv5v6f4s9qu46n7n98ajp86ttl0997qw4qak0`
- Zooid relay membership configured

#### Other Modules
- Observability: OpenTelemetry, Prometheus, structured logging — `src/observability/`
- Cron scheduler with persistent store — `src/cron/`
- Peripherals: Arduino, RPi, serial, Nucleo — `src/peripherals/`
- Hardware discovery and introspection — `src/hardware/`
- RAG module — `src/rag/`
- Runtime: Docker, native, WASM — `src/runtime/`
- Tunnel: Cloudflare, ngrok, Tailscale — `src/tunnel/`
- Gateway — `src/gateway/`
- Onboarding wizard — `src/onboard/`
- Cost tracking — `src/cost/`
- Health checks — `src/health/`
- Heartbeat engine — `src/heartbeat/`
- Skill forge (scout/evaluate/integrate) — `src/skillforge/`
- Approval system — `src/approval/`

---

*Changelog follows [Keep a Changelog](https://keepachangelog.com/) format.*
