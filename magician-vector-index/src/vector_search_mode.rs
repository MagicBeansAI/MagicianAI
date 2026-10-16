//! Hybrid vector-leg search mode: exhaustive flat KNN, ANN shadow, or ANN.
//!
//! Crate `Default` is [`VectorSearchMode::Flat`] so unit tests that never
//! install Magician config keep brute-force answers. Magician YAML default is
//! `ann`. `MAGICIAN_VECTOR_SEARCH=flat|ann_shadow|ann` is the restart-bound
//! override; invalid values are ignored.

use std::{
    collections::HashSet,
    env,
    sync::{
        atomic::{AtomicU32, AtomicU8, Ordering},
        Mutex, OnceLock,
    },
};

pub const DEFAULT_VECTOR_SEARCH_MIN_ROWS: usize = 256;
pub const DEFAULT_VECTOR_SEARCH_CANDIDATE_MULTIPLIER: usize = 4;
pub const DEFAULT_VECTOR_SEARCH_NPROBES: usize = 20;
pub const MAX_VECTOR_SEARCH_CANDIDATE_MULTIPLIER: usize = 32;

const MODE_FLAT: u8 = 0;
const MODE_ANN_SHADOW: u8 = 1;
const MODE_ANN: u8 = 2;

#[doc(hidden)]
pub static VECTOR_SEARCH_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorSearchMode {
    Flat,
    AnnShadow,
    Ann,
}

impl VectorSearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::AnnShadow => "ann_shadow",
            Self::Ann => "ann",
        }
    }

    pub fn from_label(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "flat" => Some(Self::Flat),
            "ann_shadow" | "ann-shadow" | "shadow" => Some(Self::AnnShadow),
            "ann" => Some(Self::Ann),
            _ => None,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Flat => MODE_FLAT,
            Self::AnnShadow => MODE_ANN_SHADOW,
            Self::Ann => MODE_ANN,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            MODE_ANN_SHADOW => Self::AnnShadow,
            MODE_ANN => Self::Ann,
            _ => Self::Flat,
        }
    }

    pub fn maintains_ivf(self) -> bool {
        matches!(self, Self::AnnShadow | Self::Ann)
    }

    pub fn serves_ann(self) -> bool {
        matches!(self, Self::Ann)
    }
}

impl Default for VectorSearchMode {
    fn default() -> Self {
        Self::Flat
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorSearchSettings {
    pub mode: VectorSearchMode,
    pub min_rows: usize,
    pub candidate_multiplier: usize,
    pub nprobes: usize,
}

impl Default for VectorSearchSettings {
    fn default() -> Self {
        Self {
            mode: VectorSearchMode::Flat,
            min_rows: DEFAULT_VECTOR_SEARCH_MIN_ROWS,
            candidate_multiplier: DEFAULT_VECTOR_SEARCH_CANDIDATE_MULTIPLIER,
            nprobes: DEFAULT_VECTOR_SEARCH_NPROBES,
        }
    }
}

impl VectorSearchSettings {
    pub fn sanitized(self) -> Self {
        Self {
            mode: self.mode,
            min_rows: self.min_rows.max(1),
            candidate_multiplier: self
                .candidate_multiplier
                .clamp(1, MAX_VECTOR_SEARCH_CANDIDATE_MULTIPLIER),
            nprobes: self.nprobes.max(1),
        }
    }

    pub fn ann_shortlist_limit(self, limit: usize) -> usize {
        let limit = limit.max(1);
        let multiplier = self
            .candidate_multiplier
            .clamp(1, MAX_VECTOR_SEARCH_CANDIDATE_MULTIPLIER);
        limit.saturating_mul(multiplier)
    }
}

/// How the vector leg should run for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorSearchPlan {
    Flat {
        limit: usize,
    },
    Shadow {
        limit: usize,
        ann_limit: usize,
        run_ann: bool,
    },
    Ann {
        limit: usize,
        shortlist: usize,
    },
    FlatFallback {
        limit: usize,
    },
}

impl VectorSearchPlan {
    pub fn served_limit(self) -> usize {
        match self {
            Self::Flat { limit }
            | Self::Shadow { limit, .. }
            | Self::Ann { limit, .. }
            | Self::FlatFallback { limit } => limit,
        }
    }

    pub fn bypasses_served_path(self) -> bool {
        !matches!(self, Self::Ann { .. })
    }

    pub fn records_fallback(self) -> bool {
        matches!(
            self,
            Self::FlatFallback { .. } | Self::Shadow { run_ann: false, .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorSearchRecall {
    pub milles: u16,
    pub mismatch: bool,
    pub overlap: usize,
    pub k: usize,
}

fn settings_state() -> &'static Mutex<VectorSearchSettings> {
    static STATE: OnceLock<Mutex<VectorSearchSettings>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(VectorSearchSettings::default()))
}

fn lock_settings() -> std::sync::MutexGuard<'static, VectorSearchSettings> {
    settings_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

static INSTALLED_MODE: AtomicU8 = AtomicU8::new(MODE_FLAT);
static RANKING_EPOCH: AtomicU32 = AtomicU32::new(0);

/// Served ranking identity. Flat and shadow share exhaustive KNN. Ann
/// ranking also depends on shortlist width and nprobes.
fn served_ranking_fingerprint(settings: VectorSearchSettings) -> (u8, usize, usize) {
    if settings.mode.serves_ann() {
        (1, settings.candidate_multiplier, settings.nprobes)
    } else {
        (0, 0, 0)
    }
}

/// Bumped when served ANN ranking can change without a new index generation
/// (mode/params install, or IVF_PQ create). Hybrid cache keys include this.
pub fn vector_search_ranking_epoch() -> u32 {
    RANKING_EPOCH.load(Ordering::Acquire)
}

pub(crate) fn notify_vector_search_ranking_changed() {
    RANKING_EPOCH.fetch_add(1, Ordering::AcqRel);
}

fn env_mode_override() -> Option<VectorSearchMode> {
    let value = env::var("MAGICIAN_VECTOR_SEARCH").ok()?;
    VectorSearchMode::from_label(&value)
}

pub fn vector_search_mode() -> VectorSearchMode {
    env_mode_override()
        .unwrap_or_else(|| VectorSearchMode::from_code(INSTALLED_MODE.load(Ordering::Acquire)))
}

pub fn vector_search_mode_label() -> &'static str {
    vector_search_mode().as_str()
}

pub fn vector_search_settings() -> VectorSearchSettings {
    vector_search_snapshot().0
}

/// Settings and ranking epoch from one lock so a live reload cannot tear
/// multiplier/nprobes from the epoch stored on the hybrid cache key.
pub fn vector_search_snapshot() -> (VectorSearchSettings, u32) {
    let current = lock_settings();
    let epoch = RANKING_EPOCH.load(Ordering::Acquire);
    let mut settings = *current;
    drop(current);
    if let Some(mode) = env_mode_override() {
        settings.mode = mode;
    } else {
        settings.mode = VectorSearchMode::from_code(INSTALLED_MODE.load(Ordering::Acquire));
    }
    (settings.sanitized(), epoch)
}

pub fn install_vector_search(settings: VectorSearchSettings) {
    let settings = settings.sanitized();
    let mut current = lock_settings();
    let previous = served_ranking_fingerprint(effective_installed(*current));
    *current = settings;
    INSTALLED_MODE.store(settings.mode.code(), Ordering::Release);
    if previous != served_ranking_fingerprint(effective_installed(settings)) {
        RANKING_EPOCH.fetch_add(1, Ordering::AcqRel);
    }
}

fn effective_installed(mut settings: VectorSearchSettings) -> VectorSearchSettings {
    if let Some(mode) = env_mode_override() {
        settings.mode = mode;
    }
    settings.sanitized()
}

pub fn reset_vector_search_for_tests() {
    INSTALLED_MODE.store(MODE_FLAT, Ordering::Release);
    RANKING_EPOCH.store(0, Ordering::Release);
    *lock_settings() = VectorSearchSettings::default();
}

pub fn plan_vector_search(limit: usize, ivf_present: bool) -> VectorSearchPlan {
    plan_vector_search_with(vector_search_settings(), limit, ivf_present)
}

pub fn plan_vector_search_with(
    settings: VectorSearchSettings,
    limit: usize,
    ivf_present: bool,
) -> VectorSearchPlan {
    let settings = settings.sanitized();
    let limit = limit.max(1);
    match settings.mode {
        VectorSearchMode::Flat => VectorSearchPlan::Flat { limit },
        VectorSearchMode::AnnShadow => VectorSearchPlan::Shadow {
            limit,
            ann_limit: settings.ann_shortlist_limit(limit),
            run_ann: ivf_present,
        },
        VectorSearchMode::Ann => {
            if ivf_present {
                VectorSearchPlan::Ann {
                    limit,
                    shortlist: settings.ann_shortlist_limit(limit),
                }
            } else {
                VectorSearchPlan::FlatFallback { limit }
            }
        },
    }
}

/// Recall@k as `|intersect(top-k)| / k` in milles (0..=1000). `mismatch` is
/// true when the ordered top-k key lists differ.
pub fn recall_at_k(ground_truth: &[String], observed: &[String], k: usize) -> VectorSearchRecall {
    let k = k.max(1);
    let truth_top: Vec<&str> = ground_truth.iter().take(k).map(String::as_str).collect();
    let observed_top: Vec<&str> = observed.iter().take(k).map(String::as_str).collect();
    let truth_set: HashSet<&str> = truth_top.iter().copied().collect();
    let observed_set: HashSet<&str> = observed_top.iter().copied().collect();
    let overlap = truth_set.intersection(&observed_set).count();
    let milles = ((overlap.saturating_mul(1000)) / k) as u16;
    let mismatch = truth_top != observed_top;
    VectorSearchRecall {
        milles,
        mismatch,
        overlap,
        k,
    }
}

pub fn l2_squared_distance(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() {
        return f32::INFINITY;
    }
    left.iter()
        .zip(right)
        .map(|(a, b)| {
            let delta = a - b;
            delta * delta
        })
        .sum()
}

/// ANN served scores are a different ranking contract than flat/shadow.
pub fn served_hybrid_scoring_contract(base: u32) -> u32 {
    served_hybrid_scoring_contract_for(base, vector_search_mode())
}

/// Same as [`served_hybrid_scoring_contract`] for a captured mode so a cache
/// key and the vector leg cannot disagree if install races mid-request.
pub fn served_hybrid_scoring_contract_for(base: u32, mode: VectorSearchMode) -> u32 {
    match mode {
        VectorSearchMode::Ann => base.saturating_add(1),
        VectorSearchMode::Flat | VectorSearchMode::AnnShadow => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var(key).ok();
            env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }

    #[tokio::test]
    async fn default_mode_is_flat_without_install() {
        let _lock = VECTOR_SEARCH_TEST_LOCK.lock().await;
        reset_vector_search_for_tests();
        assert_eq!(vector_search_mode(), VectorSearchMode::Flat);
        assert_eq!(vector_search_mode_label(), "flat");
        let plan = plan_vector_search(8, true);
        assert_eq!(plan, VectorSearchPlan::Flat { limit: 8 });
        assert!(plan.bypasses_served_path());
        reset_vector_search_for_tests();
    }

    #[tokio::test]
    async fn install_ann_shadow_and_ann_plans() {
        let _lock = VECTOR_SEARCH_TEST_LOCK.lock().await;
        reset_vector_search_for_tests();
        install_vector_search(VectorSearchSettings {
            mode: VectorSearchMode::AnnShadow,
            min_rows: 256,
            candidate_multiplier: 4,
            nprobes: 20,
        });
        assert_eq!(
            plan_vector_search(5, true),
            VectorSearchPlan::Shadow {
                limit: 5,
                ann_limit: 20,
                run_ann: true,
            }
        );
        assert_eq!(
            plan_vector_search(5, false),
            VectorSearchPlan::Shadow {
                limit: 5,
                ann_limit: 20,
                run_ann: false,
            }
        );
        assert!(plan_vector_search(5, true).bypasses_served_path());

        install_vector_search(VectorSearchSettings {
            mode: VectorSearchMode::Ann,
            ..VectorSearchSettings::default()
        });
        assert_eq!(
            plan_vector_search(5, true),
            VectorSearchPlan::Ann {
                limit: 5,
                shortlist: 20,
            }
        );
        assert!(!plan_vector_search(5, true).bypasses_served_path());
        assert_eq!(
            plan_vector_search(5, false),
            VectorSearchPlan::FlatFallback { limit: 5 }
        );
        assert!(plan_vector_search(5, false).records_fallback());
        reset_vector_search_for_tests();
    }

    #[tokio::test]
    async fn env_override_wins_and_invalid_is_ignored() {
        let _lock = VECTOR_SEARCH_TEST_LOCK.lock().await;
        reset_vector_search_for_tests();
        install_vector_search(VectorSearchSettings {
            mode: VectorSearchMode::Ann,
            ..VectorSearchSettings::default()
        });
        {
            let _guard = EnvVarGuard::set("MAGICIAN_VECTOR_SEARCH", "flat");
            assert_eq!(vector_search_mode(), VectorSearchMode::Flat);
            assert_eq!(
                plan_vector_search(3, true),
                VectorSearchPlan::Flat { limit: 3 }
            );
        }
        {
            let _guard = EnvVarGuard::set("MAGICIAN_VECTOR_SEARCH", "ann_shadow");
            assert_eq!(vector_search_mode(), VectorSearchMode::AnnShadow);
        }
        {
            let _guard = EnvVarGuard::set("MAGICIAN_VECTOR_SEARCH", "not-a-mode");
            assert_eq!(vector_search_mode(), VectorSearchMode::Ann);
        }
        reset_vector_search_for_tests();
    }

    #[test]
    fn recall_at_k_is_intersection_over_k() {
        let flat = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let ann = vec!["a".into(), "c".into(), "x".into(), "d".into()];
        let recall = recall_at_k(&flat, &ann, 3);
        assert_eq!(recall.k, 3);
        assert_eq!(recall.overlap, 2);
        assert_eq!(recall.milles, 666);
        assert!(recall.mismatch);

        let same = recall_at_k(&flat, &flat, 4);
        assert_eq!(same.milles, 1000);
        assert!(!same.mismatch);
        assert_eq!(same.overlap, 4);
    }

    #[test]
    fn candidate_multiplier_is_clamped() {
        let settings = VectorSearchSettings {
            mode: VectorSearchMode::Ann,
            min_rows: 0,
            candidate_multiplier: 99,
            nprobes: 0,
        }
        .sanitized();
        assert_eq!(settings.min_rows, 1);
        assert_eq!(
            settings.candidate_multiplier,
            MAX_VECTOR_SEARCH_CANDIDATE_MULTIPLIER
        );
        assert_eq!(settings.nprobes, 1);
        assert_eq!(
            settings.ann_shortlist_limit(4),
            4 * MAX_VECTOR_SEARCH_CANDIDATE_MULTIPLIER
        );
    }

    #[tokio::test]
    async fn served_hybrid_scoring_contract_bumps_only_for_ann() {
        let _lock = VECTOR_SEARCH_TEST_LOCK.lock().await;
        reset_vector_search_for_tests();
        assert_eq!(served_hybrid_scoring_contract(1), 1);
        assert_eq!(vector_search_ranking_epoch(), 0);
        install_vector_search(VectorSearchSettings {
            mode: VectorSearchMode::AnnShadow,
            ..VectorSearchSettings::default()
        });
        assert_eq!(served_hybrid_scoring_contract(1), 1);
        assert_eq!(vector_search_ranking_epoch(), 0);
        install_vector_search(VectorSearchSettings {
            mode: VectorSearchMode::Ann,
            ..VectorSearchSettings::default()
        });
        assert_eq!(served_hybrid_scoring_contract(1), 2);
        assert_eq!(
            served_hybrid_scoring_contract_for(1, VectorSearchMode::Flat),
            1
        );
        assert_eq!(vector_search_ranking_epoch(), 1);
        install_vector_search(VectorSearchSettings {
            mode: VectorSearchMode::Ann,
            candidate_multiplier: 8,
            ..VectorSearchSettings::default()
        });
        assert_eq!(vector_search_ranking_epoch(), 2);
        notify_vector_search_ranking_changed();
        assert_eq!(vector_search_ranking_epoch(), 3);
        reset_vector_search_for_tests();
        assert_eq!(vector_search_ranking_epoch(), 0);
        let (settings, epoch) = vector_search_snapshot();
        assert_eq!(settings.mode, VectorSearchMode::Flat);
        assert_eq!(epoch, 0);
    }

    #[test]
    fn l2_squared_ranks_closer_vectors_lower() {
        let query = [1.0_f32, 0.0, 0.0];
        let near = l2_squared_distance(&query, &[1.0, 0.1, 0.0]);
        let far = l2_squared_distance(&query, &[0.0, 1.0, 0.0]);
        assert!(near < far);
        assert!(l2_squared_distance(&query, &[1.0, 0.0]).is_infinite());
    }
}
