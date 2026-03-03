//! Memory ranking, conflict detection, and resolution.

use crate::config::MemoryConfig;
use crate::types::{Memory, SearchResult, SourcePreference};

/// Weight multiplier per model tier (tier 1 = best).
const TIER_WEIGHTS: [f64; 5] = [0.0, 1.0, 0.8, 0.6, 0.4];

/// Get the trust weight for a source from the preference list.
/// Returns 0.0 (untrusted) if not in the list.
fn source_trust(source: &str, preferences: &[SourcePreference]) -> f64 {
    preferences
        .iter()
        .find(|p| p.matches_source(source))
        .map(|p| p.trust)
        .unwrap_or(0.0)
}

/// Determine the model tier (1-4) for a given model string.
/// Returns 4 (lowest) if not found in any tier.
fn model_tier(model: &str, config: &MemoryConfig) -> u8 {
    for (tier_num, tier_models) in [
        (1u8, &config.tier1),
        (2, &config.tier2),
        (3, &config.tier3),
        (4, &config.tier4),
    ] {
        for pattern in tier_models {
            if pattern.ends_with('*') {
                let prefix = &pattern[..pattern.len() - 1];
                if model.starts_with(prefix) {
                    return tier_num;
                }
            } else if pattern == model {
                return tier_num;
            }
        }
    }
    4
}

/// Rank memories by: source preference -> model tier -> recency.
///
/// Each memory gets an effective score = relevance * source_trust * tier_weight.
/// Results are sorted by effective_score descending, then by created_at descending.
pub fn rank_memories(
    memories: Vec<(Memory, f64)>,
    config: &MemoryConfig,
) -> Vec<SearchResult> {
    let mut results: Vec<SearchResult> = memories
        .into_iter()
        .map(|(memory, relevance)| {
            let trust = source_trust(&memory.source, &config.sources);
            let tier = model_tier(&memory.model, config);
            let tier_weight = TIER_WEIGHTS.get(tier as usize).copied().unwrap_or(0.4);
            let effective_score = relevance * trust * tier_weight;

            SearchResult {
                memory,
                relevance,
                source_trust: trust,
                model_tier: tier,
                effective_score,
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.effective_score
            .partial_cmp(&a.effective_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.memory.created_at.cmp(&a.memory.created_at))
    });

    results
}

/// A pair of conflicting memories on the same topic from different sources.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Conflict {
    pub topic: String,
    pub memories: Vec<Memory>,
}

/// Detect conflicts: memories on the same topic from different sources.
pub fn detect_conflicts(memories: &[Memory]) -> Vec<Conflict> {
    let mut by_topic: std::collections::HashMap<&str, Vec<&Memory>> =
        std::collections::HashMap::new();

    for mem in memories {
        by_topic.entry(mem.topic.as_str()).or_default().push(mem);
    }

    let mut conflicts = Vec::new();
    for (topic, mems) in by_topic {
        if mems.len() < 2 {
            continue;
        }
        // Check if there are multiple distinct sources
        let mut sources: Vec<&str> = mems.iter().map(|m| m.source.as_str()).collect();
        sources.sort();
        sources.dedup();
        if sources.len() > 1 {
            conflicts.push(Conflict {
                topic: topic.to_string(),
                memories: mems.into_iter().cloned().collect(),
            });
        }
    }

    conflicts
}

/// Resolve a conflict by picking the winner based on ranking rules.
///
/// Returns the winning memory index in the conflict's memory list, or None if empty.
pub fn resolve_conflict(conflict: &Conflict, config: &MemoryConfig) -> Option<usize> {
    if conflict.memories.is_empty() {
        return None;
    }

    let pairs: Vec<(Memory, f64)> = conflict
        .memories
        .iter()
        .map(|m| (m.clone(), 1.0)) // equal relevance since same topic
        .collect();

    let ranked = rank_memories(pairs, config);
    if ranked.is_empty() {
        return None;
    }

    // Find the index of the winner in the original conflict memories
    let winner_id = &ranked[0].memory.id;
    conflict.memories.iter().position(|m| m.id == *winner_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MemoryConfig;
    use crate::types::{Memory, MemoryTier, SourcePreference};

    fn test_config() -> MemoryConfig {
        MemoryConfig {
            sources: vec![
                SourcePreference::for_npub("self_agent", 1.0),
                SourcePreference::for_npub("trusted_agent", 0.9),
                SourcePreference::for_npub("community_agent", 0.5),
            ],
            tier1: vec!["anthropic/claude-opus-4-6".to_string()],
            tier2: vec!["anthropic/claude-sonnet-4".to_string()],
            tier3: vec!["anthropic/claude-haiku".to_string()],
            tier4: vec!["meta/llama-*".to_string(), "local/*".to_string()],
            relays_public: vec![],
            relays_group: vec![],
        }
    }

    fn make_memory(id: &str, source: &str, model: &str, created_at: u64) -> Memory {
        Memory {
            id: id.to_string(),
            tier: MemoryTier::Public,
            topic: "test/topic".to_string(),
            summary: "test".to_string(),
            detail: "test detail".to_string(),
            context: None,
            source: source.to_string(),
            model: model.to_string(),
            confidence: 0.85,
            supersedes: None,
            version: 1,
            tags: vec![],
            created_at,
        }
    }

    #[test]
    fn rank_prefers_higher_trust_source() {
        let config = test_config();
        let memories = vec![
            (make_memory("a", "community_agent", "anthropic/claude-opus-4-6", 100), 1.0),
            (make_memory("b", "trusted_agent", "anthropic/claude-opus-4-6", 100), 1.0),
        ];

        let ranked = rank_memories(memories, &config);
        assert_eq!(ranked[0].memory.id, "b"); // trusted_agent (0.9) > community_agent (0.5)
        assert_eq!(ranked[1].memory.id, "a");
    }

    #[test]
    fn rank_prefers_higher_tier_model() {
        let config = test_config();
        let memories = vec![
            (make_memory("a", "trusted_agent", "meta/llama-70b", 100), 1.0),
            (make_memory("b", "trusted_agent", "anthropic/claude-opus-4-6", 100), 1.0),
        ];

        let ranked = rank_memories(memories, &config);
        assert_eq!(ranked[0].memory.id, "b"); // opus (tier1) > llama (tier4)
    }

    #[test]
    fn rank_prefers_newer_on_tie() {
        let config = test_config();
        let memories = vec![
            (make_memory("a", "trusted_agent", "anthropic/claude-opus-4-6", 100), 1.0),
            (make_memory("b", "trusted_agent", "anthropic/claude-opus-4-6", 200), 1.0),
        ];

        let ranked = rank_memories(memories, &config);
        assert_eq!(ranked[0].memory.id, "b"); // newer wins
    }

    #[test]
    fn unknown_source_gets_zero_trust() {
        let config = test_config();
        let memories = vec![
            (make_memory("a", "unknown_agent", "anthropic/claude-opus-4-6", 100), 1.0),
        ];

        let ranked = rank_memories(memories, &config);
        assert_eq!(ranked[0].effective_score, 0.0);
    }

    #[test]
    fn wildcard_model_matching() {
        let config = test_config();
        let tier = super::model_tier("meta/llama-70b", &config);
        assert_eq!(tier, 4);

        let tier = super::model_tier("local/my-model", &config);
        assert_eq!(tier, 4);
    }

    #[test]
    fn detect_conflicts_same_topic_different_sources() {
        let m1 = make_memory("a", "agent1", "anthropic/claude-opus-4-6", 100);
        let m2 = make_memory("b", "agent2", "anthropic/claude-opus-4-6", 200);
        let m3 = make_memory("c", "agent1", "anthropic/claude-opus-4-6", 300);

        let conflicts = detect_conflicts(&[m1, m2, m3]);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].topic, "test/topic");
        assert_eq!(conflicts[0].memories.len(), 3);
    }

    #[test]
    fn no_conflicts_same_source() {
        let m1 = make_memory("a", "agent1", "anthropic/claude-opus-4-6", 100);
        let m2 = make_memory("b", "agent1", "anthropic/claude-opus-4-6", 200);

        let conflicts = detect_conflicts(&[m1, m2]);
        assert_eq!(conflicts.len(), 0);
    }

    #[test]
    fn resolve_conflict_picks_best() {
        let config = test_config();
        let conflict = Conflict {
            topic: "test/topic".to_string(),
            memories: vec![
                make_memory("a", "community_agent", "meta/llama-70b", 100),
                make_memory("b", "trusted_agent", "anthropic/claude-opus-4-6", 50),
            ],
        };

        let winner = resolve_conflict(&conflict, &config);
        assert_eq!(winner, Some(1)); // trusted_agent + opus wins
    }
}
