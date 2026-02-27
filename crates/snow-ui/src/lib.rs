//! WASM bindings for Snow UI.
//!
//! Exposes snow-memory functions to JavaScript via wasm-bindgen.
//! The UI calls these to rank memories, detect conflicts, and parse
//! Nostr events — using the exact same logic as the agent runtime.

use wasm_bindgen::prelude::*;

use snow_memory::event::{memory_from_event, MemoryEvent};
use snow_memory::ranking::{self, Conflict};
use snow_memory::types::{Memory, SourcePreference};

/// Parse a Nostr event JSON string into a Memory.
///
/// Input: JSON string representing a `MemoryEvent`.
/// Returns: serialized `Memory` as JsValue, or throws on parse/conversion error.
#[wasm_bindgen]
pub fn parse_memory_event(json: &str) -> Result<JsValue, JsError> {
    let event: MemoryEvent = serde_json::from_str(json)
        .map_err(|e| JsError::new(&format!("invalid event JSON: {e}")))?;
    let memory = memory_from_event(&event)
        .map_err(|e| JsError::new(&format!("event conversion failed: {e}")))?;
    serde_wasm_bindgen::to_value(&memory).map_err(|e| JsError::new(&e.to_string()))
}

/// Input format for rank_memories: array of [Memory, relevance] pairs.
#[derive(serde::Deserialize)]
struct MemoryWithRelevance {
    memory: Memory,
    relevance: f64,
}

/// Rank a list of memories by source trust, model tier, and relevance.
///
/// Input: JSON array of {memory, relevance} objects + JSON array of source preferences.
/// Returns: sorted SearchResult array as JsValue.
#[wasm_bindgen]
pub fn rank_memories(results_json: &str, prefs_json: &str) -> Result<JsValue, JsError> {
    let items: Vec<MemoryWithRelevance> = serde_json::from_str(results_json)
        .map_err(|e| JsError::new(&format!("invalid results JSON: {e}")))?;
    let prefs: Vec<SourcePreference> = serde_json::from_str(prefs_json)
        .map_err(|e| JsError::new(&format!("invalid prefs JSON: {e}")))?;

    let pairs: Vec<(Memory, f64)> = items.into_iter().map(|i| (i.memory, i.relevance)).collect();
    let config = snow_memory::MemoryConfig {
        sources: prefs,
        ..Default::default()
    };
    let ranked = ranking::rank_memories(pairs, &config);
    serde_wasm_bindgen::to_value(&ranked).map_err(|e| JsError::new(&e.to_string()))
}

/// Detect conflicting memories (same topic, different sources).
///
/// Input: JSON array of Memory objects.
/// Returns: array of Conflict objects as JsValue.
#[wasm_bindgen]
pub fn detect_conflicts(memories_json: &str) -> Result<JsValue, JsError> {
    let memories: Vec<Memory> = serde_json::from_str(memories_json)
        .map_err(|e| JsError::new(&format!("invalid memories JSON: {e}")))?;
    let conflicts = ranking::detect_conflicts(&memories);
    serde_wasm_bindgen::to_value(&conflicts).map_err(|e| JsError::new(&e.to_string()))
}

/// Resolve a conflict between memories using source preferences.
///
/// Input: JSON for a Conflict object and source preferences.
/// Returns: index of the winning memory (or null if empty).
#[wasm_bindgen]
pub fn resolve_conflict(conflict_json: &str, prefs_json: &str) -> Result<JsValue, JsError> {
    let conflict: Conflict = serde_json::from_str(conflict_json)
        .map_err(|e| JsError::new(&format!("invalid conflict JSON: {e}")))?;
    let prefs: Vec<SourcePreference> = serde_json::from_str(prefs_json)
        .map_err(|e| JsError::new(&format!("invalid prefs JSON: {e}")))?;

    let config = snow_memory::MemoryConfig {
        sources: prefs,
        ..Default::default()
    };
    let winner = ranking::resolve_conflict(&conflict, &config);
    serde_wasm_bindgen::to_value(&winner).map_err(|e| JsError::new(&e.to_string()))
}
