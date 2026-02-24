# Rebase Patterns for Snowclaw

Lessons from the 55-commit upstream rebase (2026-02-20).

## Implementation Status

- `APP_DIR_NAME` constant: **done** — `src/config/schema.rs`, re-exported from `config/mod.rs`
- Functional path sites using `APP_DIR_NAME`: **done** — schema.rs, wizard.rs, native.rs, tracker.rs, openai_codex.rs
- Test structs using `..ChannelsConfig::default()`: **done** — 3 test sites in schema.rs
- Remaining `.snowclaw` hardcodes: docs/comments, test fixtures, service labels (low priority)

## Key Patterns

1. **Rebase frequently.** Weekly rebases keep conflicts to 1-2 files instead of 5+. The 55-commit gap was the root cause of pain.

2. **Isolate the snowclaw rename.** Functional path construction uses `APP_DIR_NAME` constant:
   ```rust
   // src/config/schema.rs (re-exported from config/mod.rs)
   pub const APP_DIR_NAME: &str = ".snowclaw";

   // Usage in any module:
   let dir = home.join(crate::config::APP_DIR_NAME);
   ```
   Upstream changes to those files won't conflict because we're not modifying the same lines.

3. **Use `..Default::default()` in tests.** Our `nostr: None` errors happened because upstream's tests spell out every field. Use:
   ```rust
   let c = ChannelsConfig {
       telegram: Some(TelegramConfig { .. }),
       ..ChannelsConfig::default()
   };
   ```

4. **Check upstream before fixing shared code.** Quick `git log upstream/main --oneline --grep="<keyword>"` before writing fixes to shared code.

5. **Keep new features in new files.** Our new files (`nostr_sqlite.rs`, `nostr.rs`, `nostr_tasks.rs`) had zero conflicts. Minimize edits to shared files; prefer additive registration (one-line factory wiring) over inline changes.

6. **Cargo.lock conflicts** — Always `git checkout --theirs Cargo.lock`, then `cargo check` to regenerate.

## Rebase Checklist

```bash
git fetch upstream
git log --oneline main..upstream/main  # count incoming
git rebase upstream/main
# Per conflict round:
#   Cargo.lock → git checkout --theirs Cargo.lock && git add Cargo.lock
#   schema.rs → check test vs functional, resolve accordingly
#   cargo check -p zeroclaw after each round
# Post-rebase:
cargo check -p zeroclaw  # catch type mismatches (Box→Arc, missing fields)
```
