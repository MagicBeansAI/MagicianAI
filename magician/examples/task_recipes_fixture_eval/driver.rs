//! The phase runner: one task execution per phase, with the common gates
//! every warm/variant phase must pass and the evidence bundle attached when a
//! gate fails.

use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::{
    client::{BrowserSignals, Magician, TerminalOutcome},
    evidence::{self, LogWindow},
    report::{Diagnostics, Gate, Phase},
    sites::{RequestKind, RequestRecord, RunningSite},
};

pub const RECIPE_REPLAY: &str = "recipe_replay";
pub const PREAMBLE: &str = "API-mining fixture evaluation. Use the browser tool, not web_search or web_fetch. The site's data changes between runs, so read the page now; never answer from memory of an earlier run. Do not log in unless the task says so. This runs unattended with no human to answer questions: never pause for user input — every credential and value you need is already stated in this task, so use it directly (type the given password into the form; do not ask for it). Report the answer as a plain value in your final summary. ";

#[derive(Debug, Clone, Default)]
pub struct Expect<'a> {
    /// Every needle must appear in the outcome summary.
    pub answer_contains: &'a [&'a str],
    /// `Some(kind)` requires `outcome_type == kind`.
    pub outcome_type: Option<&'a str>,
    /// `Some(kind)` requires `outcome_type != kind` (drift handoff).
    pub not_outcome_type: Option<&'a str>,
    /// Apply the browserless gates (nonce, beacon, signals, timeline).
    pub browserless: bool,
    /// C4: exactly one auth heal is allowed; beacons are tolerated only
    /// between the first 401 and the next 2xx on the healed path.
    pub allow_one_heal: bool,
}

pub struct PhaseCtx<'a> {
    pub magician: &'a Magician,
    pub magicutor_base: &'a str,
    pub site: &'a RunningSite,
    pub runtime_root: &'a Path,
    pub log_path: &'a Path,
    pub timeout: Duration,
    pub ui_thread: &'a str,
    /// The case's recipe once one exists (diagnostics + step paths).
    pub recipe: Option<&'a Value>,
}

/// A phase's raw observations, kept so case-specific gates can add to the
/// common ones without re-fetching.
pub struct Observed {
    pub task_id: String,
    pub execution_id: String,
    pub outcome: TerminalOutcome,
    pub requests: Vec<RequestRecord>,
    pub events: Vec<Value>,
    pub approvals_seen: Vec<Value>,
    pub signals_before: BrowserSignals,
    pub signals_after: BrowserSignals,
    pub log_window: LogWindow,
    pub started: Instant,
}

// ---------------------------------------------------------------------------
// Pure gate helpers (unit-tested)
// ---------------------------------------------------------------------------

/// Case-insensitive containment that ignores thousands separators and
/// whitespace inside numbers so `1,284.50` matches `1284.50`.
pub fn answer_matches(summary: &str, needle: &str) -> bool {
    fn normalize(text: &str) -> String {
        let lower = text.to_ascii_lowercase();
        let mut out = String::with_capacity(lower.len());
        let chars: Vec<char> = lower.chars().collect();
        for (index, ch) in chars.iter().enumerate() {
            let between_digits = index > 0
                && index + 1 < chars.len()
                && chars[index - 1].is_ascii_digit()
                && chars[index + 1].is_ascii_digit();
            if (*ch == ',' || *ch == ' ' || *ch == '\u{a0}') && between_digits {
                continue;
            }
            out.push(*ch);
        }
        out
    }
    let (summary, needle) = (normalize(summary), normalize(needle));
    if summary.contains(&needle) {
        return true;
    }
    // A replay answers with the JSON number the API carries (`1284.5`) while
    // the fixture's ground truth is the page's display string (`1284.50`).
    // Same value, different spelling — compare the number, not the text.
    numeric_answer_matches(&summary, &needle)
}

/// True when `needle` parses as a number and the summary carries a numerically
/// equal token. Non-numeric needles never reach here.
fn numeric_answer_matches(summary: &str, needle: &str) -> bool {
    let Ok(wanted) = needle.parse::<f64>() else {
        return false;
    };
    summary
        .split(|character: char| !(character.is_ascii_digit() || character == '.'))
        .filter(|token| !token.is_empty())
        .filter_map(|token| token.trim_matches('.').parse::<f64>().ok())
        .any(|found| (found - wanted).abs() < f64::EPSILON * wanted.abs().max(1.0))
}

/// Api requests whose nonce was never seen before the phase mean a browser
/// executed page JS during the phase.
pub fn fresh_nonces(requests: &[RequestRecord], seen_before: &HashSet<String>) -> Vec<String> {
    let mut fresh = BTreeSet::new();
    for request in requests {
        if request.kind != RequestKind::Api {
            continue;
        }
        if let Some(nonce) = &request.page_nonce {
            if !seen_before.contains(nonce) {
                fresh.insert(nonce.clone());
            }
        }
    }
    fresh.into_iter().collect()
}

pub fn beacons(requests: &[RequestRecord]) -> Vec<&RequestRecord> {
    requests
        .iter()
        .filter(|request| request.kind == RequestKind::Beacon)
        .collect()
}

/// Beacons outside the heal window: after the first 401 on an `Api` path and
/// before the next 2xx on that same path, a browser-assisted re-login is
/// expected to load pages (and so fire beacons).
pub fn beacons_outside_heal_window(requests: &[RequestRecord]) -> Vec<&RequestRecord> {
    let first_401 = requests
        .iter()
        .position(|request| request.kind == RequestKind::Api && request.status == 401);
    let Some(start) = first_401 else {
        return beacons(requests);
    };
    let healed_path = &requests[start].path;
    let end = requests
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, request)| {
            request.kind == RequestKind::Api
                && &request.path == healed_path
                && (200..300).contains(&request.status)
        })
        .map(|(index, _)| index)
        .unwrap_or(requests.len());
    requests
        .iter()
        .enumerate()
        .filter(|(index, request)| {
            request.kind == RequestKind::Beacon && !(start..=end).contains(index)
        })
        .map(|(_, request)| request)
        .collect()
}

/// Paths of every step in the recipe's current version.
pub fn recipe_paths(recipe: &Value) -> BTreeSet<String> {
    fn path_of(template: &str) -> String {
        let without_scheme = template
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(template);
        let path = without_scheme
            .find('/')
            .map(|index| &without_scheme[index..])
            .unwrap_or("/");
        path.split('?').next().unwrap_or("/").to_owned()
    }
    let mut paths = BTreeSet::new();
    let current = recipe.get("current_version").and_then(Value::as_u64);
    let versions = recipe
        .get("versions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for version in versions {
        if current.is_some() && version.get("version").and_then(Value::as_u64) != current {
            continue;
        }
        for step in version
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(template) = step.get("url_template").and_then(Value::as_str) {
                paths.insert(path_of(template));
            }
        }
    }
    paths
}

/// Requests that a replay could not legitimately have made: any Api or
/// Document path that is not a recipe step. Templated path segments
/// (`{id}`) are matched segment-wise.
pub fn requests_outside_recipe<'a>(
    requests: &'a [RequestRecord],
    paths: &BTreeSet<String>,
) -> Vec<&'a RequestRecord> {
    fn matches(template: &str, actual: &str) -> bool {
        let t: Vec<&str> = template.trim_matches('/').split('/').collect();
        let a: Vec<&str> = actual.trim_matches('/').split('/').collect();
        t.len() == a.len()
            && t.iter().zip(a.iter()).all(|(ts, as_)| {
                ts == as_ || (ts.starts_with('{') && ts.ends_with('}')) || ts.starts_with(':')
            })
    }
    requests
        .iter()
        .filter(|request| matches!(request.kind, RequestKind::Api | RequestKind::Document))
        .filter(|request| {
            !paths
                .iter()
                .any(|template| matches(template, &request.path))
        })
        .collect()
}

pub fn excerpt(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}…")
    }
}

// ---------------------------------------------------------------------------
// Running a phase
// ---------------------------------------------------------------------------

/// Nonces the site has seen so far — the "already replayed" set for the next
/// phase's freshness gate.
pub fn nonces_seen(site: &RunningSite) -> HashSet<String> {
    site.state
        .log
        .all()
        .into_iter()
        .filter_map(|request| request.page_nonce)
        .collect()
}

pub async fn observe(
    ctx: &PhaseCtx<'_>,
    title: &str,
    description: &str,
    on_pending: &mut dyn FnMut(&[Value]) -> Option<(String, String)>,
) -> anyhow::Result<Observed> {
    let started = Instant::now();
    let mark = ctx.site.state.log.mark();
    let log_window = LogWindow::open(ctx.log_path);
    let signals_before = ctx.magician.browser_signals(ctx.magicutor_base).await?;
    let (task_id, execution_id) = ctx
        .magician
        .create_and_execute(title, description, ctx.ui_thread)
        .await?;
    let mut approvals_seen = Vec::new();
    let mut relay = |pending: &[Value]| {
        for request in pending {
            if !approvals_seen
                .iter()
                .any(|seen: &Value| seen.get("id") == request.get("id"))
            {
                approvals_seen.push(request.clone());
            }
        }
        on_pending(pending)
    };
    let outcome = ctx
        .magician
        .wait_terminal(&task_id, &execution_id, ctx.timeout, &mut relay)
        .await;
    let signals_after = ctx.magician.browser_signals(ctx.magicutor_base).await?;
    let mut outcome = outcome?;
    // Detached work (compile, ledger) lands after the terminal state.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let requests = ctx.site.state.log.since(mark);
    let events = evidence::execution_events(
        ctx.runtime_root,
        &ctx.magician.principal,
        &ctx.magician.workspace,
        &task_id,
        &execution_id,
    )?;
    // The task API carries no outcome type or summary; the execution's own
    // terminal observation is canonical.
    if let Some((status, outcome_type, summary)) = evidence::terminal_outcome(&events) {
        outcome.status = status;
        outcome.outcome_type = outcome_type;
        outcome.outcome_summary = summary;
    }
    Ok(Observed {
        task_id,
        execution_id,
        outcome,
        requests,
        events,
        approvals_seen,
        signals_before,
        signals_after,
        log_window,
        started,
    })
}

pub fn common_gates(
    observed: &Observed,
    expect: &Expect<'_>,
    seen_before: &HashSet<String>,
    recipe: Option<&Value>,
) -> Vec<Gate> {
    let mut gates = Vec::new();
    let summary = &observed.outcome.outcome_summary;
    gates.push(Gate::new(
        "task_completed",
        observed.outcome.status == "completed",
        format!("status={}", observed.outcome.status),
    ));
    if !expect.answer_contains.is_empty() {
        let missing: Vec<&str> = expect
            .answer_contains
            .iter()
            .copied()
            .filter(|needle| !answer_matches(summary, needle))
            .collect();
        gates.push(Gate::new(
            "answer_correct",
            missing.is_empty(),
            if missing.is_empty() {
                format!("summary carries {:?}", expect.answer_contains)
            } else {
                format!(
                    "missing {:?} in summary: {}",
                    missing,
                    excerpt(summary, 240)
                )
            },
        ));
    }
    if let Some(wanted) = expect.outcome_type {
        gates.push(Gate::new(
            "outcome_type",
            observed.outcome.outcome_type == wanted,
            format!(
                "outcome_type={} (expected {wanted})",
                observed.outcome.outcome_type
            ),
        ));
    }
    if let Some(unwanted) = expect.not_outcome_type {
        gates.push(Gate::new(
            "outcome_type_not",
            observed.outcome.outcome_type != unwanted,
            format!(
                "outcome_type={} (expected anything but {unwanted})",
                observed.outcome.outcome_type
            ),
        ));
    }
    if expect.browserless {
        let fresh = fresh_nonces(&observed.requests, seen_before);
        gates.push(Gate::new(
            "no_fresh_page_nonce",
            fresh.is_empty(),
            if fresh.is_empty() {
                "no page JS executed during the phase".to_owned()
            } else {
                format!("{} fresh nonce(s): a browser loaded a page", fresh.len())
            },
        ));
        let stray: Vec<String> = if expect.allow_one_heal {
            beacons_outside_heal_window(&observed.requests)
        } else {
            beacons(&observed.requests)
        }
        .iter()
        .map(|request| format!("{} {}", request.method, request.path))
        .collect();
        gates.push(Gate::new(
            "no_beacon",
            stray.is_empty(),
            if stray.is_empty() {
                "no beacon reached the site".to_owned()
            } else {
                format!("beacons: {}", stray.join(", "))
            },
        ));
        gates.push(Gate::new(
            "browser_signals_unchanged",
            observed.signals_before == observed.signals_after,
            format!(
                "tabs {}→{} · browser-only sequences {}→{}",
                observed.signals_before.magicutor_tabs,
                observed.signals_after.magicutor_tabs,
                observed.signals_before.browser_only_sequences,
                observed.signals_after.browser_only_sequences
            ),
        ));
        if let Some(recipe) = recipe {
            let paths = recipe_paths(recipe);
            let outside: Vec<String> = requests_outside_recipe(&observed.requests, &paths)
                .iter()
                .map(|request| format!("{} {}", request.method, request.path))
                .collect();
            gates.push(Gate::new(
                "only_recipe_paths",
                outside.is_empty(),
                if outside.is_empty() {
                    format!(
                        "{} request(s), all within {} step path(s)",
                        observed.requests.len(),
                        paths.len()
                    )
                } else {
                    format!("outside the recipe: {}", outside.join(", "))
                },
            ));
        }
        let started = evidence::has_event(&observed.events, "recipe.replay.started");
        let completed = evidence::has_event(&observed.events, "recipe.replay.completed");
        gates.push(Gate::new(
            "timeline_recipe_replay",
            started && completed,
            format!(
                "started={started} completed={completed} ({} recipe events)",
                evidence::recipe_events(&observed.events).len()
            ),
        ));
    }
    gates
}

pub fn finish_phase(
    ctx: &PhaseCtx<'_>,
    id: &str,
    observed: Observed,
    gates: Vec<Gate>,
    recipe: Option<&Value>,
) -> Phase {
    let failed = gates.iter().any(|gate| !gate.passed);
    let diagnostics = failed.then(|| Diagnostics {
        recipe: recipe.or(ctx.recipe).cloned(),
        recipe_events: evidence::recipe_events(&observed.events),
        log_lines: observed
            .log_window
            .lines_mentioning(&[&observed.task_id, &observed.execution_id, "[API_MINING]"])
            .unwrap_or_default(),
        fixture_requests: observed.requests.clone(),
        approvals: observed.approvals_seen.clone(),
        outcome_details: Some(observed.outcome.details.clone()),
        site_sessions: Some(ctx.site.state.sessions.snapshot()),
    });
    Phase {
        id: id.to_owned(),
        task_id: Some(observed.task_id.clone()),
        execution_id: Some(observed.execution_id.clone()),
        status: observed.outcome.status.clone(),
        outcome_type: observed.outcome.outcome_type.clone(),
        summary_excerpt: excerpt(&observed.outcome.outcome_summary, 240),
        duration_ms: observed.started.elapsed().as_millis() as u64,
        gates,
        diagnostics,
        error: None,
    }
}

pub fn errored_phase(id: &str, error: &anyhow::Error, started: Instant) -> Phase {
    Phase {
        id: id.to_owned(),
        task_id: None,
        execution_id: None,
        status: "error".into(),
        outcome_type: String::new(),
        summary_excerpt: String::new(),
        duration_ms: started.elapsed().as_millis() as u64,
        gates: Vec::new(),
        diagnostics: None,
        error: Some(format!("{error:#}")),
    }
}

/// Run a phase with only the common gates.
pub async fn run_phase(
    ctx: &PhaseCtx<'_>,
    id: &str,
    title: &str,
    description: &str,
    expect: Expect<'_>,
    on_pending: &mut dyn FnMut(&[Value]) -> Option<(String, String)>,
) -> Phase {
    let started = Instant::now();
    let seen_before = nonces_seen(ctx.site);
    match observe(ctx, title, description, on_pending).await {
        Ok(observed) => {
            let gates = common_gates(&observed, &expect, &seen_before, ctx.recipe);
            finish_phase(ctx, id, observed, gates, ctx.recipe)
        },
        Err(error) => errored_phase(id, &error, started),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sites::headers;
    use serde_json::json;

    fn request(kind_path: &str, nonce: Option<&str>, status: u16) -> RequestRecord {
        let mut record = RequestRecord::classify(
            "GET",
            kind_path,
            &headers(&nonce.map(|n| vec![("x-page-nonce", n)]).unwrap_or_default()),
        );
        record.status = status;
        record
    }

    #[test]
    fn answer_gate_accepts_the_same_number_spelled_differently() {
        // A replay answers with the JSON number; ground truth is the display
        // string the page renders.
        assert!(answer_matches("id: ORD-8841 total: 1284.5", "1284.50"));
        assert!(answer_matches("total: 40", "40.00"));
        assert!(!answer_matches("id: ORD-8841 total: 1284.5", "1285.50"));
        // A non-numeric needle still has to appear.
        assert!(!answer_matches("total: 1284.5", "Ingrid Solvang"));
    }

    #[test]
    fn answer_gate_ignores_thousands_separator_and_case() {
        assert!(answer_matches(
            "The total is USD 1,284.50 for ORD-8841.",
            "1284.50"
        ));
        assert!(answer_matches("Points: 3 119", "3119"));
        assert!(answer_matches("by INGRID SOLVANG", "Ingrid Solvang"));
        assert!(!answer_matches("Points: 3119", "2711"));
    }

    #[test]
    fn nonce_gate_flags_fresh_nonce() {
        let seen: HashSet<String> = ["n-cold".to_owned()].into_iter().collect();
        let replayed = vec![request("/api/search", Some("n-cold"), 200)];
        assert!(fresh_nonces(&replayed, &seen).is_empty());
        let browser = vec![request("/api/search", Some("n-warm"), 200)];
        assert_eq!(fresh_nonces(&browser, &seen), vec!["n-warm".to_owned()]);
        let no_nonce = vec![
            request("/api/search", None, 200),
            request("/about", Some("x"), 200),
        ];
        assert!(
            fresh_nonces(&no_nonce, &seen).is_empty(),
            "documents carry no nonce header"
        );
    }

    #[test]
    fn heal_window_tolerates_beacons_only_between_401_and_recovery() {
        let requests = vec![
            request("/px.gif", None, 200),        // stray, before the 401
            request("/api/me/orders", None, 401), // the expiry
            request("/px.gif", None, 200),        // login page load: allowed
            request("/api/session", None, 200),
            request("/px.gif", None, 200), // orders page load: allowed
            request("/api/me/orders", None, 200), // recovered
            request("/px.gif", None, 200), // stray, after recovery
        ];
        let stray = beacons_outside_heal_window(&requests);
        assert_eq!(stray.len(), 2);
        assert_eq!(beacons(&requests).len(), 4);
    }

    #[test]
    fn recipe_paths_come_from_the_current_version_only() {
        let recipe = json!({
            "current_version": 2,
            "versions": [
                {"version": 1, "steps": [{"url_template": "http://127.0.0.1:1/api/old"}]},
                {"version": 2, "steps": [
                    {"url_template": "http://127.0.0.1:1/api/search?q={q}"},
                    {"url_template": "http://127.0.0.1:1/api/items/{id}"}
                ]}
            ]
        });
        let paths = recipe_paths(&recipe);
        assert_eq!(paths.len(), 2);
        assert!(paths.contains("/api/search"));
        assert!(paths.contains("/api/items/{id}"));
        let requests = vec![
            request("/api/search", None, 200),
            request("/api/items/7", None, 200),
            request("/api/old", None, 200),
            request("/px.gif", None, 200),
        ];
        let outside = requests_outside_recipe(&requests, &paths);
        assert_eq!(outside.len(), 1);
        assert_eq!(outside[0].path, "/api/old");
    }

    #[test]
    fn excerpt_flattens_and_caps() {
        assert_eq!(excerpt("a\n  b   c", 10), "a b c");
        assert_eq!(excerpt("abcdefghij", 5), "abcde…");
    }
}
