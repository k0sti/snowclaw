# Key Management Specification v0.1

## Principle

An agent's nsec is its **identity**. Lose the nsec → lose the identity. Leak the nsec → identity is compromised. There is no recovery, no reset password, no admin override. Treat nsec like a root private key.

## Storage

### Location
```
~/.snowclaw/secrets/nostr.json    # mode 600, owner-only
```

### Format
```json
{
  "nsec": "nsec1...",
  "hex_secret": "...",
  "npub": "npub1...",
  "hex_pubkey": "...",
  "generated": "YYYY-MM-DD"
}
```

### File Permissions
```bash
chmod 700 ~/.snowclaw/secrets/
chmod 600 ~/.snowclaw/secrets/nostr.json
chmod 600 ~/.snowclaw/config.toml        # also contains nsec reference
```

### Config Reference
In `config.toml`, the nsec is referenced directly (for now):
```toml
[channels_config.nostr]
nsec = "nsec1..."
```

Alternative: environment variable `SNOWCLAW_NSEC` (preferred for systemd services):
```ini
# In systemd unit or environment file
Environment=SNOWCLAW_NSEC=nsec1...
# Or from a file:
EnvironmentFile=/home/user/.snowclaw/secrets/env
```

## Rules

### 1. One key per agent
Every agent instance gets its own unique keypair. **Never** share nsecs between agents, even temporarily. Clarity's key is Clarity's. Snowclaw's key is Snowclaw's. Zep's key is Zep's.

### 2. Never in LLM context
The security filter (`key_filter.rs`) ensures nsecs are redacted before reaching the LLM. But defense in depth — don't put nsecs where they don't belong:
- ❌ Never in git repos
- ❌ Never in logs (WARN about detection, don't print the key)
- ❌ Never in chat messages
- ❌ Never in memory/context files
- ❌ Never in SOUL.md, IDENTITY.md, or any workspace file
- ✅ Only in `secrets/nostr.json` and `config.toml` (both mode 600)

### 3. Never in git
Add to `.gitignore`:
```
secrets/
*.nsec
```
The `secrets/` directory must never be committed. If accidentally committed, rotate the key immediately — the old key is compromised.

### 4. Backup
The `secrets/nostr.json` file should be backed up securely (encrypted backup, hardware vault, etc.). If the secrets file is lost and no backup exists, the agent's identity is gone forever.

Recommended: keep an encrypted backup at a separate location.

### 5. Key rotation
To rotate a key:
1. Generate new keypair: `snowclaw nostr keygen`
2. Update relay membership (zooid config `roles.member`)
3. Publish new profile (kind 0) with new key
4. Update `secrets/nostr.json` and `config.toml`
5. Restart agent
6. The old npub is abandoned — inform contacts of the new identity

There is no key migration in Nostr. Rotation means a new identity.

### 6. Owner key
The owner's pubkey (not nsec!) is stored in `config.toml`:
```toml
owner = "hex_pubkey_here"
```
The owner's nsec is **never** stored in the agent's config. The agent only needs the pubkey to verify owner messages.

## Threat Model

| Threat | Mitigation |
|--------|-----------|
| nsec in LLM context | `key_filter.rs` redacts nsec patterns before LLM processing |
| nsec in logs | WARN log mentions detection but never prints the key |
| nsec in git | `.gitignore` + pre-commit hook recommended |
| nsec in chat | Security filter catches and redacts |
| File permission too open | Config warns on startup if mode > 600 |
| Lost nsec | Backup policy; no recovery without backup |
| Compromised nsec | Rotate immediately; old identity is burned |

## Future: Encrypted Storage

ZeroClaw's `SecretStore` supports `secrets.encrypt = true` which encrypts secrets at rest. When enabled, the nsec in `config.toml` is encrypted after first read. This is the recommended production configuration.

## Summary

```
                    ┌─────────────────────────┐
                    │   secrets/nostr.json     │ ← Source of truth (mode 600)
                    │   config.toml nsec=...   │ ← Runtime reference (mode 600)
                    └──────────┬──────────────┘
                               │
                    ┌──────────▼──────────────┐
                    │   SNOWCLAW_NSEC env var  │ ← Alternative (systemd)
                    └──────────┬──────────────┘
                               │
                    ┌──────────▼──────────────┐
                    │   NostrChannel (memory)  │ ← Runtime only, never persisted
                    └──────────┬──────────────┘
                               │
                    ┌──────────▼──────────────┐
                    │   key_filter.rs          │ ← Blocks nsec from reaching LLM
                    └─────────────────────────┘
```
