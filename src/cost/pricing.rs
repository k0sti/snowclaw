/// Return (input_price_per_million, output_price_per_million) for known models.
/// Returns (0.0, 0.0) for unknown models — cost will be recorded as $0.
pub fn model_pricing(model: &str) -> (f64, f64) {
    // Normalize: strip provider prefix (e.g. "anthropic/claude-sonnet-4-20250514" → "claude-sonnet-4-20250514")
    let name = model
        .rsplit_once('/')
        .map(|(_, m)| m)
        .unwrap_or(model);

    match name {
        // Anthropic Claude 4.x / Opus
        n if n.starts_with("claude-opus-4") => (15.0, 75.0),
        // Anthropic Claude 4.x / Sonnet
        n if n.starts_with("claude-sonnet-4") => (3.0, 15.0),
        // Anthropic Claude 3.5 Sonnet
        n if n.starts_with("claude-3-5-sonnet") || n.starts_with("claude-3.5-sonnet") => {
            (3.0, 15.0)
        }
        // Anthropic Claude 3.5 Haiku
        n if n.starts_with("claude-3-5-haiku") || n.starts_with("claude-3.5-haiku") => {
            (0.80, 4.0)
        }
        // Anthropic Claude 3 Opus
        n if n.starts_with("claude-3-opus") => (15.0, 75.0),
        // Anthropic Claude 3 Sonnet
        n if n.starts_with("claude-3-sonnet") => (3.0, 15.0),
        // Anthropic Claude 3 Haiku
        n if n.starts_with("claude-3-haiku") => (0.25, 1.25),
        // OpenAI GPT-4o
        n if n.starts_with("gpt-4o") && !n.contains("mini") => (2.50, 10.0),
        // OpenAI GPT-4o mini
        n if n.starts_with("gpt-4o-mini") => (0.15, 0.60),
        // OpenAI GPT-4 Turbo
        n if n.starts_with("gpt-4-turbo") => (10.0, 30.0),
        // OpenAI GPT-4
        n if n == "gpt-4" || n.starts_with("gpt-4-0") => (30.0, 60.0),
        // OpenAI o1
        n if n.starts_with("o1") && !n.starts_with("o1-mini") => (15.0, 60.0),
        // OpenAI o1-mini
        n if n.starts_with("o1-mini") => (3.0, 12.0),
        // Google Gemini 2.0 Flash
        n if n.starts_with("gemini-2.0-flash") => (0.10, 0.40),
        // Google Gemini 1.5 Pro
        n if n.starts_with("gemini-1.5-pro") => (1.25, 5.0),
        // Google Gemini 1.5 Flash
        n if n.starts_with("gemini-1.5-flash") => (0.075, 0.30),
        // DeepSeek V3/Chat
        n if n.starts_with("deepseek-chat") || n.starts_with("deepseek-v3") => (0.27, 1.10),
        // DeepSeek R1
        n if n.starts_with("deepseek-r1") || n.starts_with("deepseek-reasoner") => (0.55, 2.19),
        // Unknown model — record usage without cost
        _ => (0.0, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_model_prices() {
        let (inp, out) = model_pricing("claude-sonnet-4-20250514");
        assert!(inp > 0.0);
        assert!(out > 0.0);
    }

    #[test]
    fn prefixed_model_prices() {
        let (inp, out) = model_pricing("anthropic/claude-opus-4-20250514");
        assert_eq!(inp, 15.0);
        assert_eq!(out, 75.0);
    }

    #[test]
    fn unknown_model_returns_zero() {
        let (inp, out) = model_pricing("some-unknown-model");
        assert_eq!(inp, 0.0);
        assert_eq!(out, 0.0);
    }
}
