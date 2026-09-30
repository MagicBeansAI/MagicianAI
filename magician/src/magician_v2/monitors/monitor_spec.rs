//! Recurring Monitors (Phase 1) — the typed `MonitorSpecV1` contract.
//!
//! A Monitor is an existing persistent Task + `Task.schedule` + this optional
//! typed spec on `TaskManifest` (plan:
//! `docs/plans/2026-07-21-recurring-monitors-productization-design-implementation.md`
//! §6.1). The spec is the SOLE discriminator: a task is a monitor exactly when
//! `manifest.monitor_spec` is `Some(_)` — never inferred from titles, tags, or
//! prose. Cadence, pause state, `max_runs`, timezone, and next-fire state stay
//! on `Task.schedule` and are intentionally NOT duplicated here.
//!
//! The wire shape is pinned by the canonical Phase 0 fixture
//! `magician/tests/fixtures/monitors/monitor_spec_v1.json` — web
//! (`ui/unified-ui/src/lib/types/monitor.ts`) and iOS read the same file, so
//! the in-module tests decode it with these types to break together on drift.
//!
//! [`validate_and_normalize`] is the single admission gate (plan §6.1 rules
//! 1-4): every API/tool write of a spec must pass through it before the spec
//! reaches `UpdateTaskInput.monitor_spec`.

use serde::{Deserialize, Serialize};

/// Only schema version accepted in Phase 1. Unknown versions fail clearly
/// instead of being silently ignored (plan §6.1 rule 4).
pub const MONITOR_SPEC_SCHEMA_VERSION: u32 = 1;

/// Bounded string budgets enforced at admission (plan §6.1 rule 1).
const MAX_OBJECTIVE_CHARS: usize = 2000;
const MAX_LIST_ENTRY_CHARS: usize = 500;
const MAX_LIST_ENTRIES: usize = 50;
const MAX_SOURCE_URLS: usize = 100;

/// How strictly findings must match the monitor contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorMatchMode {
    Strict,
    Balanced,
    Broad,
}

/// When a monitor run is allowed to notify the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorNotificationPolicy {
    MaterialChanges,
    EveryRun,
    Never,
}

/// Where the monitor looks: explicit URLs, whole domains, and named
/// authenticated sources (accounts/connectors the runtime already holds
/// credentials for — never raw credentials themselves).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorSources {
    pub urls: Vec<String>,
    pub domains: Vec<String>,
    pub authenticated_sources: Vec<String>,
}

/// Typed, versioned monitor contract carried on `TaskManifest.monitor_spec`
/// (plan §6.1). All fields are data, not prompt conventions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorSpecV1 {
    /// Must equal [`MONITOR_SPEC_SCHEMA_VERSION`]; anything else is rejected
    /// at admission.
    pub schema_version: u32,
    /// What the user wants watched, in their words. Trimmed, non-empty,
    /// bounded at admission.
    pub objective: String,
    /// Search phrases used to discover coverage beyond the explicit sources.
    pub query_seeds: Vec<String>,
    pub sources: MonitorSources,
    /// Positive filters — what counts as in-contract.
    pub include_rules: Vec<String>,
    /// Negative filters — what must be ignored.
    pub exclude_rules: Vec<String>,
    pub match_mode: MonitorMatchMode,
    pub notification_policy: MonitorNotificationPolicy,
    /// When `true` the initial baseline run may notify; default product
    /// behavior is a quiet baseline.
    pub notify_initial_baseline: bool,
}

/// Admission gate for every monitor-spec write (plan §6.1 rules 1-4).
///
/// Normalizes in place (trims strings, drops empty entries, dedupes
/// preserving order) and rejects with a **stable snake_case reason** suitable
/// for a 400 body, e.g. `"monitor_schema_version_unsupported"`. Callers must
/// run this before handing the spec to `UpdateTaskInput.monitor_spec`.
pub fn validate_and_normalize(spec: &mut MonitorSpecV1) -> Result<(), String> {
    if spec.schema_version != MONITOR_SPEC_SCHEMA_VERSION {
        return Err("monitor_schema_version_unsupported".to_string());
    }

    let objective = spec.objective.trim().to_string();
    if objective.is_empty() {
        return Err("monitor_objective_required".to_string());
    }
    if objective.chars().count() > MAX_OBJECTIVE_CHARS {
        return Err("monitor_objective_too_long".to_string());
    }
    spec.objective = objective;

    spec.query_seeds = normalize_string_list(std::mem::take(&mut spec.query_seeds), "query_seeds")?;
    spec.include_rules =
        normalize_string_list(std::mem::take(&mut spec.include_rules), "include_rules")?;
    spec.exclude_rules =
        normalize_string_list(std::mem::take(&mut spec.exclude_rules), "exclude_rules")?;
    spec.sources.domains =
        normalize_string_list(std::mem::take(&mut spec.sources.domains), "domains")?;
    spec.sources.authenticated_sources = normalize_string_list(
        std::mem::take(&mut spec.sources.authenticated_sources),
        "authenticated_sources",
    )?;
    spec.sources.urls = normalize_source_urls(std::mem::take(&mut spec.sources.urls))?;

    // A monitor must have at least one place to look (plan §6.1): explicit
    // URLs, domains, or query seeds. Authenticated-source names alone are not
    // enough to scope a run in Phase 1.
    if spec.sources.urls.is_empty()
        && spec.sources.domains.is_empty()
        && spec.query_seeds.is_empty()
    {
        return Err("monitor_sources_required".to_string());
    }

    Ok(())
}

/// Shared list normalization: trim, drop empties, bound entry length, dedupe
/// preserving first-seen order, bound list length. `field` feeds the stable
/// error reason (`monitor_{field}_entry_too_long` /
/// `monitor_{field}_too_many_entries`).
fn normalize_string_list(entries: Vec<String>, field: &str) -> Result<Vec<String>, String> {
    let mut normalized: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if entry.chars().count() > MAX_LIST_ENTRY_CHARS {
            return Err(format!("monitor_{field}_entry_too_long"));
        }
        if !normalized.iter().any(|existing| existing == entry) {
            normalized.push(entry.to_string());
        }
    }
    if normalized.len() > MAX_LIST_ENTRIES {
        return Err(format!("monitor_{field}_too_many_entries"));
    }
    Ok(normalized)
}

/// Phase 4 chat tools — deterministic fingerprint over the NORMALIZED
/// monitor contract (spec + optional schedule JSON). `preview_monitor`
/// returns it and `create_monitor` requires it back unchanged, enforcing
/// "the user reviewed exactly this contract before activation" (plan §5.1)
/// without any prompt convention. `mpv_` + 16-hex blake3, matching the
/// fingerprint style of the run/change fingerprints in `monitor_run`.
pub fn monitor_contract_fingerprint(
    spec: &MonitorSpecV1,
    schedule: Option<&serde_json::Value>,
) -> String {
    let spec_json = serde_json::to_string(spec).unwrap_or_default();
    let schedule_json = schedule
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let digest = blake3::hash(
        format!("monitor_contract_v1\u{1f}{spec_json}\u{1f}{schedule_json}").as_bytes(),
    )
    .to_hex();
    format!("mpv_{}", &digest.as_str()[..16])
}

/// Source URLs are parsed with a real URL parser, never string-matched
/// (plan §6.1 rule 2). Only http/https schemes are watchable sources —
/// anything else (javascript:, file:, data:, …) is rejected.
///
/// Dedupe runs on the CANONICAL form — the same §7.1 normalization the run
/// ledger uses for stable keys (`monitor_run::normalize_monitor_url`:
/// lowercased host, tracking params + fragments stripped, trailing slash
/// dropped, query pairs sorted) — so cosmetic spellings of one source
/// (`…/pricing/` vs `…/pricing`, `?utm_…` noise) collapse to a single
/// entry. The FIRST spelling the user gave is the one kept.
fn normalize_source_urls(urls: Vec<String>) -> Result<Vec<String>, String> {
    let mut normalized: Vec<String> = Vec::new();
    let mut canonical_seen: Vec<String> = Vec::new();
    for url in urls {
        let url = url.trim();
        if url.is_empty() {
            continue;
        }
        let parsed = url::Url::parse(url).map_err(|_| "monitor_source_url_invalid".to_string())?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("monitor_source_url_scheme_unsupported".to_string());
        }
        // http/https URLs always canonicalize (they always carry a host);
        // the raw-string fallback merely keeps this total.
        let canonical =
            super::monitor_run::normalize_monitor_url(url).unwrap_or_else(|| url.to_string());
        if !canonical_seen.iter().any(|existing| existing == &canonical) {
            canonical_seen.push(canonical);
            normalized.push(url.to_string());
        }
    }
    if normalized.len() > MAX_SOURCE_URLS {
        return Err("monitor_source_urls_too_many".to_string());
    }
    Ok(normalized)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::models::TaskManifest;
    use serde_json::{json, Value};

    /// The CANONICAL Phase 0 wire fixture — shared byte-for-byte with the
    /// web vitest and iOS XCTest contract checks.
    const SPEC_FIXTURE: &str =
        include_str!("../../../tests/fixtures/monitors/monitor_spec_v1.json");

    fn fixture_spec() -> MonitorSpecV1 {
        serde_json::from_str(SPEC_FIXTURE).expect("canonical fixture must decode as MonitorSpecV1")
    }

    #[test]
    fn fixture_decodes_and_round_trips_field_stable() {
        let spec = fixture_spec();
        assert_eq!(spec.schema_version, 1);
        assert_eq!(spec.match_mode, MonitorMatchMode::Balanced);
        assert_eq!(
            spec.notification_policy,
            MonitorNotificationPolicy::MaterialChanges
        );
        assert!(!spec.notify_initial_baseline);

        // Field-wise byte stability: re-serializing must reproduce the
        // fixture's exact keys and values (no renamed/dropped/added fields).
        let reserialized = serde_json::to_value(&spec).expect("spec serializes");
        let original: Value = serde_json::from_str(SPEC_FIXTURE).expect("fixture is JSON");
        assert_eq!(
            reserialized, original,
            "MonitorSpecV1 must round-trip the canonical fixture without drift"
        );
    }

    #[test]
    fn validation_accepts_the_canonical_fixture_unchanged() {
        let mut spec = fixture_spec();
        let pristine = spec.clone();
        validate_and_normalize(&mut spec).expect("the canonical fixture must be valid");
        // The fixture is already normalized, so normalization is a no-op.
        assert_eq!(spec, pristine);
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let mut spec = fixture_spec();
        spec.schema_version = 2;
        assert_eq!(
            validate_and_normalize(&mut spec),
            Err("monitor_schema_version_unsupported".to_string())
        );
    }

    #[test]
    fn rejects_empty_objective() {
        let mut spec = fixture_spec();
        spec.objective = "   ".to_string();
        assert_eq!(
            validate_and_normalize(&mut spec),
            Err("monitor_objective_required".to_string())
        );
    }

    #[test]
    fn rejects_over_length_objective() {
        let mut spec = fixture_spec();
        spec.objective = "x".repeat(MAX_OBJECTIVE_CHARS + 1);
        assert_eq!(
            validate_and_normalize(&mut spec),
            Err("monitor_objective_too_long".to_string())
        );
    }

    #[test]
    fn rejects_non_http_source_url_schemes() {
        let mut spec = fixture_spec();
        spec.sources.urls = vec!["javascript:alert(1)".to_string()];
        assert_eq!(
            validate_and_normalize(&mut spec),
            Err("monitor_source_url_scheme_unsupported".to_string())
        );

        let mut spec = fixture_spec();
        spec.sources.urls = vec!["not a url at all".to_string()];
        assert_eq!(
            validate_and_normalize(&mut spec),
            Err("monitor_source_url_invalid".to_string())
        );
    }

    #[test]
    fn rejects_spec_with_no_sources_at_all() {
        let mut spec = fixture_spec();
        spec.query_seeds = vec![" ".to_string()];
        spec.sources.urls = Vec::new();
        spec.sources.domains = vec!["".to_string()];
        assert_eq!(
            validate_and_normalize(&mut spec),
            Err("monitor_sources_required".to_string())
        );
    }

    #[test]
    fn normalization_trims_dedupes_and_drops_empty_strings() {
        let mut spec = fixture_spec();
        spec.objective = "  Watch the pricing page  ".to_string();
        spec.query_seeds = vec![
            " acme pricing ".to_string(),
            "".to_string(),
            "acme pricing".to_string(),
            "acme tiers".to_string(),
        ];
        spec.include_rules = vec!["  ".to_string(), "plans".to_string(), "plans".to_string()];
        spec.sources.urls = vec![
            " https://acme.example/pricing ".to_string(),
            "https://acme.example/pricing".to_string(),
        ];
        spec.sources.domains = vec!["acme.example".to_string(), " acme.example ".to_string()];

        validate_and_normalize(&mut spec).expect("normalizable spec must pass");

        assert_eq!(spec.objective, "Watch the pricing page");
        assert_eq!(
            spec.query_seeds,
            vec!["acme pricing".to_string(), "acme tiers".to_string()]
        );
        assert_eq!(spec.include_rules, vec!["plans".to_string()]);
        assert_eq!(
            spec.sources.urls,
            vec!["https://acme.example/pricing".to_string()]
        );
        assert_eq!(spec.sources.domains, vec!["acme.example".to_string()]);
    }

    #[test]
    fn source_urls_dedupe_on_canonical_form_keeping_the_first_spelling() {
        let mut spec = fixture_spec();
        spec.sources.urls = vec![
            // Four spellings of ONE source: trailing slash, tracking params,
            // host case, and fragment all collapse under §7.1 normalization.
            "https://acme.example/pricing/".to_string(),
            "https://acme.example/pricing".to_string(),
            "https://ACME.example/pricing?utm_source=newsletter".to_string(),
            "https://acme.example/pricing#plans".to_string(),
            // A genuinely different source survives.
            "https://acme.example/plans".to_string(),
        ];
        validate_and_normalize(&mut spec).expect("spec validates");
        assert_eq!(
            spec.sources.urls,
            vec![
                // First spelling wins for the deduped source.
                "https://acme.example/pricing/".to_string(),
                "https://acme.example/plans".to_string(),
            ]
        );

        // Meaningful query params are identity-bearing — NOT deduped away.
        let mut spec = fixture_spec();
        spec.sources.urls = vec![
            "https://dash.example/report?region=eu".to_string(),
            "https://dash.example/report?region=us".to_string(),
        ];
        validate_and_normalize(&mut spec).expect("spec validates");
        assert_eq!(spec.sources.urls.len(), 2);
    }

    #[test]
    fn rejects_oversized_lists_and_entries() {
        let mut spec = fixture_spec();
        spec.query_seeds = (0..(MAX_LIST_ENTRIES + 1))
            .map(|i| format!("seed {i}"))
            .collect();
        assert_eq!(
            validate_and_normalize(&mut spec),
            Err("monitor_query_seeds_too_many_entries".to_string())
        );

        let mut spec = fixture_spec();
        spec.exclude_rules = vec!["y".repeat(MAX_LIST_ENTRY_CHARS + 1)];
        assert_eq!(
            validate_and_normalize(&mut spec),
            Err("monitor_exclude_rules_entry_too_long".to_string())
        );
    }

    // ── Contract fingerprint (Phase 4 preview → create binding) ─────────

    #[test]
    fn contract_fingerprint_is_deterministic_and_input_sensitive() {
        let spec = fixture_spec();
        let schedule = json!({
            "kind": { "type": "Cron", "expression": "0 9 * * *", "timezone": "UTC" }
        });

        let first = monitor_contract_fingerprint(&spec, Some(&schedule));
        let second = monitor_contract_fingerprint(&spec, Some(&schedule));
        assert_eq!(first, second, "same contract ⇒ same fingerprint");
        assert!(first.starts_with("mpv_"), "prefixed: {first}");
        assert_eq!(first.len(), "mpv_".len() + 16);

        // Any spec change invalidates the fingerprint…
        let mut edited = fixture_spec();
        edited.objective = "Something else entirely".to_string();
        assert_ne!(
            first,
            monitor_contract_fingerprint(&edited, Some(&schedule))
        );

        // …and so does any schedule change (the EXACT schedule was reviewed).
        let other_schedule = json!({
            "kind": { "type": "Cron", "expression": "0 18 * * *", "timezone": "UTC" }
        });
        assert_ne!(
            first,
            monitor_contract_fingerprint(&spec, Some(&other_schedule))
        );
        assert_ne!(first, monitor_contract_fingerprint(&spec, None));
    }

    /// T5 — normalization is IDEMPOTENT with respect to the contract
    /// fingerprint: normalizing an already-normalized spec is a no-op, so
    /// preview → create (which re-validates) can never drift fingerprints.
    #[test]
    fn contract_fingerprint_is_stable_under_repeated_normalization() {
        // Start from a messy spec (whitespace, duplicates, dup-by-canonical
        // URLs) and normalize once.
        let mut spec = fixture_spec();
        spec.objective = "  Watch the pricing page  ".to_string();
        spec.query_seeds = vec![
            " acme pricing ".to_string(),
            "acme pricing".to_string(),
            "acme tiers".to_string(),
        ];
        spec.sources.urls = vec![
            "https://acme.example/pricing/".to_string(),
            "https://acme.example/pricing".to_string(),
        ];
        validate_and_normalize(&mut spec).expect("first normalization passes");
        let once = monitor_contract_fingerprint(&spec, None);

        // Normalize AGAIN: the spec must be byte-identical, so the
        // fingerprint is too.
        let mut twice_spec = spec.clone();
        validate_and_normalize(&mut twice_spec).expect("second normalization passes");
        assert_eq!(
            twice_spec, spec,
            "normalize(normalize(spec)) == normalize(spec)"
        );
        assert_eq!(monitor_contract_fingerprint(&twice_spec, None), once);
    }

    /// T5 — ACTUAL ordering semantics: list normalization dedupes
    /// preserving FIRST-SEEN ORDER (it does not sort), so two specs whose
    /// lists differ only in order normalize to differently-ordered lists
    /// and therefore fingerprint DIFFERENTLY. Order is contract-bearing;
    /// only duplicates/whitespace/empties collapse to the same fingerprint.
    #[test]
    fn contract_fingerprint_is_order_sensitive_but_duplicate_insensitive() {
        let base = |seeds: Vec<&str>| {
            let mut spec = fixture_spec();
            spec.query_seeds = seeds.into_iter().map(str::to_string).collect();
            validate_and_normalize(&mut spec).expect("spec validates");
            spec
        };

        let forward = base(vec!["alpha", "beta"]);
        let reversed = base(vec!["beta", "alpha"]);
        assert_ne!(
            monitor_contract_fingerprint(&forward, None),
            monitor_contract_fingerprint(&reversed, None),
            "normalization preserves order, so reordered lists are a different contract"
        );

        // Duplicates and cosmetic whitespace DO collapse: same fingerprint.
        let noisy = base(vec!["alpha", "  alpha  ", "beta", "beta"]);
        assert_eq!(
            monitor_contract_fingerprint(&noisy, None),
            monitor_contract_fingerprint(&forward, None),
            "dedupe + trim normalize to the identical contract"
        );
    }

    // ── TaskManifest carrier serialization (plan §6.1 back-compat) ──────

    /// A pre-monitor manifest as written by older builds — no
    /// `monitor_spec` / `monitor_revision` keys anywhere.
    fn legacy_manifest_json() -> Value {
        json!({
            "task_id": "task_legacy_1",
            "principal": "anonymous",
            "workspace": "default",
            "title": "Legacy task",
            "description": "Created before monitors existed",
            "agent_id": "personal-assistant",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        })
    }

    #[test]
    fn legacy_manifest_without_monitor_fields_deserializes_as_non_monitor() {
        let manifest: TaskManifest = serde_json::from_value(legacy_manifest_json())
            .expect("legacy manifests must keep deserializing");
        assert!(manifest.monitor_spec.is_none());
        assert_eq!(manifest.monitor_revision, 0);
    }

    #[test]
    fn non_monitor_manifest_serializes_without_monitor_keys() {
        let manifest: TaskManifest =
            serde_json::from_value(legacy_manifest_json()).expect("manifest decodes");
        let serialized = serde_json::to_value(&manifest).expect("manifest serializes");
        assert!(
            serialized.get("monitor_spec").is_none(),
            "non-monitors must not emit monitor_spec"
        );
        assert!(
            serialized.get("monitor_revision").is_none(),
            "revision 0 (= not a monitor) must be omitted from the wire"
        );
    }

    #[test]
    fn manifest_with_monitor_spec_round_trips() {
        let mut manifest: TaskManifest =
            serde_json::from_value(legacy_manifest_json()).expect("manifest decodes");
        manifest.monitor_spec = Some(fixture_spec());
        manifest.monitor_revision = 3;

        let serialized = serde_json::to_value(&manifest).expect("manifest serializes");
        assert_eq!(serialized["monitor_revision"], 3);
        assert_eq!(
            serialized["monitor_spec"],
            serde_json::from_str::<Value>(SPEC_FIXTURE).expect("fixture is JSON"),
        );

        let round_tripped: TaskManifest =
            serde_json::from_value(serialized).expect("manifest round-trips");
        assert_eq!(round_tripped.monitor_spec, Some(fixture_spec()));
        assert_eq!(round_tripped.monitor_revision, 3);
    }
}
