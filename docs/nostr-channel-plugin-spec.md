# Nostr Channel Plugin for OpenClaw — Implementation Spec

## Overview

A native OpenClaw channel plugin (`nostr`) that enables Clarity (or any OpenClaw agent) to participate in Nostr NIP-29 group chats and DMs as a first-class communication channel — same as Telegram, Discord, or Signal.

**Goal:** Replace the current webhook-based bridge with a proper channel plugin that gives:
- Persistent sessions per group (continuous conversation context)
- Native outbound messaging (reply to Nostr from agent context)
- Mention detection and activation gating
- Group chat history and threading
- Profile resolution for display names

## Architecture

```
┌─────────────────────────────────────────────┐
│  OpenClaw Gateway                           │
│  ┌────────────────────────────────────────┐ │
│  │  Nostr Channel Plugin (TypeScript)     │ │
│  │  ┌──────────┐  ┌──────────────────┐   │ │
│  │  │ Inbound  │  │ Outbound         │   │ │
│  │  │ (relay   │  │ (sign + publish  │   │ │
│  │  │  events) │  │  via relay)      │   │ │
│  │  └────┬─────┘  └───────┬──────────┘   │ │
│  │       │                │               │ │
│  │  ┌────┴────────────────┴──────────┐   │ │
│  │  │  nostr-tools (lightweight)     │   │ │
│  │  │  Raw WebSocket to relay        │   │ │
│  │  └────────────────────────────────┘   │ │
│  └────────────────────────────────────────┘ │
│                                             │
│  Sessions: nostr:group:<relay>:<group_id>   │
│            nostr:dm:<relay>:<pubkey>        │
└─────────────────────────────────────────────┘
          │                    ▲
          ▼                    │
   ┌──────────────────────────────┐
   │  Nostr Relay (NIP-29)       │
   │  wss://zooid.atlantislabs.space │
   └──────────────────────────────┘
```

## Plugin Identity

```typescript
const plugin: ChannelPlugin = {
  id: "nostr",
  meta: {
    id: "nostr",
    label: "Nostr",
    selectionLabel: "Nostr (NIP-29 Groups)",
    docsPath: "nostr",
    blurb: "Connect to Nostr relay group chats and DMs",
    order: 90,
  },
  capabilities: {
    chatTypes: ["group", "dm"],
    reactions: false,     // NIP-25 reactions possible later
    edit: false,          // Nostr events are immutable
    unsend: false,        // No delete in NIP-29
    reply: false,         // No threading in basic NIP-29
    media: false,         // Text-only initially
    threads: false,
    blockStreaming: false,
  },
};
```

## Configuration Schema

```jsonc
// ~/.openclaw/openclaw.json
{
  "channels": {
    "nostr": {
      "enabled": true,
      "relay": "wss://zooid.atlantislabs.space",
      "nsecFile": "/home/k0/openclaw/.secrets/nostr.json",
      // or inline: "nsec": "nsec1..."
      "groups": {
        "techteam": {
          "requireMention": false,   // respond to all messages
          "allowFrom": []            // empty = allow all pubkeys
        },
        "inner-circle": {
          "requireMention": true,    // only when mentioned
          "allowFrom": []
        }
      },
      "dmPolicy": "allowlist",      // "open" | "allowlist" | "pairing"
      "allowFrom": [
        "npub1zc6ts76lel22d38l9uk7zazsen8yd7dtuzcz5uv8d3vkast9hlks4725sl"  // k0
      ],
      "profileCache": true,         // resolve kind 0 profiles
      "backfillSeconds": 3600       // fetch last hour on connect
    }
  }
}
```

## Session Keys

```
nostr:group:<group_id>                → e.g. nostr:group:techteam
nostr:dm:<hex_pubkey_short>           → e.g. nostr:dm:1634b87b
```

Each group gets a persistent session with full conversation history — no more isolated sessions per message.

## Inbound Flow (Relay → Agent)

### 1. Gateway Startup (`gateway.startAccount`)

```typescript
async startAccount(ctx: ChannelGatewayContext) {
  // 1. Load nsec from file or config
  // 2. Connect to relay via raw WebSocket (nostr-tools SimplePool or manual WS)
  // 3. Authenticate (NIP-42 AUTH challenge)
  // 4. Subscribe to configured groups (kind 9, #h filter)
  // 5. Subscribe to DMs (kind 4, #p filter)
  // 6. Start event loop
}
```

### 2. Event Processing

On each incoming event:

```typescript
function handleEvent(event: NostrEvent) {
  // Skip own events (pubkey === our_pubkey)
  // Resolve author display name from kind 0 cache
  // Check allowFrom / security policy
  // Route to correct session:
  //   - kind 9 → session "nostr:group:<h_tag>"
  //   - kind 4 → session "nostr:dm:<author_pubkey_short>"
  // Inject as inbound message with metadata:
  //   - sender id: hex pubkey
  //   - sender name: display name from profile
  //   - chat type: "group" or "dm"
  //   - group subject: group name (for groups)
  //   - message id: event id (hex)
}
```

### 3. Mention Detection

For `requireMention: true` groups:
- Check if our npub/hex pubkey appears in event tags (`p` tag)
- Check if our display name or npub appears in content text
- Check for `@Clarity` or similar @-mentions in content
- If not mentioned → skip (don't create agent turn)

## Outbound Flow (Agent → Relay)

### 1. Outbound Adapter

```typescript
outbound: {
  deliveryMode: "direct",
  
  resolveTarget({ to }) {
    // Parse "nostr:group:techteam" or "nostr:dm:<pubkey>"
    // Return normalized target
  },

  async sendText({ to, text }) {
    // Parse target: group or DM
    // For groups: build kind 9 event with h tag
    // For DMs: build kind 4 event (NIP-04 encrypted)
    // Sign with our keys
    // Publish to relay
    // Return { messageId: event_id_hex }
  }
}
```

### 2. Message Tool Integration

The agent sees Nostr as a channel in the `message` tool:
```
message action=send channel=nostr to=nostr:group:techteam message="Hello from Clarity"
```

Or for replies in the Nostr session context, the agent's response is automatically routed back to the correct group/DM.

## Key Components

### Profile Cache

```typescript
class ProfileCache {
  // LRU cache of hex_pubkey → { name, displayName, about, picture, nip05 }
  // Populated from kind 0 events
  // Subscribed on first encounter of unknown pubkey
  // Used for display names in inbound messages
  // TTL: 1 hour, refresh on update
}
```

### Relay Connection Manager

```typescript
class RelayConnection {
  // Raw WebSocket to relay
  // NIP-42 AUTH on connect
  // Auto-reconnect with backoff (5s, 10s, 30s, 60s max)
  // Subscription management (REQ/CLOSE)
  // Event signing and publishing
  // Ping/pong keepalive
}
```

Use `nostr-tools` (npm) for:
- Event creation, signing, verification
- NIP-19 encoding/decoding (npub, nsec, note)
- NIP-04 encryption/decryption (DMs)
- Filter building

Use raw WebSocket (not nostr-tools relay pool) for connection — gives full control over reconnect, AUTH, and subscription lifecycle.

### Security Adapter

```typescript
security: {
  dmPolicy(ctx) {
    return {
      policy: ctx.account.dmPolicy || "allowlist",
      allowFrom: ctx.account.allowFrom || [],
      allowFromPath: "channels.nostr.allowFrom",
      policyPath: "channels.nostr.dmPolicy",
      approveHint: "Add their npub to channels.nostr.allowFrom",
    };
  },
  
  isAllowedSender(ctx, senderId) {
    // Check hex pubkey against allowFrom (supports npub and hex)
    // For groups: check group-specific allowFrom, fall back to global
    // For DMs: check dmPolicy
  }
}
```

## File Structure

```
openclaw/
├── plugins/
│   └── nostr/
│       ├── package.json
│       ├── index.ts              # Plugin entry point, exports ChannelPlugin
│       ├── relay.ts              # WebSocket connection, AUTH, subscriptions
│       ├── events.ts             # Event creation, signing, parsing
│       ├── profiles.ts           # Kind 0 profile cache
│       ├── config.ts             # Config adapter, schema, validation
│       ├── gateway.ts            # Gateway adapter (startAccount/stopAccount)
│       ├── outbound.ts           # Outbound adapter (sendText)
│       ├── security.ts           # Security adapter (allowFrom, dmPolicy)
│       ├── mentions.ts           # Mention detection for group gating
│       └── types.ts              # Nostr-specific types
```

## Dependencies

```json
{
  "dependencies": {
    "nostr-tools": "^2.10",       // Event signing, NIP-04, NIP-19
    "ws": "^8.18"                  // WebSocket (Node.js)
  },
  "peerDependencies": {
    "openclaw": ">=2026.2"
  }
}
```

## Migration from Bridge

1. **Phase 1 (current):** Rust bridge + webhook hooks — working now
2. **Phase 2:** Build the channel plugin, test alongside bridge
3. **Phase 3:** Switch config from hooks to `channels.nostr`, disable bridge
4. **Phase 4:** Remove bridge webhook config, keep bridge as optional cache/API

The Rust bridge (`crates/bridge/`) remains useful as:
- Standalone bridge for non-OpenClaw agents (Snowclaw, Zep)
- SQLite event cache and query API
- Testing and development tool

## Implementation Priority

### MVP (get it working)
1. `relay.ts` — WebSocket connection with NIP-42 AUTH and auto-reconnect
2. `gateway.ts` — startAccount/stopAccount, event loop, inbound routing
3. `outbound.ts` — sendText for groups (kind 9)
4. `config.ts` — basic config adapter (one account, relay + nsec + groups)
5. `index.ts` — wire it all together as ChannelPlugin

### Post-MVP
6. `profiles.ts` — kind 0 profile cache for display names
7. `security.ts` — allowFrom, dmPolicy, mention gating
8. `mentions.ts` — @mention detection in content and p-tags
9. DM support (kind 4 with NIP-04 encryption)
10. Multiple relay support

### Future
11. NIP-25 reactions (emoji reactions on Nostr events)
12. Media attachments (image URLs in content)
13. Multiple account support
14. NIP-17 encrypted group messages
15. Nostr-native threading

## Open Questions

1. **Plugin loading:** Does OpenClaw support loading channel plugins from a local directory, or does it need to be an npm package? Need to check `plugins.entries` and `internal.handlers` config.
2. **Inbound message injection:** The gateway adapter's `startAccount` returns a handle — how does it inject inbound messages into the session system? Need to study how Telegram plugin does it.
3. **Session routing:** How does OpenClaw route an agent's text reply back through the correct channel? Is it automatic based on the session's channel context?
4. **Config hot-reload:** When relay URL or groups change, can the plugin reconnect without full gateway restart?

---

*Spec created 2026-02-19 by Clarity*
*Based on OpenClaw plugin-sdk types analysis and existing bridge implementation*
