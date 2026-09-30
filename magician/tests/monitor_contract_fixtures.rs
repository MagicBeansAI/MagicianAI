//! Recurring Monitors Phase 0 — contract fixture compatibility checks.
//!
//! The JSON files under `tests/fixtures/monitors/` are the CANONICAL wire
//! contract for `MonitorSpecV1` / `MonitorRunResultV1` / the list + update
//! projections (plan: docs/plans/2026-07-21-recurring-monitors-
//! productization-design-implementation.md §6–§8). Web (vitest) and iOS
//! (XCTest) read the same files, so all three platforms break together if a
//! fixture drifts. Phase 1 replaces the structural assertions here with
//! typed serde decodes; the invariants below (removal safety, fingerprint
//! presence, dedupe-key composition) must keep holding.

use serde_json::Value;

fn fixture(name: &str) -> Value {
    let raw = match name {
        "spec" => include_str!("fixtures/monitors/monitor_spec_v1.json"),
        "changed" => include_str!("fixtures/monitors/monitor_run_result_v1_changed.json"),
        "unchanged" => include_str!("fixtures/monitors/monitor_run_result_v1_unchanged.json"),
        "degraded" => include_str!("fixtures/monitors/monitor_run_result_v1_degraded.json"),
        "list" => include_str!("fixtures/monitors/monitor_list_page_v1.json"),
        "update" => include_str!("fixtures/monitors/monitor_update_detail_v1.json"),
        other => panic!("unknown fixture {other}"),
    };
    serde_json::from_str(raw).unwrap_or_else(|e| panic!("fixture {name} is not valid JSON: {e}"))
}

fn require_keys(value: &Value, keys: &[&str], ctx: &str) {
    for key in keys {
        assert!(
            value.get(key).is_some(),
            "{ctx}: required key `{key}` missing"
        );
    }
}

#[test]
fn monitor_spec_v1_shape() {
    let spec = fixture("spec");
    require_keys(
        &spec,
        &[
            "schema_version",
            "objective",
            "query_seeds",
            "sources",
            "include_rules",
            "exclude_rules",
            "match_mode",
            "notification_policy",
            "notify_initial_baseline",
        ],
        "MonitorSpecV1",
    );
    assert_eq!(
        spec["schema_version"], 1,
        "unknown schema versions must fail clearly"
    );
    require_keys(
        &spec["sources"],
        &["urls", "domains", "authenticated_sources"],
        "MonitorSpecV1.sources",
    );
    assert!(
        matches!(
            spec["match_mode"].as_str(),
            Some("strict" | "balanced" | "broad")
        ),
        "match_mode enum"
    );
    assert!(
        matches!(
            spec["notification_policy"].as_str(),
            Some("material_changes" | "every_run" | "never")
        ),
        "notification_policy enum"
    );
    for url in spec["sources"]["urls"].as_array().expect("urls array") {
        let parsed = url.as_str().expect("url string");
        assert!(
            url::Url::parse(parsed).is_ok(),
            "source urls must be parseable: {parsed}"
        );
    }
}

fn assert_run_result_shape(run: &Value, ctx: &str) {
    require_keys(
        run,
        &[
            "monitor_task_id",
            "execution_id",
            "monitor_revision",
            "started_at",
            "completed_at",
            "status",
            "complete_scan",
            "source_outcomes",
            "counts",
            "findings",
            "run_fingerprint",
        ],
        ctx,
    );
    assert!(
        matches!(
            run["status"].as_str(),
            Some("baseline" | "changed" | "unchanged" | "degraded" | "failed")
        ),
        "{ctx}: status enum"
    );
    require_keys(
        &run["counts"],
        &["scanned", "new", "updated", "unchanged", "possibly_removed"],
        &format!("{ctx}.counts"),
    );
    for finding in run["findings"].as_array().expect("findings array") {
        require_keys(
            finding,
            &[
                "stable_key",
                "title",
                "source",
                "observed_at",
                "summary",
                "why_it_matters",
                "entities",
                "evidence",
                "content_fingerprint",
                "classification",
            ],
            &format!("{ctx}.finding"),
        );
        assert!(
            matches!(
                finding["classification"].as_str(),
                Some("new" | "updated" | "unchanged" | "possibly_removed")
            ),
            "{ctx}: finding classification enum"
        );
    }
}

#[test]
fn run_result_changed_carries_change_fingerprint() {
    let run = fixture("changed");
    assert_run_result_shape(&run, "changed run");
    assert_eq!(run["status"], "changed");
    assert!(run["complete_scan"].as_bool().unwrap());
    assert!(
        run["change_fingerprint"].as_str().is_some(),
        "a changed run must carry the change fingerprint the dedupe key is built from"
    );
    assert!(!run["findings"].as_array().unwrap().is_empty());
}

#[test]
fn run_result_unchanged_has_no_change_fingerprint() {
    let run = fixture("unchanged");
    assert_run_result_shape(&run, "unchanged run");
    assert_eq!(run["status"], "unchanged");
    assert!(
        run.get("change_fingerprint").is_none(),
        "an unchanged run must NOT carry a change fingerprint — nothing to notify or dedupe"
    );
}

#[test]
fn run_result_degraded_respects_removal_safety() {
    let run = fixture("degraded");
    assert_run_result_shape(&run, "degraded run");
    assert_eq!(run["status"], "degraded");
    // §7.3 removal safety: an incomplete/auth-failed scan can never make
    // items look deleted.
    assert!(
        !run["complete_scan"].as_bool().unwrap(),
        "a degraded run must mark the scan incomplete"
    );
    assert_eq!(
        run["counts"]["possibly_removed"], 0,
        "removal safety: incomplete scans report zero possibly_removed"
    );
    let outcomes = run["source_outcomes"].as_array().unwrap();
    assert!(
        outcomes
            .iter()
            .any(|o| o["status"] == "auth_failed" && o["complete"] == false),
        "the degraded fixture models an auth failure"
    );
    require_keys(
        &run["access_problem"],
        &["source", "kind", "message", "since"],
        "degraded run access_problem",
    );
}

#[test]
fn list_page_uses_cursor_envelope() {
    let page = fixture("list");
    // Plan §8: GET /monitors?limit=&cursor= — cursor envelope, not offset.
    // `total` and `offset` were added additively: the cursor still carries
    // next/prev, and the two counts carry what a cursor cannot express —
    // how many pages there are and which one the reader is on.
    require_keys(
        &page,
        &["items", "next_cursor", "limit", "total", "offset"],
        "monitor list page",
    );
    let items = page["items"].as_array().expect("items array");
    let total = page["total"].as_u64().expect("total is a number");
    let offset = page["offset"].as_u64().expect("offset is a number");
    assert!(
        offset + items.len() as u64 <= total,
        "a page cannot end past the corpus it is a window onto: \
         offset {offset} + {} rows > total {total}",
        items.len()
    );
    // The fixture deliberately models a page that is NOT the last one, so
    // the three platforms all see a non-null cursor decoded at least once.
    // A `total` equal to the rows on the page would describe a pager with
    // nothing left to fetch and a cursor still pointing at it.
    assert!(
        page["next_cursor"].is_string(),
        "the fixture models a mid-corpus page"
    );
    assert!(
        offset + (items.len() as u64) < total,
        "next_cursor set means there is more corpus after this page"
    );
    for item in items {
        require_keys(
            item,
            &[
                "task_id",
                "title",
                "objective",
                "state",
                "cadence_summary",
                "monitor_revision",
                "last_run_status",
                "health",
            ],
            "monitor list item",
        );
    }
}

#[test]
fn all_timestamps_are_rfc3339() {
    for name in ["changed", "unchanged", "degraded"] {
        let run = fixture(name);
        for key in ["started_at", "completed_at"] {
            let raw = run[key].as_str().unwrap();
            assert!(
                chrono::DateTime::parse_from_rfc3339(raw).is_ok(),
                "{name}.{key} must be RFC3339: {raw}"
            );
        }
        for finding in run["findings"].as_array().unwrap() {
            let observed = finding["observed_at"].as_str().unwrap();
            assert!(
                chrono::DateTime::parse_from_rfc3339(observed).is_ok(),
                "{name} finding observed_at must be RFC3339: {observed}"
            );
        }
    }
    let update = fixture("update");
    let occurred = update["occurred_at"].as_str().unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(occurred).is_ok());
}

#[test]
fn counts_arithmetic_holds_for_complete_scans() {
    // For a COMPLETE scan every scanned item lands in exactly one bucket.
    // (Degraded/incomplete scans are exempt — partial sources can't bucket
    // what they never saw.)
    for name in ["changed", "unchanged"] {
        let run = fixture(name);
        assert!(run["complete_scan"].as_bool().unwrap());
        let counts = &run["counts"];
        let bucketed = counts["new"].as_u64().unwrap()
            + counts["updated"].as_u64().unwrap()
            + counts["unchanged"].as_u64().unwrap();
        assert_eq!(
            counts["scanned"].as_u64().unwrap(),
            bucketed,
            "{name}: scanned must equal new+updated+unchanged on a complete scan"
        );
    }
}

#[test]
fn cross_fixture_consistency() {
    // The update detail's findings are exactly the producing run's material
    // (non-unchanged) findings — same stable keys, same content fingerprints.
    let changed = fixture("changed");
    let update = fixture("update");
    let run_keys: Vec<(&str, &str)> = changed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["classification"] != "unchanged")
        .map(|f| {
            (
                f["stable_key"].as_str().unwrap(),
                f["content_fingerprint"].as_str().unwrap(),
            )
        })
        .collect();
    let update_keys: Vec<(&str, &str)> = update["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["stable_key"].as_str().unwrap(),
                f["content_fingerprint"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(update_keys, run_keys);
    assert_eq!(update["monitor_task_id"], changed["monitor_task_id"]);
    assert_eq!(update["execution_id"], changed["execution_id"]);
    assert_eq!(update["monitor_revision"], changed["monitor_revision"]);

    // The list page's needs_attention row is the degraded fixture's monitor,
    // and its healthy row is the changed/unchanged fixture's monitor with the
    // most recent (unchanged) run reflected.
    let list = fixture("list");
    let degraded = fixture("degraded");
    let unchanged = fixture("unchanged");
    let items = list["items"].as_array().unwrap();
    let unhealthy = items
        .iter()
        .find(|i| i["health"] == "needs_attention")
        .expect("list fixture models one needs_attention monitor");
    assert_eq!(unhealthy["task_id"], degraded["monitor_task_id"]);
    assert_eq!(unhealthy["last_run_status"], "degraded");
    let healthy = items.iter().find(|i| i["health"] == "ok").unwrap();
    assert_eq!(healthy["task_id"], unchanged["monitor_task_id"]);
    assert_eq!(healthy["last_run_status"], "unchanged");
    assert_eq!(healthy["last_run_at"], unchanged["completed_at"]);
}

#[test]
fn update_detail_dedupe_key_composition() {
    let update = fixture("update");
    require_keys(
        &update,
        &[
            "update_id",
            "monitor_task_id",
            "monitor_revision",
            "execution_id",
            "occurred_at",
            "status",
            "change_fingerprint",
            "headline",
            "summary",
            "findings",
            "notification",
        ],
        "monitor update detail",
    );
    let notification = &update["notification"];
    require_keys(
        notification,
        &["policy", "emitted", "channel", "dedupe_key"],
        "update notification",
    );
    // §7.4: dedupe key = (scope, monitor_task_id, monitor_revision,
    // change_fingerprint, channel). The fixture's key must contain every
    // component so retries/restarts can never double-notify.
    let key = notification["dedupe_key"].as_str().unwrap();
    for component in [
        update["monitor_task_id"].as_str().unwrap(),
        &update["monitor_revision"].to_string(),
        update["change_fingerprint"].as_str().unwrap(),
        notification["channel"].as_str().unwrap(),
    ] {
        assert!(
            key.contains(component),
            "dedupe key missing component `{component}`: {key}"
        );
    }
    // The update's findings mirror the changed run's material findings.
    let changed = fixture("changed");
    assert_eq!(
        update["change_fingerprint"], changed["change_fingerprint"],
        "update detail and its producing run share one change fingerprint"
    );
}
