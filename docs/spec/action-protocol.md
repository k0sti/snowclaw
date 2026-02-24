# Snowclaw Action Protocol v0.1

## Overview

All agent commands are Nostr events. No parsing natural language for config changes, no hardcoded string matching (except the owner killswitch). Every action is a signed, verifiable, relayed event.

## Event Kinds

| Kind | Type | Purpose |
|------|------|---------|
| 1121 | Regular | Action requests and responses (logged, kept by relay) |
| 31121 | Parameterized replaceable | Agent state (online status, current config — relay keeps latest) |

## Action Request (Kind 1121)

```json
{
  "kind": 1121,
  "tags": [
    ["p", "<agent_pubkey>"],
    ["action", "config.set"],
    ["param", "respond_mode", "mention"],
    ["param", "context_history", "30"],
    ["h", "techteam"]
  ],
  "content": "",
  "pubkey": "<sender_pubkey>"
}
```

### Tag Schema

| Tag | Required | Description |
|-----|----------|-------------|
| `p` | Yes | Target agent npub (hex) |
| `action` | Yes | Dot-separated action identifier |
| `param` | No | Key-value parameter (repeatable) |
| `h` | No | Group context |
| `e` | No | References another event (for responses, task updates) |

## Action Response (Kind 1121)

```json
{
  "kind": 1121,
  "tags": [
    ["p", "<requester_pubkey>"],
    ["e", "<request_event_id>", "", "reply"],
    ["action", "config.set.result"],
    ["status", "ok"]
  ],
  "content": "{\"respond_mode\":\"mention\",\"applied_to\":\"techteam\"}",
  "pubkey": "<agent_pubkey>"
}
```

### Status Values
- `ok` — action completed
- `error` — action failed (content has error message)
- `denied` — insufficient permissions
- `pending` — action queued

## Agent State (Kind 31121)

Replaceable — relay keeps only the latest per `d` tag.

```json
{
  "kind": 31121,
  "tags": [
    ["d", "snowclaw:status"],
    ["status", "online"],
    ["version", "0.1.0"],
    ["model", "claude-opus-4-6"]
  ],
  "content": "{\"uptime\":3600,\"groups\":[\"techteam\",\"inner-circle\"]}",
  "pubkey": "<agent_pubkey>"
}
```

### State `d` Tags
- `snowclaw:status` — agent online/offline/maintenance
- `snowclaw:config:global` — current global config
- `snowclaw:config:group:<id>` — current group config (replaces NIP-78 approach)

## Action Taxonomy

### profile.*
| Action | Params | Owner | Allowed | Public |
|--------|--------|----------|---------|--------|
| `profile.lookup` | `npub` | ✅ | ✅ | ❌ |
| `profile.set` | `name`, `about`, `picture`, `nip05` | ✅ | ❌ | ❌ |

### config.*
| Action | Params | Owner | Allowed | Public |
|--------|--------|----------|---------|--------|
| `config.set` | `respond_mode`, `context_history` + `h` tag for group | ✅ | ❌ | ❌ |
| `config.get` | optional `h` tag for group | ✅ | ✅ | ❌ |

### memory.*
| Action | Params | Owner | Allowed | Public |
|--------|--------|----------|---------|--------|
| `memory.note` | `npub`, `text` | ✅ | ❌ | ❌ |
| `memory.get` | `npub` or `group` | ✅ | ✅ | ❌ |
| `memory.forget` | `npub` or `group` | ✅ | ❌ | ❌ |
| `memory.list` | — | ✅ | ✅ | ❌ |

### task.*
| Action | Params | Owner | Allowed | Public |
|--------|--------|----------|---------|--------|
| `task.create` | `title`, `description`, `priority`, `assignee` | ✅ | ✅ | ❌ |
| `task.status` | `task_id`, `status` | ✅ | ✅ | ❌ |
| `task.list` | optional `status`, `assignee` | ✅ | ✅ | ❌ |
| `task.assign` | `task_id`, `npub` | ✅ | ❌ | ❌ |

### control.*
| Action | Params | Owner | Allowed | Public |
|--------|--------|----------|---------|--------|
| `control.stop` | optional `h` for group-specific | ✅ | ❌ | ❌ |
| `control.resume` | `mode` + optional `h` | ✅ | ❌ | ❌ |
| `control.ping` | — | ✅ | ✅ | ✅ (if enabled) |
| `control.status` | — | ✅ | ✅ | ❌ |

## Access Control

### Permission Levels
1. **Owner** — all actions, no restrictions
2. **Allowed** — pubkeys in `allowed_pubkeys` list: query actions + task management
3. **Public** — anyone: `control.ping` only (configurable)

### Config
```toml
[channels_config.nostr.action_permissions]
allowed = ["profile.lookup", "memory.get", "memory.list", "task.create", "task.status", "task.list", "config.get", "control.ping", "control.status"]
public = ["control.ping"]
```

## Owner Killswitch

**Independent of the action protocol.** A plain text safeguard that works even if the action system is broken.

In any kind 9 group message or NIP-17 DM, if the owner sends a message that is exactly one of these words (case-insensitive, trimmed):

```
HALT
```

The agent immediately:
1. Sets ALL groups to `respond_mode = none`
2. Stops processing all pending messages
3. Logs `🛑 HALT from owner — all processing stopped`
4. Publishes kind 31121 state event with `["status", "halted"]`
5. Remains halted until owner sends `control.resume` action event OR the text `RESUME`

This is the nuclear option. One word, instant effect, no parsing, no LLM in the loop.

### Additional Soft Stops (group-specific)
Owner text `stop` in a group → that group only goes to `none`
Owner text `resume` or `resume <mode>` → reactivates that group

These remain as convenience shortcuts alongside the action protocol.

## Implementation Plan

### Phase 1: Core dispatcher
- Subscribe to kind 1121 where `#p` matches agent pubkey
- Parse action tag, validate permissions
- Route to handler functions
- Publish response events

### Phase 2: Migrate existing features
- Replace NIP-78 config events → `config.set`/`config.get` actions
- Replace hardcoded stop/resume → `control.stop`/`control.resume` actions
- Keep killswitch word as permanent safeguard

### Phase 3: CLI integration
- `snowclaw nostr action <action> [params]` → publishes kind 1121 event
- Waits for response event, displays result
- Sugar commands: `snowclaw nostr stop` = `snowclaw nostr action control.stop`

### Phase 4: Multi-agent
- Agents can send actions to other agents
- Task delegation via `task.create` with assignee = another agent's npub
- Agent-to-agent `control.ping` for health checks
