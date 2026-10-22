//! Cost accounting and budget enforcement.
//!
//! Token accounting (U2) made the *usage* real; this makes the *bill* real. A
//! harness that can iterate for hours needs to know what it is spending and to
//! stop before it spends too much — a cost gate is the difference between "the
//! agent ran overnight" and "the agent ran overnight and cost $400".
//!
//! The price table is host-supplied (providers differ, and prices change); the
//! accumulator and the budget are framework mechanisms.

use std::collections::HashMap;

use crate::adapter::Usage;

/// A per-model price table, in USD per million tokens.
#[derive(Debug, Clone, PartialEq)]
pub struct Pricing {
    /// USD per million prompt/input tokens.
    pub input_per_million: f64,
    /// USD per million completion/output tokens.
    pub output_per_million: f64,
    /// USD per million cached-input tokens, when the provider charges less.
    pub cached_input_per_million: Option<f64>,
}

impl Pricing {
    /// A price table with no cached-input discount.
    #[must_use]
    pub fn new(input_per_million: f64, output_per_million: f64) -> Self {
        Self {
            input_per_million,
            output_per_million,
            cached_input_per_million: None,
        }
    }

    /// Set the cached-input price.
    #[must_use]
    pub fn with_cached_input(mut self, cached_input_per_million: f64) -> Self {
        self.cached_input_per_million = Some(cached_input_per_million);
        self
    }

    /// The cost of one usage record under this table.
    ///
    /// `Usage` reports prompt and completion tokens; a provider that charges
    /// less for cache hits would be modelled by a `Usage` that splits them.
    #[must_use]
    pub fn cost_of(&self, usage: &Usage) -> f64 {
        let input = f64::from(usage.prompt_tokens) / 1e6 * self.input_per_million;
        let output = f64::from(usage.completion_tokens) / 1e6 * self.output_per_million;
        input + output
    }
}

/// Usage and cost accumulated for one model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelAccount {
    /// Prompt/input tokens.
    pub prompt_tokens: u64,
    /// Completion/output tokens.
    pub completion_tokens: u64,
    /// Number of requests.
    pub requests: u64,
    /// Estimated cost in USD.
    pub cost_usd: f64,
}

/// Accumulates usage per model and computes cost.
#[derive(Debug, Clone, Default)]
pub struct UsageAccount {
    per_model: HashMap<String, ModelAccount>,
    total_cost_usd: f64,
    requests: u64,
}

/// What a budget check decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDecision {
    /// Spend is within budget.
    Within,
    /// The budget is exhausted; further spend should be refused.
    Exhausted,
}

impl UsageAccount {
    /// An empty account.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one request's usage under `pricing`.
    pub fn record(&mut self, model: &str, usage: &Usage, pricing: &Pricing) -> f64 {
        let cost = pricing.cost_of(usage);
        let entry = self.per_model.entry(model.to_string()).or_default();
        entry.prompt_tokens += u64::from(usage.prompt_tokens);
        entry.completion_tokens += u64::from(usage.completion_tokens);
        entry.requests += 1;
        entry.cost_usd += cost;
        self.total_cost_usd += cost;
        self.requests += 1;
        cost
    }

    /// The accumulated account for one model.
    #[must_use]
    pub fn model(&self, model: &str) -> Option<&ModelAccount> {
        self.per_model.get(model)
    }

    /// Every model's account, sorted by model name.
    #[must_use]
    pub fn models(&self) -> Vec<(String, ModelAccount)> {
        let mut out: Vec<(String, ModelAccount)> = self
            .per_model
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Total estimated cost in USD.
    #[must_use]
    pub fn total_cost_usd(&self) -> f64 {
        self.total_cost_usd
    }

    /// Total requests recorded.
    #[must_use]
    pub fn requests(&self) -> u64 {
        self.requests
    }

    /// Total tokens across every model.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.per_model
            .values()
            .map(|m| m.prompt_tokens + m.completion_tokens)
            .sum()
    }

    /// Check the spend against a budget.
    ///
    /// A non-positive budget means "no limit" (the default for a harness that
    /// is not cost-gated); a positive budget is exhausted once spend reaches it.
    #[must_use]
    pub fn check_budget(&self, budget_usd: f64) -> BudgetDecision {
        if budget_usd > 0.0 && self.total_cost_usd >= budget_usd {
            BudgetDecision::Exhausted
        } else {
            BudgetDecision::Within
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(prompt: u32, completion: u32) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
        }
    }

    #[test]
    fn cost_is_pricing_times_tokens() {
        // $3 / $15 per million (a typical mid-range model).
        let pricing = Pricing::new(3.0, 15.0);
        let cost = pricing.cost_of(&usage(1_000_000, 1_000_000));
        assert!((cost - 18.0).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn usage_accumulates_per_model() {
        let pricing = Pricing::new(3.0, 15.0);
        let mut account = UsageAccount::new();
        account.record("model-a", &usage(1_000_000, 0), &pricing);
        account.record("model-a", &usage(1_000_000, 0), &pricing);
        account.record("model-b", &usage(0, 1_000_000), &pricing);

        assert_eq!(account.requests(), 3);
        assert_eq!(account.model("model-a").unwrap().requests, 2);
        assert_eq!(account.model("model-a").unwrap().prompt_tokens, 2_000_000);
        assert_eq!(
            account.model("model-b").unwrap().completion_tokens,
            1_000_000
        );
        // 2 × $3 + 1 × $15 = $21.
        assert!((account.total_cost_usd() - 21.0).abs() < 1e-9);
        assert_eq!(account.total_tokens(), 3_000_000);
        // Sorted by model name.
        let models: Vec<String> = account.models().into_iter().map(|(k, _)| k).collect();
        assert_eq!(models, vec!["model-a", "model-b"]);
    }

    #[test]
    fn the_budget_gate_trips_once_spend_reaches_it() {
        let pricing = Pricing::new(3.0, 15.0);
        let mut account = UsageAccount::new();
        account.record("m", &usage(1_000_000, 0), &pricing); // $3
        assert_eq!(account.check_budget(10.0), BudgetDecision::Within);
        account.record("m", &usage(0, 1_000_000), &pricing); // +$15 = $18
        assert_eq!(account.check_budget(10.0), BudgetDecision::Exhausted);
        // A zero budget is "no limit".
        assert_eq!(account.check_budget(0.0), BudgetDecision::Within);
    }
}
