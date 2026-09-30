//! Pre-bootstrap effective runtime plan (PR8).
//!
//! `null` / omitted [`RuntimeScaleOverrides`] inherit the selected profile.
//! An explicit integer wins. `MAGICIAN_SCALE_PROFILE` overrides the profile
//! *name* only. Mixing leftover current-era `llm.dispatch.workers` (`3` or
//! `12`) with an explicit non-`current` profile is a boot error.
//!
//! This module is pure: CPU count and env profile are injected. Magician-bin
//! loads config and resolves this plan before constructing main, execution,
//! or Lance runtimes.

use magicllm::dispatch::{DispatchCapacityPlan, DispatchConfig};
use serde::{Deserialize, Serialize};

/// Host class that owns Tokio widths, live-agent cap, and dispatch sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScaleProfile {
    /// Git-seed / live canary. Dispatch matches [`DispatchCapacityPlan::CURRENT`].
    #[default]
    Current,
    /// Conservative 4-core (and smaller) host.
    SmallCpu,
    /// Explicit M2 Max canary. Never inferred from CPU count.
    M2Max,
    /// Explicit large-cloud host. Never inferred from hardware.
    CloudHeavy,
    /// `cpu <= 4` → [`ScaleProfile::SmallCpu`], otherwise [`ScaleProfile::Current`].
    Auto,
}

impl ScaleProfile {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "current" => Some(Self::Current),
            "small_cpu" => Some(Self::SmallCpu),
            "m2_max" => Some(Self::M2Max),
            "cloud_heavy" => Some(Self::CloudHeavy),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::SmallCpu => "small_cpu",
            Self::M2Max => "m2_max",
            Self::CloudHeavy => "cloud_heavy",
            Self::Auto => "auto",
        }
    }

    /// True when leftover current-era dispatch scalars are allowed.
    pub const fn allows_current_era_dispatch_scalars(self) -> bool {
        matches!(self, Self::Current | Self::Auto)
    }
}

impl std::fmt::Display for ScaleProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Optional integers. `null` / omitted inherits the selected profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeScaleOverrides {
    #[serde(default)]
    pub main_worker_threads: Option<usize>,
    #[serde(default)]
    pub http_worker_threads: Option<usize>,
    #[serde(default)]
    pub execution_worker_threads: Option<usize>,
    #[serde(default)]
    pub background_worker_threads: Option<usize>,
    #[serde(default)]
    pub lance_worker_threads: Option<usize>,
    #[serde(default)]
    pub lance_search_cost_units: Option<usize>,
    /// `0` is unlimited (no semaphore wait). `null` inherits the profile.
    #[serde(default)]
    pub blocking_admission_permits: Option<usize>,
    #[serde(default)]
    pub live_agent_limit: Option<usize>,
    #[serde(default)]
    pub workers: Option<usize>,
    #[serde(default)]
    pub reserved_interactive_workers: Option<usize>,
    #[serde(default)]
    pub global_cloud_concurrency: Option<usize>,
    #[serde(default)]
    pub queue_capacity_high: Option<usize>,
    #[serde(default)]
    pub queue_capacity_normal: Option<usize>,
    #[serde(default)]
    pub queue_capacity_background: Option<usize>,
    #[serde(default)]
    pub max_request_bytes: Option<u64>,
    #[serde(default)]
    pub queue_bytes_high: Option<u64>,
    #[serde(default)]
    pub queue_bytes_normal: Option<u64>,
    #[serde(default)]
    pub queue_bytes_background: Option<u64>,
    #[serde(default)]
    pub queue_bytes_global: Option<u64>,
}

/// `runtime.scale` block. Default profile is [`ScaleProfile::Current`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeScaleSettings {
    #[serde(default)]
    pub profile: ScaleProfile,
    #[serde(default)]
    pub overrides: RuntimeScaleOverrides,
}

/// Parsed `llm.dispatch` integers used only to detect leftover current-era
/// YAML after serde has already filled concrete `usize` fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeftoverDispatchScalars {
    pub workers: usize,
    pub reserved_interactive_workers: usize,
    pub queue_capacity_high: usize,
    pub queue_capacity_normal: usize,
    pub queue_capacity_background: usize,
}

impl From<&DispatchConfig> for LeftoverDispatchScalars {
    fn from(config: &DispatchConfig) -> Self {
        Self {
            workers: config.workers,
            reserved_interactive_workers: config.reserved_interactive_workers,
            queue_capacity_high: config.queue_capacity_high,
            queue_capacity_normal: config.queue_capacity_normal,
            queue_capacity_background: config.queue_capacity_background,
        }
    }
}

/// Resolved integers used to construct runtimes and to overwrite
/// `llm.dispatch` worker/queue/byte knobs the profile owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveRuntimePlan {
    pub yaml_profile: ScaleProfile,
    pub env_profile: Option<ScaleProfile>,
    pub selected_profile: ScaleProfile,
    pub cpu_count: usize,
    pub main_worker_threads: usize,
    pub http_worker_threads: usize,
    pub execution_worker_threads: usize,
    pub background_worker_threads: usize,
    pub lance_worker_threads: usize,
    pub lance_search_cost_units: usize,
    pub blocking_admission_permits: usize,
    pub live_agent_limit: usize,
    pub workers: usize,
    pub reserved_interactive_workers: usize,
    pub global_cloud_concurrency: usize,
    pub queue_capacity_high: usize,
    pub queue_capacity_normal: usize,
    pub queue_capacity_background: usize,
    pub max_request_bytes: u64,
    pub queue_bytes_high: u64,
    pub queue_bytes_normal: u64,
    pub queue_bytes_background: u64,
    pub queue_bytes_global: u64,
}

impl EffectiveRuntimePlan {
    pub fn dispatch_capacity(self) -> DispatchCapacityPlan {
        DispatchCapacityPlan {
            workers: self.workers,
            reserved_interactive_workers: self.reserved_interactive_workers,
            queue_capacity_high: self.queue_capacity_high,
            queue_capacity_normal: self.queue_capacity_normal,
            queue_capacity_background: self.queue_capacity_background,
            max_request_bytes: self.max_request_bytes,
            queue_bytes_high: self.queue_bytes_high,
            queue_bytes_normal: self.queue_bytes_normal,
            queue_bytes_background: self.queue_bytes_background,
            queue_bytes_global: self.queue_bytes_global,
        }
    }

    pub fn apply_to_dispatch(self, config: &mut DispatchConfig) {
        self.dispatch_capacity().apply_to(config);
        config.global_cloud_concurrency = self.global_cloud_concurrency;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimePlanError {
    UnknownProfile(String),
    MixedLeftover {
        profile: ScaleProfile,
        leftover_workers: usize,
    },
    InvalidOverride {
        field: &'static str,
        value: u64,
    },
}

impl std::fmt::Display for RuntimePlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProfile(value) => write!(
                f,
                "unknown MAGICIAN_SCALE_PROFILE {value:?}; expected current, small_cpu, m2_max, cloud_heavy, or auto"
            ),
            Self::MixedLeftover {
                profile,
                leftover_workers,
            } => write!(
                f,
                "profile {profile} with leftover llm.dispatch.workers={leftover_workers} \
(legacy 3 or current canary 12); omit those scalars or set runtime.scale.overrides.workers \
to inherit a non-current profile"
            ),
            Self::InvalidOverride { field, value } => {
                write!(f, "runtime.scale.overrides.{field} must be greater than 0, got {value}")
            }
        }
    }
}

impl std::error::Error for RuntimePlanError {}

struct HostRuntimeWidths {
    main_worker_threads: usize,
    http_worker_threads: usize,
    execution_worker_threads: usize,
    background_worker_threads: usize,
    lance_worker_threads: usize,
    lance_search_cost_units: usize,
    blocking_admission_permits: usize,
    live_agent_limit: usize,
    global_cloud_concurrency: usize,
}

fn current_main_worker_threads(cpu_count: usize) -> usize {
    cpu_count.max(2).min(32)
}

fn host_widths(profile: ScaleProfile, cpu_count: usize) -> HostRuntimeWidths {
    match profile {
        ScaleProfile::Current | ScaleProfile::Auto => HostRuntimeWidths {
            // Keep today's dedicated-runtime widths. `http: 4` is the PR8
            // canary (Actix previously used num_cpus). Lance stays 4, not a
            // silent shrink, while search still occupies that pool.
            main_worker_threads: current_main_worker_threads(cpu_count),
            http_worker_threads: 4,
            execution_worker_threads: 4,
            background_worker_threads: 4,
            lance_worker_threads: 4,
            lance_search_cost_units: 4,
            blocking_admission_permits: 16,
            live_agent_limit: 50,
            global_cloud_concurrency: 16,
        },
        ScaleProfile::SmallCpu => HostRuntimeWidths {
            main_worker_threads: 2,
            http_worker_threads: 2,
            execution_worker_threads: 2,
            background_worker_threads: 1,
            lance_worker_threads: 1,
            lance_search_cost_units: 1,
            blocking_admission_permits: 4,
            live_agent_limit: 30,
            global_cloud_concurrency: 4,
        },
        ScaleProfile::M2Max => HostRuntimeWidths {
            // Distinct from `current`: more dispatch/execution/live-agent,
            // fewer main/bg threads than a 12-core ambient default.
            main_worker_threads: 4,
            http_worker_threads: 4,
            execution_worker_threads: 8,
            background_worker_threads: 2,
            lance_worker_threads: 4,
            lance_search_cost_units: 4,
            blocking_admission_permits: 32,
            live_agent_limit: 300,
            global_cloud_concurrency: 24,
        },
        ScaleProfile::CloudHeavy => HostRuntimeWidths {
            main_worker_threads: 8,
            http_worker_threads: 8,
            execution_worker_threads: 8,
            background_worker_threads: 4,
            lance_worker_threads: 8,
            lance_search_cost_units: 8,
            blocking_admission_permits: 32,
            live_agent_limit: 500,
            global_cloud_concurrency: 32,
        },
    }
}

fn dispatch_plan_for(profile: ScaleProfile) -> DispatchCapacityPlan {
    match profile {
        ScaleProfile::Current | ScaleProfile::Auto => DispatchCapacityPlan::CURRENT,
        ScaleProfile::SmallCpu => DispatchCapacityPlan::SMALL_CPU,
        ScaleProfile::M2Max => DispatchCapacityPlan::M2_MAX,
        ScaleProfile::CloudHeavy => DispatchCapacityPlan::CLOUD_HEAVY,
    }
}

fn parse_env_profile(env_profile: Option<&str>) -> Result<Option<ScaleProfile>, RuntimePlanError> {
    match env_profile {
        None => Ok(None),
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                ScaleProfile::parse(trimmed)
                    .map(Some)
                    .ok_or_else(|| RuntimePlanError::UnknownProfile(trimmed.to_string()))
            }
        },
    }
}

fn select_profile(
    yaml_profile: ScaleProfile,
    env_profile: Option<&str>,
    cpu_count: usize,
) -> Result<(Option<ScaleProfile>, ScaleProfile), RuntimePlanError> {
    let env_profile = parse_env_profile(env_profile)?;
    let named = env_profile.unwrap_or(yaml_profile);
    let selected = match named {
        ScaleProfile::Auto => {
            if cpu_count <= 4 {
                ScaleProfile::SmallCpu
            } else {
                ScaleProfile::Current
            }
        },
        other => other,
    };
    Ok((env_profile, selected))
}

fn apply_usize_override(
    base: usize,
    override_value: Option<usize>,
    field: &'static str,
) -> Result<usize, RuntimePlanError> {
    match override_value {
        None => Ok(base.max(1)),
        Some(0) => Err(RuntimePlanError::InvalidOverride { field, value: 0 }),
        Some(value) => Ok(value),
    }
}

fn apply_u64_override(
    base: u64,
    override_value: Option<u64>,
    field: &'static str,
) -> Result<u64, RuntimePlanError> {
    match override_value {
        None => Ok(base.max(1)),
        Some(0) => Err(RuntimePlanError::InvalidOverride { field, value: 0 }),
        Some(value) => Ok(value),
    }
}

/// Fail closed when an explicit non-`current` profile still carries the
/// historical YAML `workers: 3` or PR5 `workers: 12` seed.
pub fn reject_mixed_legacy_dispatch(
    selected_profile: ScaleProfile,
    leftover: LeftoverDispatchScalars,
    overrides: &RuntimeScaleOverrides,
) -> Result<(), RuntimePlanError> {
    if selected_profile.allows_current_era_dispatch_scalars() {
        return Ok(());
    }
    if overrides.workers.is_some() {
        return Ok(());
    }
    let leftover_workers = leftover.workers;
    if leftover_workers == DispatchCapacityPlan::LEGACY_THREE.workers
        || leftover_workers == DispatchCapacityPlan::CURRENT.workers
    {
        return Err(RuntimePlanError::MixedLeftover {
            profile: selected_profile,
            leftover_workers,
        });
    }
    Ok(())
}

/// Pure resolver. `env_profile` is the `MAGICIAN_SCALE_PROFILE` value, not
/// read from the process here.
pub fn resolve_runtime_plan(
    profile: ScaleProfile,
    overrides: &RuntimeScaleOverrides,
    cpu_count: usize,
    env_profile: Option<&str>,
) -> Result<EffectiveRuntimePlan, RuntimePlanError> {
    let cpu_count = cpu_count.max(1);
    let (parsed_env_profile, selected_profile) = select_profile(profile, env_profile, cpu_count)?;
    let host = host_widths(selected_profile, cpu_count);
    let dispatch = dispatch_plan_for(selected_profile);

    let plan = EffectiveRuntimePlan {
        yaml_profile: profile,
        env_profile: parsed_env_profile,
        selected_profile,
        cpu_count,
        main_worker_threads: apply_usize_override(
            host.main_worker_threads,
            overrides.main_worker_threads,
            "main_worker_threads",
        )?,
        http_worker_threads: apply_usize_override(
            host.http_worker_threads,
            overrides.http_worker_threads,
            "http_worker_threads",
        )?,
        execution_worker_threads: apply_usize_override(
            host.execution_worker_threads,
            overrides.execution_worker_threads,
            "execution_worker_threads",
        )?,
        background_worker_threads: apply_usize_override(
            host.background_worker_threads,
            overrides.background_worker_threads,
            "background_worker_threads",
        )?,
        lance_worker_threads: apply_usize_override(
            host.lance_worker_threads,
            overrides.lance_worker_threads,
            "lance_worker_threads",
        )?,
        lance_search_cost_units: apply_usize_override(
            host.lance_search_cost_units,
            overrides.lance_search_cost_units,
            "lance_search_cost_units",
        )?,
        // `0` is unlimited (no semaphore wait), unlike lance which rejects 0.
        blocking_admission_permits: overrides
            .blocking_admission_permits
            .unwrap_or(host.blocking_admission_permits),
        // `0` is PR7 observe-only rollback, so this knob may be zero.
        live_agent_limit: overrides.live_agent_limit.unwrap_or(host.live_agent_limit),
        workers: apply_usize_override(dispatch.workers, overrides.workers, "workers")?,
        reserved_interactive_workers: apply_usize_override(
            dispatch.reserved_interactive_workers,
            overrides.reserved_interactive_workers,
            "reserved_interactive_workers",
        )?,
        global_cloud_concurrency: apply_usize_override(
            host.global_cloud_concurrency,
            overrides.global_cloud_concurrency,
            "global_cloud_concurrency",
        )?,
        queue_capacity_high: apply_usize_override(
            dispatch.queue_capacity_high,
            overrides.queue_capacity_high,
            "queue_capacity_high",
        )?,
        queue_capacity_normal: apply_usize_override(
            dispatch.queue_capacity_normal,
            overrides.queue_capacity_normal,
            "queue_capacity_normal",
        )?,
        queue_capacity_background: apply_usize_override(
            dispatch.queue_capacity_background,
            overrides.queue_capacity_background,
            "queue_capacity_background",
        )?,
        max_request_bytes: apply_u64_override(
            dispatch.max_request_bytes,
            overrides.max_request_bytes,
            "max_request_bytes",
        )?,
        queue_bytes_high: apply_u64_override(
            dispatch.queue_bytes_high,
            overrides.queue_bytes_high,
            "queue_bytes_high",
        )?,
        queue_bytes_normal: apply_u64_override(
            dispatch.queue_bytes_normal,
            overrides.queue_bytes_normal,
            "queue_bytes_normal",
        )?,
        queue_bytes_background: apply_u64_override(
            dispatch.queue_bytes_background,
            overrides.queue_bytes_background,
            "queue_bytes_background",
        )?,
        queue_bytes_global: apply_u64_override(
            dispatch.queue_bytes_global,
            overrides.queue_bytes_global,
            "queue_bytes_global",
        )?,
    };
    plan.dispatch_capacity()
        .validate()
        .map_err(|_| RuntimePlanError::InvalidOverride {
            field: "workers",
            value: plan.workers as u64,
        })?;
    Ok(plan)
}

/// Boot resolver: profile table + overrides, then leftover-scalar check.
pub fn resolve_boot_runtime_plan(
    profile: ScaleProfile,
    overrides: &RuntimeScaleOverrides,
    cpu_count: usize,
    env_profile: Option<&str>,
    leftover: LeftoverDispatchScalars,
) -> Result<EffectiveRuntimePlan, RuntimePlanError> {
    let plan = resolve_runtime_plan(profile, overrides, cpu_count, env_profile)?;
    reject_mixed_legacy_dispatch(plan.selected_profile, leftover, overrides)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leftover(workers: usize) -> LeftoverDispatchScalars {
        LeftoverDispatchScalars {
            workers,
            reserved_interactive_workers: DispatchCapacityPlan::CURRENT
                .reserved_interactive_workers,
            queue_capacity_high: DispatchCapacityPlan::CURRENT.queue_capacity_high,
            queue_capacity_normal: DispatchCapacityPlan::CURRENT.queue_capacity_normal,
            queue_capacity_background: DispatchCapacityPlan::CURRENT.queue_capacity_background,
        }
    }

    #[test]
    fn current_profile_without_overrides_matches_current_dispatch() {
        let plan = resolve_runtime_plan(
            ScaleProfile::Current,
            &RuntimeScaleOverrides::default(),
            12,
            None,
        )
        .expect("current plan");
        assert_eq!(plan.selected_profile, ScaleProfile::Current);
        assert_eq!(plan.workers, DispatchCapacityPlan::CURRENT.workers);
        assert_eq!(
            plan.reserved_interactive_workers,
            DispatchCapacityPlan::CURRENT.reserved_interactive_workers
        );
        assert_eq!(
            plan.queue_capacity_normal,
            DispatchCapacityPlan::CURRENT.queue_capacity_normal
        );
        assert_eq!(plan.execution_worker_threads, 4);
        assert_eq!(plan.background_worker_threads, 4);
        assert_eq!(plan.lance_worker_threads, 4);
        assert_eq!(plan.blocking_admission_permits, 16);
        assert_eq!(plan.live_agent_limit, 50);
        assert_eq!(plan.http_worker_threads, 4);
        assert_eq!(plan.main_worker_threads, 12);
        assert_eq!(plan.dispatch_capacity(), DispatchCapacityPlan::CURRENT);
    }

    #[test]
    fn inherit_overrides_none_leaves_profile_table() {
        let yaml = r#"
profile: current
overrides:
  main_worker_threads: null
  workers: null
  live_agent_limit: null
"#;
        let settings: RuntimeScaleSettings = serde_yaml::from_str(yaml).expect("scale yaml");
        assert!(settings.overrides.workers.is_none());
        assert!(settings.overrides.main_worker_threads.is_none());
        let plan =
            resolve_runtime_plan(settings.profile, &settings.overrides, 8, None).expect("inherit");
        assert_eq!(plan.workers, 12);
        assert_eq!(plan.main_worker_threads, 8);
    }

    #[test]
    fn explicit_override_workers_wins_on_current() {
        let overrides = RuntimeScaleOverrides {
            workers: Some(8),
            ..RuntimeScaleOverrides::default()
        };
        let plan =
            resolve_runtime_plan(ScaleProfile::Current, &overrides, 12, None).expect("override");
        assert_eq!(plan.workers, 8);
        assert_eq!(
            plan.queue_capacity_normal,
            DispatchCapacityPlan::CURRENT.queue_capacity_normal
        );
    }

    #[test]
    fn auto_with_cpu_4_selects_small_cpu() {
        let plan = resolve_runtime_plan(
            ScaleProfile::Auto,
            &RuntimeScaleOverrides::default(),
            4,
            None,
        )
        .expect("auto small");
        assert_eq!(plan.selected_profile, ScaleProfile::SmallCpu);
        assert_eq!(plan.workers, DispatchCapacityPlan::SMALL_CPU.workers);
        assert_eq!(plan.live_agent_limit, 30);
        assert_eq!(plan.blocking_admission_permits, 4);
        assert_eq!(plan.execution_worker_threads, 2);
        assert_eq!(plan.main_worker_threads, 2);
    }

    #[test]
    fn auto_with_cpu_8_selects_current() {
        let plan = resolve_runtime_plan(
            ScaleProfile::Auto,
            &RuntimeScaleOverrides::default(),
            8,
            None,
        )
        .expect("auto current");
        assert_eq!(plan.selected_profile, ScaleProfile::Current);
        assert_eq!(plan.workers, DispatchCapacityPlan::CURRENT.workers);
        assert_eq!(plan.main_worker_threads, 8);
    }

    #[test]
    fn env_profile_overrides_yaml_name() {
        let plan = resolve_runtime_plan(
            ScaleProfile::Current,
            &RuntimeScaleOverrides::default(),
            12,
            Some("small_cpu"),
        )
        .expect("env override");
        assert_eq!(plan.yaml_profile, ScaleProfile::Current);
        assert_eq!(plan.env_profile, Some(ScaleProfile::SmallCpu));
        assert_eq!(plan.selected_profile, ScaleProfile::SmallCpu);
        assert_eq!(plan.workers, 4);
    }

    #[test]
    fn unknown_env_profile_fails_closed() {
        let error = resolve_runtime_plan(
            ScaleProfile::Current,
            &RuntimeScaleOverrides::default(),
            12,
            Some("m1_ultra"),
        )
        .expect_err("unknown profile");
        assert!(matches!(error, RuntimePlanError::UnknownProfile(_)));
        assert!(error.to_string().contains("m1_ultra"));
    }

    #[test]
    fn m2_max_is_not_a_silent_current_noop() {
        let plan = resolve_runtime_plan(
            ScaleProfile::M2Max,
            &RuntimeScaleOverrides::default(),
            12,
            None,
        )
        .expect("m2_max");
        assert_eq!(plan.selected_profile, ScaleProfile::M2Max);
        assert_eq!(plan.workers, DispatchCapacityPlan::M2_MAX.workers);
        assert_ne!(plan.workers, DispatchCapacityPlan::CURRENT.workers);
        assert_eq!(plan.reserved_interactive_workers, 8);
        assert_eq!(plan.execution_worker_threads, 8);
        assert_eq!(plan.live_agent_limit, 300);
        assert_eq!(plan.blocking_admission_permits, 32);
        assert_eq!(plan.background_worker_threads, 2);
        assert_eq!(plan.main_worker_threads, 4);
        assert!(plan.workers > DispatchCapacityPlan::CURRENT.workers);
    }

    #[test]
    fn m2_max_plus_legacy_workers_3_fails_closed() {
        let error = resolve_boot_runtime_plan(
            ScaleProfile::M2Max,
            &RuntimeScaleOverrides::default(),
            12,
            None,
            leftover(3),
        )
        .expect_err("legacy leftover");
        assert_eq!(
            error,
            RuntimePlanError::MixedLeftover {
                profile: ScaleProfile::M2Max,
                leftover_workers: 3,
            }
        );
        assert!(error.to_string().contains("m2_max"));
        assert!(error.to_string().contains('3'));
    }

    #[test]
    fn m2_max_plus_current_workers_12_fails_closed() {
        let error = resolve_boot_runtime_plan(
            ScaleProfile::M2Max,
            &RuntimeScaleOverrides::default(),
            12,
            None,
            leftover(12),
        )
        .expect_err("current leftover");
        assert_eq!(
            error,
            RuntimePlanError::MixedLeftover {
                profile: ScaleProfile::M2Max,
                leftover_workers: 12,
            }
        );
    }

    #[test]
    fn small_cpu_plus_current_workers_12_fails_closed() {
        let error = resolve_boot_runtime_plan(
            ScaleProfile::SmallCpu,
            &RuntimeScaleOverrides::default(),
            4,
            None,
            leftover(12),
        )
        .expect_err("current leftover on small_cpu");
        assert!(matches!(
            error,
            RuntimePlanError::MixedLeftover {
                profile: ScaleProfile::SmallCpu,
                leftover_workers: 12
            }
        ));
    }

    #[test]
    fn auto_resolved_current_allows_workers_12() {
        let plan = resolve_boot_runtime_plan(
            ScaleProfile::Auto,
            &RuntimeScaleOverrides::default(),
            12,
            None,
            leftover(12),
        )
        .expect("auto-resolved current");
        assert_eq!(plan.selected_profile, ScaleProfile::Current);
        assert_eq!(plan.workers, 12);
    }

    #[test]
    fn auto_resolved_small_cpu_rejects_workers_12() {
        let error = resolve_boot_runtime_plan(
            ScaleProfile::Auto,
            &RuntimeScaleOverrides::default(),
            4,
            None,
            leftover(12),
        )
        .expect_err("auto small leftover");
        assert_eq!(
            error,
            RuntimePlanError::MixedLeftover {
                profile: ScaleProfile::SmallCpu,
                leftover_workers: 12,
            }
        );
    }

    #[test]
    fn m2_max_with_explicit_override_workers_allows_leftover_yaml() {
        let overrides = RuntimeScaleOverrides {
            workers: Some(24),
            ..RuntimeScaleOverrides::default()
        };
        let plan =
            resolve_boot_runtime_plan(ScaleProfile::M2Max, &overrides, 12, None, leftover(12))
                .expect("explicit override owns workers");
        assert_eq!(plan.workers, 24);
        assert_eq!(plan.live_agent_limit, 300);
    }

    #[test]
    fn env_m2_max_with_yaml_current_and_workers_12_fails_closed() {
        let error = resolve_boot_runtime_plan(
            ScaleProfile::Current,
            &RuntimeScaleOverrides::default(),
            12,
            Some("m2_max"),
            leftover(12),
        )
        .expect_err("env selected m2_max");
        assert_eq!(
            error,
            RuntimePlanError::MixedLeftover {
                profile: ScaleProfile::M2Max,
                leftover_workers: 12,
            }
        );
    }

    #[test]
    fn live_agent_limit_zero_is_observe_only_rollback() {
        let overrides = RuntimeScaleOverrides {
            live_agent_limit: Some(0),
            ..RuntimeScaleOverrides::default()
        };
        let plan = resolve_runtime_plan(ScaleProfile::Current, &overrides, 12, None)
            .expect("0 live-agent is PR7 observe-only");
        assert_eq!(plan.live_agent_limit, 0);
        assert_eq!(plan.workers, 12);
    }

    #[test]
    fn zero_override_fails_closed() {
        let overrides = RuntimeScaleOverrides {
            execution_worker_threads: Some(0),
            ..RuntimeScaleOverrides::default()
        };
        let error = resolve_runtime_plan(ScaleProfile::Current, &overrides, 12, None)
            .expect_err("zero override");
        assert!(matches!(
            error,
            RuntimePlanError::InvalidOverride {
                field: "execution_worker_threads",
                value: 0
            }
        ));
    }

    #[test]
    fn apply_to_dispatch_copies_resolved_capacity() {
        let plan = resolve_runtime_plan(
            ScaleProfile::SmallCpu,
            &RuntimeScaleOverrides::default(),
            4,
            None,
        )
        .expect("small_cpu");
        let mut config = DispatchConfig::default();
        plan.apply_to_dispatch(&mut config);
        assert_eq!(config.workers, DispatchCapacityPlan::SMALL_CPU.workers);
        assert_eq!(config.global_cloud_concurrency, 4);
        assert_eq!(
            DispatchCapacityPlan::from_config(&config),
            DispatchCapacityPlan::SMALL_CPU
        );
    }

    #[test]
    fn cloud_heavy_is_larger_than_m2_max() {
        let m2 = resolve_runtime_plan(
            ScaleProfile::M2Max,
            &RuntimeScaleOverrides::default(),
            12,
            None,
        )
        .expect("m2");
        let cloud = resolve_runtime_plan(
            ScaleProfile::CloudHeavy,
            &RuntimeScaleOverrides::default(),
            12,
            None,
        )
        .expect("cloud");
        assert!(cloud.workers > m2.workers);
        assert!(cloud.live_agent_limit > m2.live_agent_limit);
        assert!(cloud.lance_worker_threads > m2.lance_worker_threads);
        assert_eq!(m2.blocking_admission_permits, 32);
        assert_eq!(cloud.blocking_admission_permits, 32);
    }

    #[test]
    fn blocking_admission_zero_is_unlimited() {
        let overrides = RuntimeScaleOverrides {
            blocking_admission_permits: Some(0),
            ..RuntimeScaleOverrides::default()
        };
        let plan = resolve_runtime_plan(ScaleProfile::Current, &overrides, 12, None)
            .expect("0 blocking-admission is unlimited");
        assert_eq!(plan.blocking_admission_permits, 0);
        assert_eq!(plan.workers, 12);
        assert_eq!(plan.lance_search_cost_units, 4);
    }
}
