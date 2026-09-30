use std::{collections::HashMap, fmt::Display, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::{
    confidence::{ConfidenceConfig, ConfidenceService},
    state_tracker::StageContext,
};

/// Governs budget initialization and ask decision policies.
#[derive(Debug, Clone)]
pub struct BudgetPolicy {
    config: BudgetConfig,
    confidence_service: Arc<ConfidenceService>,
}

impl BudgetPolicy {
    pub fn new(config: BudgetConfig) -> Self {
        let mut confidence_config = ConfidenceConfig::default();
        confidence_config.slope_window_size = config.confidence_slope_window;
        Self::with_confidence_service(config, Arc::new(ConfidenceService::new(confidence_config)))
    }

    pub fn with_confidence_service(
        config: BudgetConfig,
        confidence_service: Arc<ConfidenceService>,
    ) -> Self {
        if confidence_service.config().slope_window_size != config.confidence_slope_window {
            tracing::debug!(
                window = config.confidence_slope_window,
                service_window = confidence_service.config().slope_window_size,
                "BudgetPolicy configured with slope window that differs from ConfidenceService; service configuration takes precedence"
            );
        }
        Self {
            config,
            confidence_service,
        }
    }

    pub fn config(&self) -> &BudgetConfig {
        &self.config
    }

    pub fn confidence_service(&self) -> Arc<ConfidenceService> {
        Arc::clone(&self.confidence_service)
    }

    /// Initialize the ask budget based on task complexity.
    pub fn initialize_budget(&self, task_complexity: TaskComplexity) -> f64 {
        let base = self.config.initial_budget.max(1.0);
        let multiplier = match task_complexity {
            TaskComplexity::Simple => 1.0,
            TaskComplexity::Moderate => 2.0,
            TaskComplexity::Complex => 3.0,
        };
        (base * multiplier).max(1.0)
    }

    /// Determine whether we should ask the user for clarification.
    pub fn should_ask(
        &self,
        stage: StageContext,
        current_budget: f64,
        confidence_history: &[(DateTime<Utc>, f64)],
        last_ask_timestamp: Option<DateTime<Utc>>,
    ) -> AskDecision {
        let stage_thresholds = self.config.thresholds_for(stage);
        let slope = self
            .confidence_service
            .calculate_confidence_slope(confidence_history);
        let slope_triggered = slope
            .map(|trend| trend <= stage_thresholds.slope_threshold)
            .unwrap_or(false);

        let recency = last_ask_timestamp.map(|ts| Utc::now() - ts);
        let is_sequential = recency
            .map(|duration| duration < Duration::minutes(5))
            .unwrap_or(false);

        let recommended_channel = self.recommend_channel(is_sequential);
        let min_cost = self.calculate_cost(recommended_channel, is_sequential);
        let reserve = stage_thresholds.min_budget_reserve.max(0.0);
        let available_budget = (current_budget - reserve).max(0.0);
        let has_budget = available_budget + f64::EPSILON >= min_cost;

        let plateaued = slope.map(|trend| trend.abs() < 1e-6).unwrap_or(false);
        let latest_confidence = confidence_history
            .last()
            .map(|(_, score)| *score)
            .unwrap_or(0.0);
        let low_confidence = latest_confidence < stage_thresholds.low_confidence_floor;

        let planning_stage = matches!(
            stage,
            StageContext::PlanningBootstrap | StageContext::PlanningIteration
        );

        // Execution stage clarifications are about missing data needed to proceed,
        // not about confidence levels. If we have budget, allow the ask.
        let execution_stage =
            matches!(stage, StageContext::ExecutionCycle | StageContext::FollowUp);

        let should_ask = has_budget
            && (execution_stage // Execution questions bypass confidence checks - they're about missing data
                || slope_triggered
                || (low_confidence && (plateaued || planning_stage))
                || (planning_stage
                    && !confidence_history.is_empty()
                    && latest_confidence < (stage_thresholds.low_confidence_floor - 0.05)));

        let mut reasons: Vec<String> = Vec::new();
        if !has_budget {
            reasons.push(format!(
                "Insufficient stage-adjusted budget ({available_budget:.2} after reserving {reserve:.2}) for minimum ask cost ({min_cost:.2})"
            ));
        }
        if execution_stage && has_budget {
            reasons.push(format!(
                "Execution stage {} - asking for required data (confidence checks bypassed)",
                stage
            ));
        }
        if slope_triggered {
            if let Some(trend) = slope {
                reasons.push(format!(
                    "Confidence slope {trend:.4} below {stage} threshold {trigger:.4}",
                    stage = stage,
                    trigger = stage_thresholds.slope_threshold
                ));
            }
        }
        if low_confidence && plateaued && !slope_triggered && !execution_stage {
            reasons.push(format!(
                "Confidence stalled below {:.2} for stage {}",
                stage_thresholds.low_confidence_floor, stage
            ));
        } else if low_confidence && planning_stage && !slope_triggered {
            reasons.push(format!(
                "Planning confidence {:.2} remains below the {:.2} stage floor",
                latest_confidence, stage_thresholds.low_confidence_floor
            ));
        }
        if is_sequential {
            reasons.push("Sequential ask penalty applied".to_string());
        }
        if reasons.is_empty() {
            reasons.push(format!(
                "Confidence trending positively for stage {}",
                stage
            ));
        }

        let urgency = if should_ask {
            let slope_component = slope.map(|trend| (-trend).max(0.0)).unwrap_or(0.0);
            let budget_pressure = if current_budget <= min_cost * 2.0 {
                0.6
            } else {
                0.3
            };
            (0.4 + slope_component + budget_pressure).min(1.0).max(0.0)
        } else {
            0.0
        };

        AskDecision {
            should_ask,
            reason: reasons.join("; "),
            urgency,
            recommended_channel,
            estimated_cost: min_cost,
            sequential_penalty: is_sequential,
        }
    }

    /// Calculate the cost for sending an ask over the given channel.
    pub fn calculate_cost(&self, channel: Channel, is_sequential_ask: bool) -> f64 {
        let channel_penalty = self
            .config
            .penalty_factor_channel
            .get(&channel)
            .copied()
            .unwrap_or(1.0);
        let recency = if is_sequential_ask {
            self.config.penalty_factor_recency.max(1.0)
        } else {
            1.0
        };
        channel_penalty * recency
    }

    /// Determine whether a confidence improvement warrants budget replenishment.
    pub fn should_replenish(&self, old_confidence: f64, new_confidence: f64) -> Option<f64> {
        let delta = (new_confidence - old_confidence).max(0.0);
        if delta >= self.config.replenish_threshold {
            let replenish_base = self.config.initial_budget.max(1.0);
            let replenish_amount = (replenish_base * self.config.replenish_amount).max(0.0);
            if replenish_amount > 0.0 {
                Some(replenish_amount)
            } else {
                None
            }
        } else {
            None
        }
    }

    fn recommend_channel(&self, is_sequential: bool) -> Channel {
        Channel::all()
            .into_iter()
            .min_by(|a, b| {
                let cost_a = self.calculate_cost(*a, is_sequential);
                let cost_b = self.calculate_cost(*b, is_sequential);
                cost_a
                    .partial_cmp(&cost_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(Channel::InApp)
    }
}

/// Configuration inputs for the budget policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    pub initial_budget: f64,
    pub confidence_slope_window: usize,
    pub slope_threshold: f64,
    pub penalty_factor_recency: f64,
    pub penalty_factor_channel: HashMap<Channel, f64>,
    pub replenish_threshold: f64,
    pub replenish_amount: f64,
    #[serde(default = "default_stage_threshold_map")]
    pub stage_thresholds: HashMap<StageContext, StageBudgetThresholds>,
    #[serde(default = "default_stage_threshold")]
    pub default_stage_thresholds: StageBudgetThresholds,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        let mut penalty_factor_channel = HashMap::new();
        penalty_factor_channel.insert(Channel::InApp, 1.0);
        penalty_factor_channel.insert(Channel::PushNotification, 1.3);
        penalty_factor_channel.insert(Channel::Email, 1.1);

        Self {
            initial_budget: 150.0,
            confidence_slope_window: 5,
            slope_threshold: -0.03,
            penalty_factor_recency: 1.1,
            penalty_factor_channel,
            replenish_threshold: 0.1,
            replenish_amount: 0.3,
            stage_thresholds: default_stage_threshold_map(),
            default_stage_thresholds: default_stage_threshold(),
        }
    }
}

impl BudgetConfig {
    pub fn thresholds_for(&self, stage: StageContext) -> StageBudgetThresholds {
        self.stage_thresholds
            .get(&stage)
            .cloned()
            .unwrap_or_else(|| StageBudgetThresholds {
                slope_threshold: self.slope_threshold,
                ..self.default_stage_thresholds.clone()
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageBudgetThresholds {
    pub slope_threshold: f64,
    pub low_confidence_floor: f64,
    pub min_budget_reserve: f64,
}

fn default_stage_threshold() -> StageBudgetThresholds {
    StageBudgetThresholds {
        slope_threshold: -0.03,
        low_confidence_floor: 0.45,
        min_budget_reserve: 0.0,
    }
}

fn default_stage_threshold_map() -> HashMap<StageContext, StageBudgetThresholds> {
    let mut map = HashMap::new();
    map.insert(
        StageContext::PlanningBootstrap,
        StageBudgetThresholds {
            slope_threshold: -0.02,
            low_confidence_floor: 0.80,
            min_budget_reserve: 0.0,
        },
    );
    map.insert(
        StageContext::PlanningIteration,
        StageBudgetThresholds {
            slope_threshold: -0.025,
            low_confidence_floor: 0.5,
            min_budget_reserve: 0.0,
        },
    );
    map.insert(
        StageContext::ExecutionCycle,
        StageBudgetThresholds {
            slope_threshold: -0.05,
            low_confidence_floor: 0.4,
            min_budget_reserve: 0.0,
        },
    );
    map.insert(
        StageContext::FollowUp,
        StageBudgetThresholds {
            slope_threshold: -0.045,
            low_confidence_floor: 0.38,
            min_budget_reserve: 0.0,
        },
    );
    map
}

/// Communication channel for the ask loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    InApp,
    PushNotification,
    Email,
}

impl Channel {
    pub fn all() -> [Channel; 3] {
        [Channel::InApp, Channel::PushNotification, Channel::Email]
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "in_app" => Some(Channel::InApp),
            "push_notification" => Some(Channel::PushNotification),
            "email" => Some(Channel::Email),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Channel::InApp => "in_app",
            Channel::PushNotification => "push_notification",
            Channel::Email => "email",
        }
    }
}

impl Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Complexity bucket for initializing the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskComplexity {
    Simple,
    Moderate,
    Complex,
}

/// Outcome of the budget decision when considering an ask.
#[derive(Debug, Clone)]
pub struct AskDecision {
    pub should_ask: bool,
    pub reason: String,
    pub urgency: f64,
    pub recommended_channel: Channel,
    pub estimated_cost: f64,
    pub sequential_penalty: bool,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::state_tracker::StageContext;

    #[test]
    fn initializes_budget_based_on_complexity() {
        let policy = BudgetPolicy::new(BudgetConfig::default());
        // Simple: 1.0x multiplier (100.0)
        assert_eq!(
            policy.initialize_budget(TaskComplexity::Simple),
            policy.config.initial_budget
        );
        // Moderate: 2.0x multiplier (200.0)
        assert_eq!(
            policy.initialize_budget(TaskComplexity::Moderate),
            policy.config.initial_budget * 2.0
        );
        // Complex: 3.0x multiplier (300.0)
        assert_eq!(
            policy.initialize_budget(TaskComplexity::Complex),
            policy.config.initial_budget * 3.0
        );
    }

    #[test]
    fn detects_negative_confidence_slope() {
        let policy = BudgetPolicy::new(BudgetConfig::default());
        let start = Utc::now();
        let history = vec![
            (start, 0.8),
            (start + Duration::seconds(10), 0.35),
            (start + Duration::seconds(20), 0.1),
        ];
        let decision = policy.should_ask(StageContext::PlanningBootstrap, 10.0, &history, None);
        assert!(decision.should_ask);
        assert!(decision.reason.contains("Confidence slope"));
    }

    #[test]
    fn blocks_when_budget_insufficient() {
        let policy = BudgetPolicy::new(BudgetConfig::default());
        let start = Utc::now();
        let history = vec![(start, 0.6), (start + Duration::seconds(10), 0.59)];
        let decision = policy.should_ask(StageContext::PlanningBootstrap, 0.6, &history, None);
        assert!(!decision.should_ask);
        assert!(decision.reason.contains("Insufficient"));
    }

    #[test]
    fn execution_stage_bypasses_confidence_checks() {
        let policy = BudgetPolicy::new(BudgetConfig::default());
        let start = Utc::now();
        // Stable confidence that wouldn't trigger planning-stage ask
        let history = vec![
            (start, 0.5),
            (start + Duration::seconds(10), 0.5),
            (start + Duration::seconds(20), 0.5),
        ];

        // Planning stage with stable confidence: should ask because confidence is low
        let planning_decision =
            policy.should_ask(StageContext::PlanningBootstrap, 5.0, &history, None);
        assert!(planning_decision.should_ask);

        // Execution stage: should ask because execution bypasses confidence checks
        // (execution questions are about missing data, not confidence levels)
        let execution_decision =
            policy.should_ask(StageContext::ExecutionCycle, 5.0, &history, None);
        assert!(
            execution_decision.should_ask,
            "execution stage should ask if budget available, regardless of confidence"
        );
        assert!(
            execution_decision
                .reason
                .contains("Execution stage")
                .then_some(())
                .or_else(|| execution_decision
                    .reason
                    .contains("confidence")
                    .then_some(()))
                .is_some(),
            "reason should mention execution stage or confidence: {}",
            execution_decision.reason
        );
    }

    #[test]
    fn replenishes_on_confidence_gain() {
        let policy = BudgetPolicy::new(BudgetConfig::default());
        assert!(policy.should_replenish(0.4, 0.6).unwrap().abs() > f64::EPSILON);
        assert!(policy.should_replenish(0.5, 0.52).is_none());
    }
}
