use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDecision {
    None,
    SoftWarning,
    HardStop,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BudgetState {
    pub soft_budget_usd: Option<f64>,
    pub hard_budget_usd: Option<f64>,
    pub warning_emitted: bool,
    pub hard_limit_reached: bool,
    pub total_cost_usd: f64,
}

#[derive(Debug, Clone)]
pub struct BudgetTracker {
    state: BudgetState,
}

impl BudgetTracker {
    pub fn new(soft_budget_usd: Option<f64>, hard_budget_usd: Option<f64>) -> Self {
        Self {
            state: BudgetState {
                soft_budget_usd,
                hard_budget_usd,
                warning_emitted: false,
                hard_limit_reached: false,
                total_cost_usd: 0.0,
            },
        }
    }

    pub fn from_state(state: BudgetState) -> Self {
        Self { state }
    }

    pub fn apply_cost(&mut self, delta_cost_usd: f64) -> BudgetDecision {
        self.state.total_cost_usd += delta_cost_usd;

        if let Some(hard_budget) = self.state.hard_budget_usd {
            if !self.state.hard_limit_reached && self.state.total_cost_usd >= hard_budget {
                self.state.hard_limit_reached = true;
                if let Some(soft_budget) = self.state.soft_budget_usd {
                    if self.state.total_cost_usd >= soft_budget {
                        self.state.warning_emitted = true;
                    }
                }
                return BudgetDecision::HardStop;
            }
        }

        if let Some(soft_budget) = self.state.soft_budget_usd {
            if !self.state.warning_emitted && self.state.total_cost_usd >= soft_budget {
                self.state.warning_emitted = true;
                return BudgetDecision::SoftWarning;
            }
        }

        BudgetDecision::None
    }

    pub fn state(&self) -> BudgetState {
        self.state.clone()
    }

    pub fn is_hard_stopped(&self) -> bool {
        self.state.hard_limit_reached
    }
}

#[cfg(test)]
mod tests {
    use super::{BudgetDecision, BudgetTracker};

    #[test]
    fn hard_budget_blocks_future_submissions_after_threshold() {
        let mut budget = BudgetTracker::new(Some(1.0), Some(2.0));

        assert_eq!(budget.apply_cost(1.2), BudgetDecision::SoftWarning);
        assert_eq!(budget.apply_cost(0.9), BudgetDecision::HardStop);
        assert!(budget.is_hard_stopped());
    }
}
