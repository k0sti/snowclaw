# Spec Alignment Review: Phase 0+1 vs Obsidian Specs

**Specs reviewed:**
- `/home/k0/Obsidian/vault/Projects/Snowclaw/Memory-Context-Spec.md`
- `/home/k0/Obsidian/vault/Projects/Snowclaw/Nostr-Events-Spec.md`

## Discrepancies

### 1. D-Tag Namespace (MAJOR)

**Spec:** `snowclaw:memory:npub:<hex>`, `snowclaw:memory:group:<id>`
**Implementation (nostr_sqlite.rs):** `snowclaw:<category>:<key>` (e.g., `snowclaw:core:my-key`)
**social.rs:** No relay persistence yet — SQLite only

**Action needed:** When social data gets relay-published (Phase 2+), use the spec's `snowclaw:memory:npub:` and `snowclaw:memory:group:` prefixes, not the agent memory's `snowclaw:<category>:` pattern.

### 2. Missing Fields on SocialNpub (MINOR)

**Spec NpubMemory has:**
- `preferences: HashMap<String, String>` (language, verbosity, etc.)
- `is_owner: bool`

**social.rs SocialNpub has:** Neither field.

**Action:** Add `preferences_json TEXT` and `is_owner INTEGER DEFAULT 0` to `social_npubs` table.

### 3. Missing Fields on SocialGroup (MINOR)

**Spec GroupMemory has:**
- `themes: Vec<String>` — discussion themes
- `decisions: Vec<(String, u64)>` — timestamped decisions

**social.rs SocialGroup has:** Only `notes_json` (generic).

**Action:** Add `themes_json TEXT` and `decisions_json TEXT` columns, or structure notes to include these.

### 4. No `h` Tag on Memory Events (MAJOR)

**Spec:** Memory events include `["h", "group-id"]` for relay-enforced group scoping.
**Implementation:** `nostr_sqlite.rs` publish_to_relay doesn't include `h` tag.

**Action:** Add `h` tag when publishing social memory events that are group-scoped.

### 5. Npub Hex Truncation (NIT)

**Spec:** "npub hex prefix in the d tag is truncated to 32 chars (first half of pubkey) for brevity"
**Implementation:** Will use full hex when relay persistence is added.

**Action:** Use 32-char truncation per spec.

### 6. Kind 4129 Agent Lessons (FUTURE)

Specced but not implemented. These are behavioral learnings the agent publishes. Natural fit for the memory system — could be a new MemoryCategory or stored in `message_index`.

### 7. Data Sovereignty Principle (OK — transitional)

Spec says relay is canonical, SQLite is cache. Phase 1 dual-writes SQLite + JSON, which is correct for transition. Relay publish for social data is Phase 2+ work.

## Verdict

The Phase 0 SQLite schema needs minor additions (preferences, is_owner, themes, decisions) before Phase 2 wires relay persistence. The d-tag namespace mismatch is the most important thing to get right before any social data hits the relay.

No blockers for Phase 1 (SQLite facade) — these are Phase 2 pre-requisites.
