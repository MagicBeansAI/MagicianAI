//! Per-operation Decision Model health over the retained recent cases.
//!
//! Reads only the bounded `cases` table (7 days, at most 2,048 rows) through a
//! read-only connection. Outcomes are derived from closed status codes; no
//! provider error text is stored, so none can be returned.
use super::types::Case;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const DEFAULT_HOURS: u32 = 24;
/// Cases are retained for 7 days; a longer window would silently under-count.
pub const MAX_HOURS: u32 = 168;
const HOUR: i64 = 3_600_000;
const MAX_CASES: usize = 2048;
const MAX_TEXT: usize = 96;
const MAX_CAUSES: usize = 8;
const MAX_MODELS: usize = 8;
const MAX_ROUTE_MODELS: usize = 8;
const MAX_OPERATIONS: usize = 64;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthQuery {
    pub hours: Option<u32>,
}
impl HealthQuery {
    pub fn hours(&self) -> Result<u32, String> {
        match self.hours.unwrap_or(DEFAULT_HOURS) {
            hours @ 1..=MAX_HOURS => Ok(hours),
            _ => Err(format!("hours must be between 1 and {MAX_HOURS}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every offered item answered (and, in gate mode, cleared its thresholds).
    Decided,
    /// Gate mode: answered, but below threshold, so the caller fell back/held.
    Unresolved,
    /// Engine call timed out, or items ran out of execution budget.
    Timeout,
    /// Engine unreachable/invalid reply, or items failed / had no fitting model.
    Error,
    /// Never reached the engine (disabled, policy unavailable, admission...).
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classified {
    pub outcome: Outcome,
    pub cause: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ItemCounts {
    pub offered: i64,
    pub answered: i64,
    pub eligible: i64,
    pub failed: i64,
    pub no_fitting_model: i64,
    pub cancelled: i64,
    pub not_started: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ModelHealth {
    pub model: String,
    pub answered: i64,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RouteHealth {
    pub model: String,
    /// Closed engine issue code; None means the engine saw the model healthy.
    pub issue: Option<String>,
    pub observed_ms: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OperationHealth {
    pub operation: String,
    /// Mode of the most recent engine case (`gate` or `shadow`).
    pub mode: Option<String>,
    /// Cases that reached the engine (decided + unresolved + timeout + error).
    pub cases: i64,
    pub decided: i64,
    pub unresolved: i64,
    pub timeout: i64,
    pub error: i64,
    /// Not counted in `cases`: these never reached the engine.
    pub skipped: i64,
    pub items: ItemCounts,
    /// Exact nearest-rank percentiles of caller wall latency per case.
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    /// Most recent budget granted by the caller (absent on older records).
    pub budget_ms: Option<u64>,
    pub last_case_ms: i64,
    /// Most recent timeout or error.
    pub last_error_ms: Option<i64>,
    pub last_error: Option<String>,
    pub causes: BTreeMap<String, i64>,
    pub models: Vec<ModelHealth>,
    pub route_health: Vec<RouteHealth>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HealthReport {
    pub window_hours: u32,
    pub since_ms: i64,
    pub generated_at_ms: i64,
    pub cases_scanned: usize,
    pub unreadable_cases: usize,
    /// The scan stopped at the row bound; older cases in the window are missing.
    pub truncated: bool,
    pub operations: Vec<OperationHealth>,
    pub capture_dropped: u64,
    pub write_errors: u64,
    pub read_ms: f64,
}

fn bounded(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect()
}

fn item_status(item: &Value) -> &str {
    item["status"].as_str().unwrap_or("")
}

/// First unhealthy route model, as `model issue`.
fn route_issue(primary: &Value) -> Option<String> {
    primary["model_health"].as_object().and_then(|health| {
        health
            .iter()
            .find_map(|(model, issue)| issue.as_str().map(|i| bounded(&format!("{model} {i}"))))
    })
}

/// Pure classification of one retained case. `None` means the case carries no
/// outcome yet (for example, only a late reference stage has arrived).
pub fn classify(case: &Case) -> Option<Classified> {
    let Some(primary) = &case.primary else {
        if let Some(status) = case.invocation_status.as_deref() {
            return Some(Classified {
                outcome: Outcome::Skipped,
                cause: Some(bounded(status)),
            });
        }
        // Invocation records written before `invocation_status` existed leave
        // an empty shell. A late reference/application stage is not one.
        let shell = case.applications.is_empty()
            && case.reference.is_none()
            && case.reference_status.is_none()
            && case.projection.is_none();
        return shell.then(|| Classified {
            outcome: Outcome::Skipped,
            cause: Some("skipped_unrecorded".into()),
        });
    };
    let failure = primary["failure"].as_str();
    let with_route = |cause: &str| match route_issue(primary) {
        Some(issue) => bounded(&format!("{cause}: {issue}")),
        None => bounded(cause),
    };
    // Never sent (policy expired, budget spent, classification switched):
    // not the engine's health.
    if let Some(cause) = failure.filter(|cause| cause.starts_with("not_dispatched")) {
        return Some(Classified {
            outcome: Outcome::Skipped,
            cause: Some(bounded(cause)),
        });
    }
    // accounting_complete is false exactly when the engine returned no reply.
    if primary["accounting_complete"] != true {
        let cause = failure.unwrap_or("no_engine_reply");
        return Some(Classified {
            outcome: if cause == "timeout" {
                Outcome::Timeout
            } else {
                Outcome::Error
            },
            cause: Some(bounded(cause)),
        });
    }
    let empty = Vec::new();
    let items = primary["items"].as_array().unwrap_or(&empty);
    let count = |status: &str| items.iter().filter(|i| item_status(i) == status).count();
    let (failed, no_fit) = (count("failed"), count("no_fitting_model"));
    let (cancelled, not_started) = (count("cancelled"), count("not_started"));
    if failed + no_fit > 0 {
        let cause = if failed > 0 {
            "item_failed"
        } else {
            "no_fitting_model"
        };
        return Some(Classified {
            outcome: Outcome::Error,
            cause: Some(with_route(cause)),
        });
    }
    if cancelled + not_started > 0 {
        return Some(Classified {
            outcome: Outcome::Timeout,
            cause: Some(with_route("budget_exhausted")),
        });
    }
    if let Some(failure) = failure {
        return Some(Classified {
            outcome: Outcome::Error,
            cause: Some(with_route(failure)),
        });
    }
    if items.is_empty() {
        return Some(Classified {
            outcome: Outcome::Skipped,
            cause: Some("empty_batch".into()),
        });
    }
    let required: Vec<&str> = primary["required_questions"]
        .as_array()
        .map(|v| v.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let below_threshold = primary["mode"] == "gate"
        && !required.is_empty()
        && items.iter().any(|item| {
            !required
                .iter()
                .all(|head| item["eligible_answers"][*head].is_string())
        });
    Some(if below_threshold {
        Classified {
            outcome: Outcome::Unresolved,
            cause: Some("below_threshold".into()),
        }
    } else {
        Classified {
            outcome: Outcome::Decided,
            cause: None,
        }
    })
}

fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    Some(sorted[rank - 1])
}

#[derive(Default)]
struct Acc {
    health: Option<OperationHealth>,
    latencies: Vec<u64>,
    models: BTreeMap<String, (i64, Vec<u64>)>,
}

/// Aggregate cases ordered newest first (the store's index order).
pub fn aggregate(cases: &[Case]) -> Vec<OperationHealth> {
    let mut ops: BTreeMap<String, Acc> = BTreeMap::new();
    for case in cases {
        let Some(classified) = classify(case) else {
            continue;
        };
        if !ops.contains_key(&case.operation) && ops.len() >= MAX_OPERATIONS {
            continue;
        }
        let acc = ops.entry(case.operation.clone()).or_default();
        let h = acc.health.get_or_insert_with(|| OperationHealth {
            operation: bounded(&case.operation),
            mode: None,
            cases: 0,
            decided: 0,
            unresolved: 0,
            timeout: 0,
            error: 0,
            skipped: 0,
            items: ItemCounts::default(),
            p50_ms: None,
            p95_ms: None,
            budget_ms: None,
            last_case_ms: case.at_ms,
            last_error_ms: None,
            last_error: None,
            causes: BTreeMap::new(),
            models: Vec::new(),
            route_health: Vec::new(),
        });
        h.last_case_ms = h.last_case_ms.max(case.at_ms);
        match classified.outcome {
            Outcome::Decided => h.decided += 1,
            Outcome::Unresolved => h.unresolved += 1,
            Outcome::Timeout => h.timeout += 1,
            Outcome::Error => h.error += 1,
            Outcome::Skipped => h.skipped += 1,
        }
        if let Some(cause) = &classified.cause {
            if h.causes.contains_key(cause) || h.causes.len() < MAX_CAUSES {
                *h.causes.entry(cause.clone()).or_default() += 1;
            } else {
                *h.causes.entry("other".into()).or_default() += 1;
            }
            if matches!(classified.outcome, Outcome::Timeout | Outcome::Error)
                && h.last_error_ms.is_none_or(|at| case.at_ms > at)
            {
                h.last_error_ms = Some(case.at_ms);
                h.last_error = Some(cause.clone());
            }
        }
        if classified.outcome == Outcome::Skipped {
            continue;
        }
        h.cases += 1;
        let Some(primary) = &case.primary else {
            continue;
        };
        if h.mode.is_none() {
            h.mode = primary["mode"].as_str().map(bounded);
        }
        if h.budget_ms.is_none() {
            h.budget_ms = primary["budget_ms"].as_u64();
        }
        if h.route_health.is_empty() {
            if let Some(health) = primary["model_health"].as_object() {
                h.route_health = health
                    .iter()
                    .take(MAX_ROUTE_MODELS)
                    .map(|(model, issue)| RouteHealth {
                        model: bounded(model),
                        issue: issue.as_str().map(bounded),
                        observed_ms: case.at_ms,
                    })
                    .collect();
            }
        }
        // A shadow case's wall time also waits for the incumbent it is
        // compared with, so the engine's own latency is what it spent.
        let wall = primary["wall_latency_ms"].as_u64();
        let latency = if primary["mode"] == "shadow" {
            primary["engine_latency_ms"].as_u64().or(wall)
        } else {
            wall
        };
        if let Some(ms) = latency {
            acc.latencies.push(ms);
        }
        let required: Vec<&str> = primary["required_questions"]
            .as_array()
            .map(|v| v.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for item in primary["items"].as_array().into_iter().flatten() {
            h.items.offered += 1;
            match item_status(item) {
                "answered" => h.items.answered += 1,
                "failed" => h.items.failed += 1,
                "no_fitting_model" => h.items.no_fitting_model += 1,
                "cancelled" => h.items.cancelled += 1,
                "not_started" => h.items.not_started += 1,
                _ => {},
            }
            if !required.is_empty()
                && required
                    .iter()
                    .all(|head| item["eligible_answers"][*head].is_string())
            {
                h.items.eligible += 1;
            }
            if item_status(item) == "answered" {
                if let Some(model) = item["response"]["model"]["model"].as_str() {
                    if acc.models.contains_key(model) || acc.models.len() < MAX_MODELS {
                        let entry = acc.models.entry(bounded(model)).or_default();
                        entry.0 += 1;
                        if let Some(ms) = item["latency_ms"].as_u64() {
                            entry.1.push(ms);
                        }
                    }
                }
            }
        }
    }
    ops.into_values()
        .filter_map(|mut acc| {
            let mut h = acc.health.take()?;
            acc.latencies.sort_unstable();
            h.p50_ms = percentile(&acc.latencies, 0.5);
            h.p95_ms = percentile(&acc.latencies, 0.95);
            h.models = acc
                .models
                .into_iter()
                .map(|(model, (answered, mut latencies))| {
                    latencies.sort_unstable();
                    ModelHealth {
                        model,
                        answered,
                        p50_ms: percentile(&latencies, 0.5),
                        p95_ms: percentile(&latencies, 0.95),
                    }
                })
                .collect();
            Some(h)
        })
        .collect()
}

/// Bounded read of the cases in `[now - hours, now]`.
pub fn health(conn: &Connection, q: &HealthQuery, now: i64) -> Result<HealthReport, String> {
    let hours = q.hours()?;
    let since = now - i64::from(hours) * HOUR;
    let mut stmt = conn
        .prepare(
            "SELECT data FROM cases WHERE at_ms>=?1 AND at_ms<=?2 ORDER BY at_ms DESC,id DESC LIMIT ?3",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![since, now, (MAX_CASES + 1) as i64], |r| {
            r.get::<_, String>(0)
        })
        .map_err(|e| e.to_string())?;
    let mut cases = Vec::new();
    let mut scanned = 0;
    let mut unreadable = 0;
    for row in rows {
        scanned += 1;
        if scanned > MAX_CASES {
            break;
        }
        match serde_json::from_str::<Case>(&row.map_err(|e| e.to_string())?) {
            Ok(case) => cases.push(case),
            Err(_) => unreadable += 1,
        }
    }
    let truncated = scanned > MAX_CASES;
    Ok(HealthReport {
        window_hours: hours,
        since_ms: since,
        generated_at_ms: now,
        cases_scanned: scanned.min(MAX_CASES),
        unreadable_cases: unreadable,
        truncated,
        operations: aggregate(&cases),
        capture_dropped: 0,
        write_errors: 0,
        read_ms: 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::{super::store, *};
    use serde_json::json;

    const AT: i64 = 10 * store::DAY;

    fn gate(id: &str, operation: &str, wall_ms: u64, items: Value) -> Value {
        json!({"comparison_id":id,"operation":operation,"mode":"gate","required_questions":["relation"],
            "accounting_complete":true,"wall_latency_ms":wall_ms,"budget_ms":3000,"items":items})
    }
    fn answered(model: &str, eligible: bool, latency: u64) -> Value {
        let eligible_answers = if eligible {
            json!({"relation":"operator-enabled:x"})
        } else {
            json!({})
        };
        json!({"item_id":"i","status":"answered","latency_ms":latency,
            "eligible_answers": eligible_answers,
            "response":{"model":{"model":model},"answers":{"relation":{"choice":"consistent"}}}})
    }

    #[test]
    fn health_classifies_outcomes_causes_and_latency_from_real_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("decision_metrics.sqlite");
        let mut c = store::open(&path, true).unwrap();
        let op = "action_alignment";
        // Decided x2, unresolved x1.
        store::event(
            &mut c,
            AT,
            &gate("d1", op, 400, json!([answered("jev", true, 300)])),
        )
        .unwrap();
        store::event(
            &mut c,
            AT + 1,
            &gate("d2", op, 600, json!([answered("jev", true, 500)])),
        )
        .unwrap();
        store::event(
            &mut c,
            AT + 2,
            &gate("u1", op, 800, json!([answered("jev", false, 700)])),
        )
        .unwrap();
        // Whole-call timeout: no reply, capture fills not_started items.
        let mut t = gate(
            "t1",
            op,
            3000,
            json!([{"item_id":"i","status":"not_started","response":null}]),
        );
        t["accounting_complete"] = json!(false);
        t["failure"] = json!("timeout");
        store::event(&mut c, AT + 3, &t).unwrap();
        // Historical no-reply record without a failure code.
        let mut h = gate(
            "h1",
            op,
            2000,
            json!([{"item_id":"i","status":"not_started","response":null}]),
        );
        h["accounting_complete"] = json!(false);
        store::event(&mut c, AT + 4, &h).unwrap();
        // Provider failure with the engine's route health issue.
        let mut e = gate(
            "e1",
            op,
            900,
            json!([{"item_id":"i","status":"failed","error":"Failed","response":null}]),
        );
        e["model_health"] = json!({"jev":"structured_provider_rate_limit","kev":null});
        store::event(&mut c, AT + 5, &e).unwrap();
        // Budget exhausted inside the engine.
        store::event(
            &mut c,
            AT + 6,
            &gate(
                "c1",
                op,
                3100,
                json!([{"item_id":"i","status":"cancelled","response":null}]),
            ),
        )
        .unwrap();
        // Never reached the engine: skipped, excluded from the case denominator.
        store::event(&mut c, AT + 7, &json!({"comparison_id":"s1","operation":op,"stage":"invocation","status":"policy_unavailable"})).unwrap();
        // Not dispatched (classification switched before the call): skipped,
        // never a timeout against the engine.
        let mut nd = gate(
            "nd1",
            op,
            5,
            json!([{"item_id":"i","status":"not_started","response":null}]),
        );
        nd["accounting_complete"] = json!(false);
        nd["failure"] = json!("not_dispatched:classification_changed");
        store::event(&mut c, AT + 7, &nd).unwrap();
        // Historical invocation shell (no status recorded) is skipped too.
        store::event(
            &mut c,
            AT + 7,
            &json!({"comparison_id":"s2","operation":op,"stage":"invocation"}),
        )
        .unwrap();
        // A late reference stage without primary evidence carries no outcome.
        store::event(&mut c, AT + 7, &json!({"comparison_id":"r1","operation":op,"stage":"reference","status":"completed","reference":{"labels":{}}})).unwrap();
        // Shadow cases never count as unresolved.
        let mut shadow = gate(
            "sh1",
            "channel_classify",
            100,
            json!([answered("kev", false, 90)]),
        );
        shadow["mode"] = json!("shadow");
        store::event(&mut c, AT + 8, &shadow).unwrap();
        // Out of window.
        store::event(
            &mut c,
            AT - 30 * HOUR,
            &gate("old", op, 1, json!([answered("jev", true, 1)])),
        )
        .unwrap();
        drop(c);

        let conn = store::open(&path, false).unwrap();
        let report = health(&conn, &HealthQuery { hours: Some(24) }, AT + 10).unwrap();
        assert!(!report.truncated);
        assert_eq!(report.unreadable_cases, 0);
        let a = report
            .operations
            .iter()
            .find(|o| o.operation == op)
            .unwrap();
        assert_eq!(
            (
                a.cases,
                a.decided,
                a.unresolved,
                a.timeout,
                a.error,
                a.skipped
            ),
            (7, 2, 1, 2, 2, 3)
        );
        assert_eq!(a.causes.get("timeout"), Some(&1));
        assert_eq!(a.causes.get("no_engine_reply"), Some(&1));
        assert_eq!(a.causes.get("budget_exhausted"), Some(&1));
        assert_eq!(
            a.causes
                .get("item_failed: jev structured_provider_rate_limit"),
            Some(&1)
        );
        assert_eq!(a.causes.get("policy_unavailable"), Some(&1));
        assert_eq!(a.causes.get("skipped_unrecorded"), Some(&1));
        assert_eq!(
            a.causes.get("not_dispatched:classification_changed"),
            Some(&1)
        );
        assert_eq!(a.causes.get("below_threshold"), Some(&1));
        // Most recent timeout/error is the in-engine budget exhaustion.
        assert_eq!(a.last_error_ms, Some(AT + 6));
        assert_eq!(a.last_error.as_deref(), Some("budget_exhausted"));
        assert_eq!(a.mode.as_deref(), Some("gate"));
        assert_eq!(a.budget_ms, Some(3000));
        // Latencies 400,600,800,3000,2000,900,3100 -> sorted p50=900, p95=3100.
        assert_eq!((a.p50_ms, a.p95_ms), (Some(900), Some(3100)));
        assert_eq!(a.items.offered, 7);
        assert_eq!((a.items.answered, a.items.eligible), (3, 2));
        assert_eq!(
            (a.items.failed, a.items.cancelled, a.items.not_started),
            (1, 1, 2)
        );
        assert_eq!(
            a.models,
            vec![ModelHealth {
                model: "jev".into(),
                answered: 3,
                p50_ms: Some(500),
                p95_ms: Some(700)
            }]
        );
        assert_eq!(a.route_health.len(), 2);
        assert_eq!(
            a.route_health[0].issue.as_deref(),
            Some("structured_provider_rate_limit")
        );
        let s = report
            .operations
            .iter()
            .find(|o| o.operation == "channel_classify")
            .unwrap();
        assert_eq!((s.cases, s.decided, s.unresolved), (1, 1, 0));
    }

    #[test]
    fn health_rejects_unbounded_windows_and_bounds_cause_text() {
        assert!(HealthQuery { hours: Some(0) }.hours().is_err());
        assert!(HealthQuery {
            hours: Some(MAX_HOURS + 1)
        }
        .hours()
        .is_err());
        assert_eq!(HealthQuery::default().hours().unwrap(), DEFAULT_HOURS);
        let case = Case {
            id: "x".into(),
            at_ms: AT,
            operation: "op".into(),
            primary: Some(json!({"accounting_complete":false,"failure":"x\n".repeat(200)})),
            ..Default::default()
        };
        let cause = classify(&case).unwrap().cause.unwrap();
        assert!(cause.chars().count() <= MAX_TEXT && !cause.contains('\n'));
    }
}
