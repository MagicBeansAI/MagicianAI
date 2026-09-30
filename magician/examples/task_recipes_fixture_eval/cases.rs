//! The six cases. Each runs cold (LLM + browser) then the phases that must
//! be browserless, adding case-specific gates on top of the common ones.

use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::{
    client::{version_count, Magician},
    driver::{self, Expect, PhaseCtx, RECIPE_REPLAY},
    evidence,
    report::{Case, Gate, Phase},
    sites::{self, board, catalog, notes, portal, RequestKind, RunningSite},
};

pub const ALL_CASES: &[&str] = &["c1", "c2", "c3", "c4", "c5", "c6"];

pub fn site_for(case: &str) -> Option<&'static str> {
    match case {
        "c1" | "c2" | "c3" => Some("catalog"),
        "c4" => Some("portal"),
        "c5" => Some("notes"),
        "c6" => Some("board"),
        _ => None,
    }
}

pub struct CaseCtx<'a> {
    pub magician: &'a Magician,
    pub magicutor_base: &'a str,
    pub sites: &'a HashMap<&'static str, RunningSite>,
    pub runtime_root: &'a Path,
    pub log_path: &'a Path,
    pub timeout: Duration,
}

const RECIPE_WAIT: Duration = Duration::from_secs(90);

fn none(_: &[Value]) -> Option<(String, String)> {
    None
}

fn phase_ctx<'a>(
    ctx: &'a CaseCtx<'a>,
    site: &'a RunningSite,
    case: &str,
    recipe: Option<&'a Value>,
) -> PhaseCtx<'a> {
    PhaseCtx {
        magician: ctx.magician,
        magicutor_base: ctx.magicutor_base,
        site,
        runtime_root: ctx.runtime_root,
        log_path: ctx.log_path,
        timeout: ctx.timeout,
        ui_thread: Box::leak(format!("task-recipes-fixture-{case}").into_boxed_str()),
        recipe,
    }
}

fn describe(origin: &str, rest: &str) -> String {
    format!("{}{}", driver::PREAMBLE, rest.replace("{origin}", origin))
}

/// Fixture secrets a recipe must never carry as literals.
fn fixture_secrets(site: &RunningSite) -> Vec<String> {
    let snapshot = site.state.sessions.snapshot();
    snapshot["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|session| {
            ["id", "csrf"]
                .into_iter()
                .filter_map(|key| session.get(key).and_then(Value::as_str).map(str::to_owned))
        })
        .collect()
}

fn recipe_steps(recipe: &Value) -> Vec<Value> {
    let current = recipe.get("current_version").and_then(Value::as_u64);
    recipe
        .get("versions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|version| {
            current.is_none() || version.get("version").and_then(Value::as_u64) == current
        })
        .flat_map(|version| {
            version
                .get("steps")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

fn recipe_gates(recipe: &Value, site: &RunningSite) -> Vec<Gate> {
    let steps = recipe_steps(recipe);
    let mut gates = vec![Gate::new(
        "recipe_compiled",
        !steps.is_empty(),
        format!("versions={} steps={}", version_count(recipe), steps.len()),
    )];
    let foreign: Vec<String> = steps
        .iter()
        .filter_map(|step| step.get("origin").and_then(Value::as_str))
        .filter(|origin| origin.trim_end_matches('/') != site.origin.trim_end_matches('/'))
        .map(str::to_owned)
        .collect();
    gates.push(Gate::new(
        "recipe_steps_on_fixture_origin",
        foreign.is_empty(),
        if foreign.is_empty() {
            format!("all steps on {}", site.origin)
        } else {
            format!("foreign origins: {}", foreign.join(", "))
        },
    ));
    let beacon_steps: Vec<String> = steps
        .iter()
        .filter_map(|step| step.get("url_template").and_then(Value::as_str))
        .filter(|template| {
            sites::BEACON_PATHS
                .iter()
                .any(|path| template.contains(path))
        })
        .map(str::to_owned)
        .collect();
    gates.push(Gate::new(
        "recipe_has_no_beacon_step",
        beacon_steps.is_empty(),
        if beacon_steps.is_empty() {
            "no tracking path became a step".to_owned()
        } else {
            format!("beacon steps: {}", beacon_steps.join(", "))
        },
    ));
    let text = recipe.to_string();
    let leaked: Vec<String> = fixture_secrets(site)
        .into_iter()
        .filter(|secret| secret.len() >= 8 && text.contains(secret))
        .map(|secret| format!("{}…", &secret[..6]))
        .collect();
    gates.push(Gate::new(
        "recipe_has_no_secret_literal",
        leaked.is_empty(),
        if leaked.is_empty() {
            "no session id or CSRF token in the recipe".to_owned()
        } else {
            format!("recipe carries fixture secrets: {}", leaked.join(", "))
        },
    ));
    gates
}

/// Cold phase: LLM + browser, then wait for the bound recipe and grade it.
async fn cold(
    ctx: &CaseCtx<'_>,
    site: &RunningSite,
    case: &str,
    title: &str,
    description: &str,
    answer: &[&str],
) -> (Phase, Option<(String, Value)>) {
    let pctx = phase_ctx(ctx, site, case, None);
    let started = Instant::now();
    let seen_before = driver::nonces_seen(site);
    let observed = match driver::observe(&pctx, title, description, &mut none).await {
        Ok(observed) => observed,
        Err(error) => return (driver::errored_phase("cold", &error, started), None),
    };
    let expect = Expect {
        answer_contains: answer,
        ..Expect::default()
    };
    let mut gates = driver::common_gates(&observed, &expect, &seen_before, None);
    let mut recipe = None;
    if observed.outcome.status == "completed" {
        match ctx
            .magician
            .wait_for_recipe_bound_to(&observed.task_id, RECIPE_WAIT)
            .await
        {
            Ok((id, detail)) => {
                gates.extend(recipe_gates(&detail, site));
                recipe = Some((id, detail));
            },
            Err(error) => gates.push(Gate::new("recipe_compiled", false, format!("{error:#}"))),
        }
    } else {
        gates.push(Gate::new(
            "recipe_compiled",
            false,
            "cold run did not complete",
        ));
    }
    let phase = driver::finish_phase(
        &pctx,
        "cold",
        observed,
        gates,
        recipe.as_ref().map(|(_, r)| r),
    );
    (phase, recipe)
}

async fn browserless(
    ctx: &CaseCtx<'_>,
    site: &RunningSite,
    case: &str,
    recipe: &Value,
    phase_id: &str,
    title: &str,
    description: &str,
    answer: &[&str],
) -> Phase {
    let pctx = phase_ctx(ctx, site, case, Some(recipe));
    driver::run_phase(
        &pctx,
        phase_id,
        title,
        description,
        Expect {
            answer_contains: answer,
            outcome_type: Some(RECIPE_REPLAY),
            browserless: true,
            ..Expect::default()
        },
        &mut none,
    )
    .await
}

fn new_case(id: &str, site: &RunningSite) -> Case {
    Case {
        id: id.to_owned(),
        site: site.name.to_owned(),
        origin: site.origin.clone(),
        passed: false,
        recipe_id: None,
        phases: Vec::new(),
        error: None,
    }
}

pub async fn run_case(id: &str, ctx: &CaseCtx<'_>) -> Case {
    let Some(site_name) = site_for(id) else {
        return Case {
            id: id.to_owned(),
            site: String::new(),
            origin: String::new(),
            passed: false,
            recipe_id: None,
            phases: Vec::new(),
            error: Some(format!("unknown case `{id}`")),
        };
    };
    let site = &ctx.sites[site_name];
    let mut case = new_case(id, site);
    match id {
        "c1" => {
            read_case(
                ctx,
                site,
                &mut case,
                ReadSpec {
                    title: "Points of the top result for {q}",
                    description: "Open {origin}/?q={q} and report the points of the first result.",
                    cold_input: "rust",
                    variant_input: Some("python"),
                    answer: |input| {
                        vec![catalog::top_hit(input)
                            .map(|item| item.points.to_string())
                            .unwrap_or_default()]
                    },
                },
            )
            .await
        },
        "c2" => {
            read_case(
                ctx,
                site,
                &mut case,
                ReadSpec {
                    title: "Author of the top result for {q}",
                    description:
                        "Open {origin}/?q={q}, open the first result, and report its author.",
                    cold_input: "rust",
                    variant_input: Some("go"),
                    answer: |input| {
                        vec![catalog::top_hit(input)
                            .map(|item| item.author.to_owned())
                            .unwrap_or_default()]
                    },
                },
            )
            .await
        },
        "c3" => {
            read_case(
                ctx,
                site,
                &mut case,
                ReadSpec {
                    title: "Founding year of the catalog",
                    description: "Open {origin}/about and report the year the catalog was founded.",
                    cold_input: "",
                    variant_input: None,
                    answer: |_| vec![catalog::FOUNDED_YEAR.to_string()],
                },
            )
            .await
        },
        "c4" => session_case(ctx, site, &mut case).await,
        "c5" => write_case(ctx, site, &mut case).await,
        "c6" => drift_case(ctx, site, &mut case).await,
        _ => unreachable!("site_for gates the id"),
    }
    case.finish()
}

struct ReadSpec {
    title: &'static str,
    description: &'static str,
    cold_input: &'static str,
    variant_input: Option<&'static str>,
    answer: fn(&str) -> Vec<String>,
}

async fn read_case(ctx: &CaseCtx<'_>, site: &RunningSite, case: &mut Case, spec: ReadSpec) {
    let title = spec.title.replace("{q}", spec.cold_input);
    let description = describe(
        &site.origin,
        &spec.description.replace("{q}", spec.cold_input),
    );
    let answer = (spec.answer)(spec.cold_input);
    let answer_refs: Vec<&str> = answer.iter().map(String::as_str).collect();
    let (phase, recipe) = cold(ctx, site, &case.id, &title, &description, &answer_refs).await;
    case.phases.push(phase);
    let Some((recipe_id, recipe)) = recipe else {
        return;
    };
    case.recipe_id = Some(recipe_id);
    case.phases.push(
        browserless(
            ctx,
            site,
            &case.id,
            &recipe,
            "warm",
            &title,
            &description,
            &answer_refs,
        )
        .await,
    );
    if let Some(variant) = spec.variant_input {
        let title = spec.title.replace("{q}", variant);
        let description = describe(&site.origin, &spec.description.replace("{q}", variant));
        let answer = (spec.answer)(variant);
        let answer_refs: Vec<&str> = answer.iter().map(String::as_str).collect();
        case.phases.push(
            browserless(
                ctx,
                site,
                &case.id,
                &recipe,
                "variant",
                &title,
                &description,
                &answer_refs,
            )
            .await,
        );
    }
}

// ---------------------------------------------------------------------------
// C4 — session-gated read + auth heal
// ---------------------------------------------------------------------------

const SESSION_TITLE: &str = "Total of my most recent order";
const SESSION_DESCRIPTION: &str = "Open {origin}/login, log in with username eval and password eval-pass, then open the orders page and report the total of the most recent order.";

async fn session_case(ctx: &CaseCtx<'_>, site: &RunningSite, case: &mut Case) {
    let description = describe(&site.origin, SESSION_DESCRIPTION);
    let answer = [portal::MOST_RECENT_TOTAL];
    let (phase, recipe) = cold(ctx, site, &case.id, SESSION_TITLE, &description, &answer).await;
    case.phases.push(phase);
    let Some((recipe_id, recipe)) = recipe else {
        return;
    };
    case.recipe_id = Some(recipe_id);
    case.phases.push(
        browserless(
            ctx,
            site,
            &case.id,
            &recipe,
            "warm",
            SESSION_TITLE,
            &description,
            &answer,
        )
        .await,
    );

    // Expire every session, then lift the expiry once the first 401 has been
    // served so the heal's re-login can succeed.
    site.state.knobs.lock().unwrap().session_ttl_secs = Some(0);
    let mark = site.state.log.mark();
    let watcher_state = std::sync::Arc::clone(&site.state);
    let watcher = tokio::spawn(async move {
        loop {
            let saw_401 = watcher_state
                .log
                .since(mark)
                .iter()
                .any(|request| request.kind == RequestKind::Api && request.status == 401);
            if saw_401 {
                watcher_state.knobs.lock().unwrap().session_ttl_secs = None;
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
    let pctx = phase_ctx(ctx, site, &case.id, Some(&recipe));
    let started = Instant::now();
    let seen_before = driver::nonces_seen(site);
    // The expired run recovers the answer, but NOT browserlessly. The design
    // asked for "one browser-assisted heal, then the answer is still replayed";
    // the product instead abandons the replay and hands the whole task to the
    // agent. That is not a gate bug to paper over: `refresh_origin_auth` heals
    // by re-reading an ALREADY signed-in profile through Magicutor, and a
    // session the agent established itself with credentials from the task text
    // exists in no such profile — the recipe deliberately stores no
    // credentials, so nothing but the agent can log in again. What the rail
    // does deliver is recovery: the fallback re-logs in, the drain captures the
    // fresh cookies, and the NEXT run is browserless again. This phase holds
    // the product to that — a correct answer and a recorded auth failure — and
    // `recovered` below proves the session came back.
    let heal_phase = match driver::observe(&pctx, SESSION_TITLE, &description, &mut none).await {
        Ok(observed) => {
            let expect = Expect {
                answer_contains: &answer,
                ..Expect::default()
            };
            let mut gates = driver::common_gates(&observed, &expect, &seen_before, Some(&recipe));
            let unauthorized = observed
                .requests
                .iter()
                .filter(|request| request.kind == RequestKind::Api && request.status == 401)
                .count();
            gates.push(Gate::new(
                "expiry_served",
                unauthorized >= 1,
                format!("{unauthorized} × 401 from the fixture"),
            ));
            let healed = evidence::count_event(&observed.events, "recipe.replay.auth.healed");
            let handed_off =
                evidence::has_event(&observed.events, "recipe.replay.fallback.handoff");
            gates.push(Gate::new(
                "auth_failure_is_recorded",
                healed == 1 || handed_off,
                format!("auth.healed × {healed}; fallback.handoff={handed_off}"),
            ));
            driver::finish_phase(&pctx, "heal", observed, gates, Some(&recipe))
        },
        Err(error) => driver::errored_phase("heal", &error, started),
    };
    watcher.abort();
    site.state.knobs.lock().unwrap().session_ttl_secs = None;
    case.phases.push(heal_phase);

    // Sessions are live again and the fallback re-login was captured, so the
    // rail must be back to answering this task without a browser.
    case.phases.push(
        browserless(
            ctx,
            site,
            &case.id,
            &recipe,
            "recovered",
            SESSION_TITLE,
            &description,
            &answer,
        )
        .await,
    );
}

// ---------------------------------------------------------------------------
// C5 — guarded write + HITL
// ---------------------------------------------------------------------------

const WRITE_TITLE: &str = "Add the note {text}";
const WRITE_DESCRIPTION: &str = "Open {origin}/login, log in with username eval and password eval-pass, open the notes page, add a note that says \"{text}\", and confirm it appears in the list.";

fn notes_contain(site: &RunningSite, text: &str) -> bool {
    site.state.data.lock().unwrap()["notes"]
        .as_array()
        .is_some_and(|notes| notes.iter().any(|note| note["text"].as_str() == Some(text)))
}

fn reset_notes(site: &RunningSite) {
    *site.state.data.lock().unwrap() = notes::initial_data();
}

fn approval_option(pending: &[Value], option: &str) -> Option<(String, String)> {
    pending.first().and_then(|request| {
        request
            .get("id")
            .and_then(Value::as_str)
            .map(|id| (id.to_owned(), option.to_owned()))
    })
}

fn preview_gate(approvals: &[Value], site: &RunningSite, text: &str) -> Gate {
    let Some(approval) = approvals.first() else {
        return Gate::new(
            "preview_has_shape_not_values",
            false,
            "no approval was requested",
        );
    };
    let json = approval.to_string();
    let has_method = json.contains("POST");
    let has_origin = json.contains(site.origin.trim_end_matches('/'));
    // `approval_url_shape` deliberately reduces the path to its depth, because
    // segments carry account ids and task data. Either telling is fine; saying
    // nothing about the path is not.
    let has_path_shape = json.contains("/api/notes") || json.contains("path segment");
    let has_body_key = json.contains("text");
    let leaks_value = json.contains(text);
    Gate::new(
        "preview_has_shape_not_values",
        has_method && has_origin && has_path_shape && has_body_key && !leaks_value,
        format!("method={has_method} origin={has_origin} path_shape={has_path_shape} body_key={has_body_key} leaks_value={leaks_value}"),
    )
}

/// A durable grant for this recipe's write step. Grants are keyed by
/// `{recipe_id, step_id, request_shape_fingerprint}` and carry no path or
/// method, so the write step's identity is what proves the grant is the one
/// `approve_always_step` created.
fn grant_for_write_step(grants: &Value, recipe_id: &str, recipe: &Value) -> (bool, String) {
    let write_step_ids: Vec<String> = current_version(recipe)
        .and_then(|version| version.get("steps").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter(|step| step.get("side_effects").and_then(Value::as_str) == Some("write"))
        .filter_map(|step| step.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let rows = grants
        .as_array()
        .cloned()
        .or_else(|| grants.get("grants").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    let matched = rows.iter().any(|grant| {
        let key = grant.get("key").unwrap_or(grant);
        let same_recipe = key.get("recipe_id").and_then(Value::as_str) == Some(recipe_id);
        let step = key
            .get("step_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let live = grant
            .get("revoked_at_ms")
            .map(Value::is_null)
            .unwrap_or(true);
        same_recipe && write_step_ids.iter().any(|id| id == step) && live
    });
    (
        matched,
        format!("{} grant(s); write steps {write_step_ids:?}", rows.len()),
    )
}

fn current_version(recipe: &Value) -> Option<Value> {
    let current = recipe.get("current_version").and_then(Value::as_u64);
    recipe
        .get("versions")
        .and_then(Value::as_array)?
        .iter()
        .find(|version| {
            current.is_none() || version.get("version").and_then(Value::as_u64) == current
        })
        .cloned()
}

async fn write_phase(
    ctx: &CaseCtx<'_>,
    site: &RunningSite,
    case: &str,
    recipe: &Value,
    phase_id: &str,
    text: &str,
    respond_with: Option<&str>,
    extra: impl FnOnce(&driver::Observed) -> Vec<Gate>,
) -> Phase {
    let title = WRITE_TITLE.replace("{text}", text);
    let description = describe(&site.origin, &WRITE_DESCRIPTION.replace("{text}", text));
    let pctx = phase_ctx(ctx, site, case, Some(recipe));
    let started = Instant::now();
    let seen_before = driver::nonces_seen(site);
    let mut answered = false;
    let mut on_pending = |pending: &[Value]| -> Option<(String, String)> {
        let option = respond_with?;
        if answered {
            return None;
        }
        let answer = approval_option(pending, option);
        answered = answer.is_some();
        answer
    };
    match driver::observe(&pctx, &title, &description, &mut on_pending).await {
        Ok(observed) => {
            let expect = Expect {
                answer_contains: &[text],
                outcome_type: Some(RECIPE_REPLAY),
                browserless: true,
                ..Expect::default()
            };
            let mut gates = driver::common_gates(&observed, &expect, &seen_before, Some(recipe));
            let posts = observed
                .requests
                .iter()
                .filter(|request| {
                    request.kind == RequestKind::Api
                        && request.method == "POST"
                        && request.path == "/api/notes"
                })
                .count();
            gates.push(Gate::new(
                "exactly_one_post",
                posts == 1,
                format!("POST /api/notes × {posts}"),
            ));
            gates.push(Gate::new(
                "note_persisted",
                notes_contain(site, text),
                format!(
                    "fixture list contains \"{text}\": {}",
                    notes_contain(site, text)
                ),
            ));
            gates.extend(extra(&observed));
            driver::finish_phase(&pctx, phase_id, observed, gates, Some(recipe))
        },
        Err(error) => driver::errored_phase(phase_id, &error, started),
    }
}

async fn write_case(ctx: &CaseCtx<'_>, site: &RunningSite, case: &mut Case) {
    reset_notes(site);
    let cold_text = "buy milk";
    let title = WRITE_TITLE.replace("{text}", cold_text);
    let description = describe(
        &site.origin,
        &WRITE_DESCRIPTION.replace("{text}", cold_text),
    );
    let (mut phase, recipe) = cold(ctx, site, &case.id, &title, &description, &[cold_text]).await;
    phase.gates.push(Gate::new(
        "note_persisted",
        notes_contain(site, cold_text),
        format!(
            "fixture list contains \"{cold_text}\": {}",
            notes_contain(site, cold_text)
        ),
    ));
    case.phases.push(phase);
    let Some((recipe_id, recipe)) = recipe else {
        return;
    };
    case.recipe_id = Some(recipe_id.clone());

    let warm = write_phase(
        ctx,
        site,
        &case.id,
        &recipe,
        "warm_approve_once",
        "call mom",
        Some("approve_once"),
        |observed| {
            vec![
                Gate::new(
                    "approval_requested",
                    !observed.approvals_seen.is_empty()
                        || evidence::has_event(
                            &observed.events,
                            "recipe.replay.approval.requested",
                        ),
                    format!(
                        "{} approval(s) seen; event={}",
                        observed.approvals_seen.len(),
                        evidence::has_event(&observed.events, "recipe.replay.approval.requested")
                    ),
                ),
                preview_gate(&observed.approvals_seen, site, "call mom"),
                Gate::new(
                    "always_option_offered",
                    observed
                        .approvals_seen
                        .first()
                        .map(|approval| approval.to_string().contains("approve_always_step"))
                        .unwrap_or(false),
                    "non-denylisted step offers approve_always_step",
                ),
            ]
        },
    )
    .await;
    case.phases.push(warm);

    let variant = write_phase(
        ctx,
        site,
        &case.id,
        &recipe,
        "variant_approve_always",
        "pay rent",
        Some("approve_always_step"),
        |observed| {
            vec![Gate::new(
                "approval_requested",
                !observed.approvals_seen.is_empty(),
                format!("{} approval(s) seen", observed.approvals_seen.len()),
            )]
        },
    )
    .await;
    case.phases.push(variant);

    let grants = ctx.magician.replay_grants().await.unwrap_or(Value::Null);
    let (grant_for_write, grant_detail) = grant_for_write_step(&grants, &recipe_id, &recipe);

    let granted = write_phase(
        ctx,
        site,
        &case.id,
        &recipe,
        "granted_no_prompt",
        "walk the dog",
        None,
        move |observed| {
            // Grants carry no path, so the denylisted write is proved unreachable
            // where it would actually show: the fixture never served it.
            let delete_attempts = observed
                .requests
                .iter()
                .filter(|request| request.path == notes::DENYLISTED_PATH)
                .count();
            vec![
                Gate::new(
                    "no_prompt",
                    observed.approvals_seen.is_empty(),
                    format!("{} approval(s) seen", observed.approvals_seen.len()),
                ),
                Gate::new("grant_created", grant_for_write, grant_detail.clone()),
                Gate::new(
                    "account_delete_never_replayed",
                    delete_attempts == 0,
                    format!("{delete_attempts} request(s) to {}", notes::DENYLISTED_PATH),
                ),
            ]
        },
    )
    .await;
    case.phases.push(granted);
    reset_notes(site);
}

// ---------------------------------------------------------------------------
// C6 — GraphQL + drift
// ---------------------------------------------------------------------------

const DRIFT_TITLE: &str = "Score of board alpha";
const DRIFT_DESCRIPTION: &str = "Open {origin}/board/alpha and report the board's score.";

async fn drift_case(ctx: &CaseCtx<'_>, site: &RunningSite, case: &mut Case) {
    site.state.knobs.lock().unwrap().schema_version = 1;
    let description = describe(&site.origin, DRIFT_DESCRIPTION);
    let score = board::board("alpha")
        .map(|b| b.score.to_string())
        .unwrap_or_default();
    let answer = [score.as_str()];
    let (phase, recipe) = cold(ctx, site, &case.id, DRIFT_TITLE, &description, &answer).await;
    case.phases.push(phase);
    let Some((recipe_id, recipe)) = recipe else {
        return;
    };
    case.recipe_id = Some(recipe_id.clone());
    case.phases.push(
        browserless(
            ctx,
            site,
            &case.id,
            &recipe,
            "warm",
            DRIFT_TITLE,
            &description,
            &answer,
        )
        .await,
    );

    site.state.knobs.lock().unwrap().schema_version = 2;
    let before_versions = version_count(&recipe);
    let pctx = phase_ctx(ctx, site, &case.id, Some(&recipe));
    let started = Instant::now();
    let seen_before = driver::nonces_seen(site);
    let mut healed_recipe = None;
    let drift_phase = match driver::observe(&pctx, DRIFT_TITLE, &description, &mut none).await {
        Ok(observed) => {
            let expect = Expect {
                answer_contains: &answer,
                not_outcome_type: Some(RECIPE_REPLAY),
                ..Expect::default()
            };
            let mut gates = driver::common_gates(&observed, &expect, &seen_before, Some(&recipe));
            let drift_recorded = evidence::has_event(&observed.events, "recipe.replay.step.failed")
                || evidence::has_event(&observed.events, "recipe.replay.fallback.handoff");
            gates.push(Gate::new(
                "drift_recorded",
                drift_recorded,
                format!(
                    "step.failed={} fallback.handoff={}",
                    evidence::has_event(&observed.events, "recipe.replay.step.failed"),
                    evidence::has_event(&observed.events, "recipe.replay.fallback.handoff")
                ),
            ));
            match ctx
                .magician
                .wait_for_recipe_version(&recipe_id, before_versions, RECIPE_WAIT)
                .await
            {
                Ok(detail) => {
                    gates.push(Gate::new(
                        "recompiled_new_version",
                        true,
                        format!("versions {before_versions} → {}", version_count(&detail)),
                    ));
                    healed_recipe = Some(detail);
                },
                Err(error) => gates.push(Gate::new(
                    "recompiled_new_version",
                    false,
                    format!("{error:#}"),
                )),
            }
            driver::finish_phase(&pctx, "drift", observed, gates, Some(&recipe))
        },
        Err(error) => driver::errored_phase("drift", &error, started),
    };
    case.phases.push(drift_phase);
    if let Some(healed) = healed_recipe {
        case.phases.push(
            browserless(
                ctx,
                site,
                &case.id,
                &healed,
                "healed",
                DRIFT_TITLE,
                &description,
                &answer,
            )
            .await,
        );
    }
    site.state.knobs.lock().unwrap().schema_version = 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn case_descriptions_embed_site_origin_and_preamble() {
        let text = describe(
            "http://127.0.0.1:4242",
            "Open {origin}/?q=rust and report the points.",
        );
        assert!(text.starts_with(driver::PREAMBLE));
        assert!(text.contains("http://127.0.0.1:4242/?q=rust"));
        assert!(!text.contains("{origin}"));
    }

    #[test]
    fn every_case_maps_to_a_site() {
        for case in ALL_CASES {
            assert!(site_for(case).is_some(), "{case}");
        }
        assert!(site_for("c99").is_none());
    }

    #[test]
    fn recipe_gates_reject_foreign_origins_beacons_and_secrets() {
        let site = RunningSite::for_tests("notes", "http://127.0.0.1:5000", json!({}));
        let session = site.state.sessions.create("eval");
        let recipe = json!({
            "current_version": 1,
            "versions": [{"version": 1, "steps": [
                {"origin": "http://127.0.0.1:5000", "url_template": "http://127.0.0.1:5000/api/notes", "headers_template": {"cookie": format!("sid={}", session.id)}},
                {"origin": "http://127.0.0.1:5000", "url_template": "http://127.0.0.1:5000/px.gif?e=view"},
                {"origin": "https://tracker.example", "url_template": "https://tracker.example/collect"}
            ]}]
        });
        let gates = recipe_gates(&recipe, &site);
        let by_id: HashMap<_, _> = gates
            .iter()
            .map(|gate| (gate.id.as_str(), gate.passed))
            .collect();
        assert_eq!(by_id["recipe_compiled"], true);
        assert_eq!(by_id["recipe_steps_on_fixture_origin"], false);
        assert_eq!(by_id["recipe_has_no_beacon_step"], false);
        assert_eq!(by_id["recipe_has_no_secret_literal"], false);
    }

    #[test]
    fn preview_gate_wants_shape_without_the_value() {
        let site = RunningSite::for_tests("notes", "http://127.0.0.1:5000", json!({}));
        let good = json!([{"id": "r1", "context": {"method": "POST", "origin": "http://127.0.0.1:5000", "url_template": "http://127.0.0.1:5000/api/notes", "body_keys": ["text"]}}]);
        assert!(preview_gate(good.as_array().unwrap(), &site, "call mom").passed);
        let leaky = json!([{"id": "r1", "context": {"method": "POST", "origin": "http://127.0.0.1:5000", "url_template": "http://127.0.0.1:5000/api/notes", "body": {"text": "call mom"}}}]);
        assert!(!preview_gate(leaky.as_array().unwrap(), &site, "call mom").passed);
        assert!(!preview_gate(&[], &site, "call mom").passed);
        // The shipped preview reduces the path to its depth on purpose.
        let redacted = json!([{"id": "r1", "context": {"method": "POST", "origin": "http://127.0.0.1:5000", "url_template": "http://127.0.0.1:5000/<2 path segments>", "request_preview": {"body_shape": ["key:$/text"]}}}]);
        assert!(preview_gate(redacted.as_array().unwrap(), &site, "call mom").passed);
        // Saying nothing at all about the path is still a failure.
        let silent = json!([{"id": "r1", "context": {"method": "POST", "origin": "http://127.0.0.1:5000", "request_preview": {"body_shape": ["key:$/text"]}}}]);
        assert!(!preview_gate(silent.as_array().unwrap(), &site, "call mom").passed);
    }

    #[test]
    fn a_grant_is_recognised_by_its_recipe_and_write_step() {
        let recipe = json!({"current_version": 1, "versions": [{"version": 1, "steps": [
            {"id": "s0", "side_effects": "write", "url_template": "http://127.0.0.1:5000/api/notes"},
            {"id": "s1", "side_effects": "read_only", "url_template": "http://127.0.0.1:5000/api/notes"}
        ]}]});
        let granted = json!([{"id": "g1", "key": {"recipe_id": "rcp_1", "step_id": "s0"}, "revoked_at_ms": null}]);
        assert!(grant_for_write_step(&granted, "rcp_1", &recipe).0);
        // The read step, another recipe, or a revoked grant are not it.
        let read_only = json!([{"id": "g1", "key": {"recipe_id": "rcp_1", "step_id": "s1"}}]);
        assert!(!grant_for_write_step(&read_only, "rcp_1", &recipe).0);
        assert!(!grant_for_write_step(&granted, "rcp_other", &recipe).0);
        let revoked = json!([{"id": "g1", "key": {"recipe_id": "rcp_1", "step_id": "s0"}, "revoked_at_ms": 12}]);
        assert!(!grant_for_write_step(&revoked, "rcp_1", &recipe).0);
    }
}
