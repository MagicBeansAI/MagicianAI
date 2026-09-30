//! Pure worker/queue/byte capacity plan for the legacy dispatch pool.
//!
//! This module has no runtime side effects. `LlmDispatchQueue::start` still
//! reads `DispatchConfig` integers. Magician's pre-bootstrap resolver (PR8)
//! inherits these tables when `runtime.scale.overrides.*` is `null`.
//!
//! `DISPATCH_CAPACITY_LEGACY_THREE` is the historical seed that created the
//! three-call ceiling. Tests keep it so a regression to those numbers is
//! obvious.

use super::config::DispatchConfig;

/// Execution engine for the dispatch queue.
///
/// [`DispatchEngine::LegacyWorkerPool`] runs provider HTTP inside `process_job`
/// and therefore ceilings concurrent HTTP to the scheduler worker count.
/// [`DispatchEngine::ProviderIsolated`] hands HTTP to a per-attempt executor
/// after local-prep and per-provider admission, so scheduler workers do not
/// hold the global slot across provider HTTP. Magician YAML seed is
/// `provider_isolated`; `legacy_worker_pool` remains the in-crate default and
/// the restart-bound rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DispatchEngine {
    LegacyWorkerPool,
    ProviderIsolated,
}

impl DispatchEngine {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LegacyWorkerPool => "legacy_worker_pool",
            Self::ProviderIsolated => "provider_isolated",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "legacy_worker_pool" => Some(Self::LegacyWorkerPool),
            "provider_isolated" => Some(Self::ProviderIsolated),
            _ => None,
        }
    }
}

impl Default for DispatchEngine {
    fn default() -> Self {
        Self::LegacyWorkerPool
    }
}

impl serde::Serialize for DispatchEngine {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for DispatchEngine {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).ok_or_else(|| {
            serde::de::Error::unknown_variant(&value, &["legacy_worker_pool", "provider_isolated"])
        })
    }
}

/// Boot-time worker, lane-entry, and retained-byte snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchCapacityPlan {
    pub workers: usize,
    pub reserved_interactive_workers: usize,
    pub queue_capacity_high: usize,
    pub queue_capacity_normal: usize,
    pub queue_capacity_background: usize,
    pub max_request_bytes: u64,
    pub queue_bytes_high: u64,
    pub queue_bytes_normal: u64,
    pub queue_bytes_background: u64,
    pub queue_bytes_global: u64,
}

impl DispatchCapacityPlan {
    /// Historical YAML seed that could not occupy OpenAI 4 and Anthropic 4
    /// together because only three global worker tasks existed.
    pub const LEGACY_THREE: Self = Self {
        workers: 3,
        reserved_interactive_workers: 2,
        queue_capacity_high: 100,
        queue_capacity_normal: 150,
        queue_capacity_background: 64,
        max_request_bytes: 64 * 1024 * 1024,
        queue_bytes_high: 256 * 1024 * 1024,
        queue_bytes_normal: 512 * 1024 * 1024,
        queue_bytes_background: 512 * 1024 * 1024,
        queue_bytes_global: 768 * 1024 * 1024,
    };

    /// PR5 `current` canary. Sized so unsaturated cloud providers can occupy
    /// their configured caps at once, interactive workers stay reserved, and
    /// a 200–300 normal-priority burst is not rejected before providers
    /// matter. Byte caps stay at the previous seed to avoid a silent RSS
    /// jump. This is not `m2_max` (24 workers).
    pub const CURRENT: Self = Self {
        workers: 12,
        reserved_interactive_workers: 4,
        queue_capacity_high: 200,
        queue_capacity_normal: 400,
        queue_capacity_background: 256,
        max_request_bytes: 64 * 1024 * 1024,
        queue_bytes_high: 256 * 1024 * 1024,
        queue_bytes_normal: 512 * 1024 * 1024,
        queue_bytes_background: 512 * 1024 * 1024,
        queue_bytes_global: 768 * 1024 * 1024,
    };

    /// Conservative 4-core (and smaller) host. Fewer worker tasks and lane
    /// entries than [`Self::CURRENT`]; retained-byte caps stay put.
    pub const SMALL_CPU: Self = Self {
        workers: 4,
        reserved_interactive_workers: 2,
        queue_capacity_high: 64,
        queue_capacity_normal: 64,
        queue_capacity_background: 32,
        max_request_bytes: 64 * 1024 * 1024,
        queue_bytes_high: 256 * 1024 * 1024,
        queue_bytes_normal: 512 * 1024 * 1024,
        queue_bytes_background: 512 * 1024 * 1024,
        queue_bytes_global: 768 * 1024 * 1024,
    };

    /// Explicit M2 Max canary. Larger than [`Self::CURRENT`] so flipping
    /// `runtime.scale.profile: m2_max` is not a silent no-op. This is not
    /// inferred from CPU count. Byte caps stay at the current seed.
    pub const M2_MAX: Self = Self {
        workers: 24,
        reserved_interactive_workers: 8,
        queue_capacity_high: 256,
        queue_capacity_normal: 384,
        queue_capacity_background: 512,
        max_request_bytes: 64 * 1024 * 1024,
        queue_bytes_high: 256 * 1024 * 1024,
        queue_bytes_normal: 512 * 1024 * 1024,
        queue_bytes_background: 512 * 1024 * 1024,
        queue_bytes_global: 768 * 1024 * 1024,
    };

    /// Explicit large-cloud host. Never inferred from hardware.
    pub const CLOUD_HEAVY: Self = Self {
        workers: 48,
        reserved_interactive_workers: 16,
        queue_capacity_high: 400,
        queue_capacity_normal: 800,
        queue_capacity_background: 1024,
        max_request_bytes: 64 * 1024 * 1024,
        queue_bytes_high: 256 * 1024 * 1024,
        queue_bytes_normal: 512 * 1024 * 1024,
        queue_bytes_background: 512 * 1024 * 1024,
        queue_bytes_global: 768 * 1024 * 1024,
    };

    pub fn from_config(config: &DispatchConfig) -> Self {
        Self {
            workers: config.workers,
            reserved_interactive_workers: config.reserved_interactive_workers,
            queue_capacity_high: config.queue_capacity_high,
            queue_capacity_normal: config.queue_capacity_normal,
            queue_capacity_background: config.queue_capacity_background,
            max_request_bytes: config.max_request_bytes,
            queue_bytes_high: config.queue_bytes_high,
            queue_bytes_normal: config.queue_bytes_normal,
            queue_bytes_background: config.queue_bytes_background,
            queue_bytes_global: config.queue_bytes_global,
        }
    }

    pub fn apply_to(self, config: &mut DispatchConfig) {
        config.workers = self.workers;
        config.reserved_interactive_workers = self.reserved_interactive_workers;
        config.queue_capacity_high = self.queue_capacity_high;
        config.queue_capacity_normal = self.queue_capacity_normal;
        config.queue_capacity_background = self.queue_capacity_background;
        config.max_request_bytes = self.max_request_bytes;
        config.queue_bytes_high = self.queue_bytes_high;
        config.queue_bytes_normal = self.queue_bytes_normal;
        config.queue_bytes_background = self.queue_bytes_background;
        config.queue_bytes_global = self.queue_bytes_global;
    }

    /// Workers allowed to drain the background lane. Matches
    /// `LlmDispatchQueue::start`: at least one background-capable worker.
    pub fn background_workers(self) -> usize {
        let workers = self.workers.max(1);
        let reserved = self
            .reserved_interactive_workers
            .min(workers.saturating_sub(1));
        workers.saturating_sub(reserved).max(1)
    }

    pub fn validate(self) -> Result<(), DispatchCapacityError> {
        if self.workers == 0 {
            return Err(DispatchCapacityError::Workers);
        }
        if self.queue_capacity_high == 0
            || self.queue_capacity_normal == 0
            || self.queue_capacity_background == 0
        {
            return Err(DispatchCapacityError::QueueCapacity);
        }
        if self.max_request_bytes == 0
            || self.queue_bytes_high == 0
            || self.queue_bytes_normal == 0
            || self.queue_bytes_background == 0
            || self.queue_bytes_global == 0
        {
            return Err(DispatchCapacityError::ByteCapacity);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchCapacityError {
    Workers,
    QueueCapacity,
    ByteCapacity,
}

impl std::fmt::Display for DispatchCapacityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workers => write!(f, "llm.dispatch.workers must be greater than 0"),
            Self::QueueCapacity => {
                write!(f, "llm.dispatch queue_capacity_* must be greater than 0")
            },
            Self::ByteCapacity => {
                write!(
                    f,
                    "llm.dispatch max_request_bytes and queue_bytes_* must be greater than 0"
                )
            },
        }
    }
}

impl std::error::Error for DispatchCapacityError {}

pub fn default_dispatch_engine() -> DispatchEngine {
    DispatchEngine::LegacyWorkerPool
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_is_strictly_larger_than_the_three_call_ceiling() {
        assert!(DispatchCapacityPlan::CURRENT.workers > DispatchCapacityPlan::LEGACY_THREE.workers);
        assert!(
            DispatchCapacityPlan::CURRENT.queue_capacity_normal
                > DispatchCapacityPlan::LEGACY_THREE.queue_capacity_normal
        );
        assert!(
            DispatchCapacityPlan::CURRENT.workers
                >= DispatchCapacityPlan::CURRENT.reserved_interactive_workers
        );
        assert!(DispatchCapacityPlan::CURRENT.background_workers() >= 1);
        assert!(
            DispatchCapacityPlan::CURRENT.workers >= 8,
            "current must be able to occupy OpenAI 4 + Anthropic 4 at once"
        );
        assert_eq!(
            DispatchCapacityPlan::CURRENT.max_request_bytes,
            64 * 1024 * 1024
        );
        assert_eq!(
            DispatchCapacityPlan::CURRENT.queue_bytes_global,
            DispatchCapacityPlan::LEGACY_THREE.queue_bytes_global,
            "PR5 does not raise retained-byte RSS"
        );
    }

    #[test]
    fn legacy_three_matches_the_historical_seed() {
        let plan = DispatchCapacityPlan::LEGACY_THREE;
        assert_eq!(plan.workers, 3);
        assert_eq!(plan.reserved_interactive_workers, 2);
        assert_eq!(plan.queue_capacity_high, 100);
        assert_eq!(plan.queue_capacity_normal, 150);
        assert_eq!(plan.queue_capacity_background, 64);
        assert_eq!(plan.background_workers(), 1);
    }

    #[test]
    fn current_keeps_interactive_reservation_and_background_headroom() {
        let plan = DispatchCapacityPlan::CURRENT;
        assert_eq!(plan.workers, 12);
        assert_eq!(plan.reserved_interactive_workers, 4);
        assert_eq!(plan.background_workers(), 8);
        assert_eq!(plan.queue_capacity_high, 200);
        assert_eq!(plan.queue_capacity_normal, 400);
        assert_eq!(plan.queue_capacity_background, 256);
        plan.validate().expect("current plan must be valid");
    }

    #[test]
    fn small_cpu_is_strictly_smaller_than_current() {
        let plan = DispatchCapacityPlan::SMALL_CPU;
        assert_eq!(plan.workers, 4);
        assert_eq!(plan.reserved_interactive_workers, 2);
        assert_eq!(plan.queue_capacity_normal, 64);
        assert!(plan.workers < DispatchCapacityPlan::CURRENT.workers);
        assert_eq!(
            plan.queue_bytes_global,
            DispatchCapacityPlan::CURRENT.queue_bytes_global
        );
        plan.validate().expect("small_cpu plan must be valid");
    }

    #[test]
    fn m2_max_is_strictly_larger_than_current_and_not_a_noop() {
        let plan = DispatchCapacityPlan::M2_MAX;
        assert_eq!(plan.workers, 24);
        assert_eq!(plan.reserved_interactive_workers, 8);
        assert_eq!(plan.queue_capacity_normal, 384);
        assert!(plan.workers > DispatchCapacityPlan::CURRENT.workers);
        assert_ne!(plan, DispatchCapacityPlan::CURRENT);
        assert_eq!(
            plan.queue_bytes_global,
            DispatchCapacityPlan::CURRENT.queue_bytes_global
        );
        plan.validate().expect("m2_max plan must be valid");
    }

    #[test]
    fn cloud_heavy_is_strictly_larger_than_m2_max() {
        let plan = DispatchCapacityPlan::CLOUD_HEAVY;
        assert!(plan.workers > DispatchCapacityPlan::M2_MAX.workers);
        assert!(plan.queue_capacity_normal > DispatchCapacityPlan::M2_MAX.queue_capacity_normal);
        plan.validate().expect("cloud_heavy plan must be valid");
    }

    #[test]
    fn apply_and_from_config_round_trip() {
        let mut config = DispatchConfig::default();
        DispatchCapacityPlan::CURRENT.apply_to(&mut config);
        assert_eq!(
            DispatchCapacityPlan::from_config(&config),
            DispatchCapacityPlan::CURRENT
        );
        DispatchCapacityPlan::LEGACY_THREE.apply_to(&mut config);
        assert_eq!(
            DispatchCapacityPlan::from_config(&config),
            DispatchCapacityPlan::LEGACY_THREE
        );
    }

    #[test]
    fn validate_rejects_zero_workers_and_caps() {
        let mut plan = DispatchCapacityPlan::CURRENT;
        plan.workers = 0;
        assert_eq!(plan.validate(), Err(DispatchCapacityError::Workers));
        plan = DispatchCapacityPlan::CURRENT;
        plan.queue_capacity_normal = 0;
        assert_eq!(plan.validate(), Err(DispatchCapacityError::QueueCapacity));
        plan = DispatchCapacityPlan::CURRENT;
        plan.queue_bytes_global = 0;
        assert_eq!(plan.validate(), Err(DispatchCapacityError::ByteCapacity));
    }

    #[test]
    fn reserved_equal_to_workers_still_leaves_one_background_worker() {
        let plan = DispatchCapacityPlan {
            workers: 4,
            reserved_interactive_workers: 4,
            ..DispatchCapacityPlan::CURRENT
        };
        assert_eq!(plan.background_workers(), 1);
        plan.validate().expect("clamp is a start-time contract");
    }

    #[test]
    fn engine_parses_known_names_and_rejects_unknown() {
        assert_eq!(
            DispatchEngine::parse("legacy_worker_pool"),
            Some(DispatchEngine::LegacyWorkerPool)
        );
        assert_eq!(
            DispatchEngine::parse("provider_isolated"),
            Some(DispatchEngine::ProviderIsolated)
        );
        assert_eq!(DispatchEngine::parse("threads"), None);
        let encoded = serde_json::to_string(&DispatchEngine::LegacyWorkerPool).unwrap();
        assert_eq!(encoded, "\"legacy_worker_pool\"");
        let decoded: DispatchEngine = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, DispatchEngine::LegacyWorkerPool);
    }
}
