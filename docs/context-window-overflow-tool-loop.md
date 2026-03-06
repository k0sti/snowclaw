# Context Window Overflow in Tool Loop

**Date**: 2026-03-06
**Status**: Fix in progress
**Severity**: High — causes API errors and potential OOM

## Problem

When the agent enters a tool loop (file_read, shell, etc.), tool results accumulate in the message history without any token budget checking. A single large tool output can blow past the 200k context window.

**Real example**: 653,513 tokens sent to API after just 7 tool iterations, resulting in an API rejection.

## Root Causes

### 1. No mid-loop history trimming
`trim_channel_prompt_history()` in `src/channels/mod.rs` runs once before the first LLM call but is **not** called during tool loop iterations in `src/agent/loop_.rs`. Each tool result gets appended to `history` and the accumulated total is never checked.

### 2. Shell output cap is too large
`MAX_OUTPUT_BYTES` in `src/tools/shell.rs` is set to 1MB (1,048,576 bytes). At ~3.3 bytes/token, that's ~330k tokens from a **single** shell call — exceeding the entire 200k context window by itself.

### 3. File read has no output truncation
`src/tools/file_read.rs` has a 10MB file size limit (`MAX_FILE_SIZE_BYTES`) but no output truncation. A 5MB text file would produce ~1.7M tokens of output, all of which goes straight into the message history.

## Fix Approach

### Fix A: Reduce tool output limits
- **Shell**: Reduce `MAX_OUTPUT_BYTES` from 1MB to 100KB (102,400 bytes). This gives ~34k tokens max per shell call — reasonable for a 200k context window.
- **File read**: Add output truncation at 100KB. If the formatted output exceeds 100KB, truncate with a message suggesting `offset`/`limit` parameters.

### Fix B: Pre-flight token budget check in tool loop
In `src/agent/loop_.rs`, before each LLM call in the tool loop, estimate total tokens of accumulated messages. If it exceeds a safety threshold (150k tokens for a 200k context model), truncate the oldest tool results by replacing their content with `[truncated — output too large]`.

Reuses the token estimation approach from `estimated_message_tokens()` / `estimated_history_tokens()` in `src/channels/mod.rs`.

## Upstream Status

- No fix exists in `zeroclaw-labs/zeroclaw` as of 2026-03-06
- PR #2808 adds loop detection but does **not** address context window overflow from large tool outputs
- This is a Snowclaw-only fix; may propose upstream later
