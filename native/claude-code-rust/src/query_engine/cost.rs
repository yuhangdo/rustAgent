use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::agent_runtime::AgentUsageRecord;
use crate::api::Usage;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub total_cost_usd: f64,
    pub call_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionUsageTotals {
    pub total_tokens: usize,
    pub total_cost_usd: f64,
    pub model_usage: BTreeMap<String, ModelUsage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageRecord {
    pub model: String,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub cost_usd: f64,
    pub usage_missing: bool,
}

#[derive(Debug, Clone, Copy)]
struct ModelPricing {
    input_per_million_usd: f64,
    output_per_million_usd: f64,
}

impl SessionUsageTotals {
    pub fn record_call(
        &mut self,
        model: impl Into<String>,
        prompt_tokens: usize,
        completion_tokens: usize,
        total_tokens: usize,
        cost_usd: f64,
    ) {
        let model = model.into();
        self.total_tokens += total_tokens;
        self.total_cost_usd += cost_usd;

        let entry = self.model_usage.entry(model).or_default();
        entry.prompt_tokens += prompt_tokens;
        entry.completion_tokens += completion_tokens;
        entry.total_tokens += total_tokens;
        entry.total_cost_usd += cost_usd;
        entry.call_count += 1;
    }
}

pub fn usage_record_for_model(model: &str, usage: Option<&Usage>) -> UsageRecord {
    let Some(usage) = usage else {
        return UsageRecord {
            model: model.to_string(),
            usage_missing: true,
            ..UsageRecord::default()
        };
    };

    let pricing = pricing_for_model(model);
    let cost_usd = ((usage.prompt_tokens as f64 / 1_000_000.0) * pricing.input_per_million_usd)
        + ((usage.completion_tokens as f64 / 1_000_000.0) * pricing.output_per_million_usd);

    UsageRecord {
        model: model.to_string(),
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        cost_usd,
        usage_missing: false,
    }
}

pub fn usage_record_from_agent_usage(model: &str, usage: &AgentUsageRecord) -> UsageRecord {
    let pricing = pricing_for_model(model);
    let cost_usd = ((usage.prompt_tokens as f64 / 1_000_000.0) * pricing.input_per_million_usd)
        + ((usage.completion_tokens as f64 / 1_000_000.0) * pricing.output_per_million_usd);

    UsageRecord {
        model: model.to_string(),
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        cost_usd,
        usage_missing: usage.usage_missing,
    }
}

fn pricing_for_model(model: &str) -> ModelPricing {
    let normalized = model.to_lowercase();
    if normalized.contains("opus") {
        return ModelPricing {
            input_per_million_usd: 15.0,
            output_per_million_usd: 75.0,
        };
    }

    if normalized.contains("haiku") {
        return ModelPricing {
            input_per_million_usd: 0.25,
            output_per_million_usd: 1.25,
        };
    }

    if normalized.contains("sonnet") {
        return ModelPricing {
            input_per_million_usd: 3.0,
            output_per_million_usd: 15.0,
        };
    }

    ModelPricing {
        input_per_million_usd: 0.0,
        output_per_million_usd: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::SessionUsageTotals;

    #[test]
    fn records_costs_per_model_and_session_total() {
        let mut usage = SessionUsageTotals::default();

        usage.record_call("sonnet", 1000, 200, 1200, 0.012);
        usage.record_call("opus", 500, 300, 800, 0.021);

        assert_eq!(usage.total_tokens, 2000);
        assert!((usage.total_cost_usd - 0.033).abs() < 0.0001);
        assert_eq!(usage.model_usage["sonnet"].call_count, 1);
        assert_eq!(usage.model_usage["opus"].total_tokens, 800);
    }
}
