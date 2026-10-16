//! Phase 2: the same rail against sites we do not control.
//!
//! A fixture can mint a nonce and watch its own request log; a public site
//! cannot. What survives is still enough to prove a replay never opened a
//! browser:
//!
//! * `outcome_type == recipe_replay` on the execution's terminal observation,
//! * `recipe.replay.started` and `.completed` in its timeline,
//! * Magicutor's tab and browser-only sequence counters unmoved,
//! * and no capture trace written for the execution — every browser run
//!   drains one, so its absence is positive evidence none ran.
//!
//! Correctness cannot be a constant either, because these answers change. The
//! harness fetches the same public endpoint itself, over plain HTTP with no
//! browser, and compares. A fetch that fails marks the phase
//! `network_unavailable` and asserts nothing: an unreachable site is not a
//! defect in the rail, and pretending otherwise would make this lane lie.
//!
//! Every case is read-only. Nothing here logs in, and nothing writes.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde_json::Value;

use crate::{
    client::{version_count, BrowserSignals, Magician},
    driver::{self, RECIPE_REPLAY},
    evidence::{self, LogWindow},
    report::{Case, Diagnostics, Gate, Phase},
};

/// Identifies the eval to the sites it reads, so an operator seeing the
/// traffic can tell what it is. Wikipedia asks automated clients for this.
const USER_AGENT: &str =
    "magician-task-recipes-eval/1 (+https://github.com/MagicBeansAI/MagicianAI; read-only eval)";

const RECIPE_WAIT: Duration = Duration::from_secs(120);

pub const ALL_CASES: &[&str] = &["p1", "p2", "p3", "p4"];

/// Cases that read a signed-in account and are therefore never part of a
/// default run. They are selectable only by naming them, because capture writes
/// the account's own data — and a snapshot of its live session cookies — into
/// the run's trace store. Someone running the suite to check the rail should
/// not have their mailbox read as a side effect of that.
pub const OPT_IN_CASES: &[&str] = &["p5", "p6"];

pub fn is_live_case(case: &str) -> bool {
    ALL_CASES.contains(&case) || OPT_IN_CASES.contains(&case)
}

pub struct LiveCtx<'a> {
    pub magician: &'a Magician,
    pub magicutor_base: &'a str,
    pub runtime_root: &'a Path,
    pub log_path: &'a Path,
    pub timeout: Duration,
    pub http: Client,
}

pub fn http_client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .context("building the independent-fetch client")
}

/// What a phase expects of the run it just observed.
struct Expect {
    /// Every needle must appear in the answer; empty accepts any answer.
    answer: Vec<String>,
    outcome_type: Option<&'static str>,
    /// `Some(kind)` requires the outcome NOT to be that kind (drift handoff).
    not_outcome_type: Option<&'static str>,
    /// Counters unmoved, no capture trace, replay started and completed.
    browserless: bool,
}

impl Default for Expect {
    fn default() -> Self {
        Self {
            answer: Vec::new(),
            outcome_type: None,
            not_outcome_type: None,
            browserless: false,
        }
    }
}

struct Observed {
    task_id: String,
    execution_id: String,
    status: String,
    outcome_type: String,
    summary: String,
    events: Vec<Value>,
    signals_before: BrowserSignals,
    signals_after: BrowserSignals,
    capture_traces: usize,
    log_window: LogWindow,
    elapsed: Duration,
}

/// Every browser run drains a capture trace into
/// `api_mining/<execution_id>/`; a replay has no browser and writes none.
fn capture_trace_count(runtime_root: &Path, magician: &Magician, execution_id: &str) -> usize {
    let dir = runtime_root
        .join("scopes")
        .join(&magician.principal)
        .join(&magician.workspace)
        .join("api_mining")
        .join(execution_id);
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("trace_") && name.ends_with(".jsonl"))
                })
                .count()
        })
        .unwrap_or(0)
}

async fn observe(ctx: &LiveCtx<'_>, title: &str, description: &str) -> Result<Observed> {
    let started = Instant::now();
    let log_window = LogWindow::open(ctx.log_path);
    let signals_before = ctx.magician.browser_signals(ctx.magicutor_base).await?;
    let (task_id, execution_id) = ctx
        .magician
        .create_and_execute(title, description, "task-recipes-public-eval")
        .await?;
    // A read-only replay never raises an approval, so this is inert for the
    // public read cases. A write replay does: the origin grant opens the door,
    // and each write step still asks for a per-shape approval. Approving it here
    // is the operator saying yes to that specific replayed write.
    let mut approve = |pending: &[Value]| -> Option<(String, String)> {
        let request = pending.first()?;
        let id = request.get("id").and_then(Value::as_str)?.to_string();
        let option = request
            .get("options")
            .and_then(Value::as_array)
            .and_then(|options| {
                options
                    .iter()
                    .find_map(|o| o.get("id").and_then(Value::as_str))
            })
            .unwrap_or("approve_once")
            .to_string();
        Some((id, option))
    };
    let outcome = ctx
        .magician
        .wait_terminal(&task_id, &execution_id, ctx.timeout, &mut approve)
        .await;
    let signals_after = ctx.magician.browser_signals(ctx.magicutor_base).await?;
    let mut outcome = outcome?;
    // Compile and ledger writes land just after the terminal state.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let events = evidence::execution_events(
        ctx.runtime_root,
        &ctx.magician.principal,
        &ctx.magician.workspace,
        &task_id,
        &execution_id,
    )?;
    if let Some((status, outcome_type, summary)) = evidence::terminal_outcome(&events) {
        outcome.status = status;
        outcome.outcome_type = outcome_type;
        outcome.outcome_summary = summary;
    }
    let capture_traces = capture_trace_count(ctx.runtime_root, ctx.magician, &execution_id);
    Ok(Observed {
        task_id,
        execution_id,
        status: outcome.status,
        outcome_type: outcome.outcome_type,
        summary: outcome.outcome_summary,
        events,
        signals_before,
        signals_after,
        capture_traces,
        log_window,
        elapsed: started.elapsed(),
    })
}

fn gates_for(observed: &Observed, expect: &Expect) -> Vec<Gate> {
    let mut gates = vec![Gate::new(
        "task_completed",
        observed.status == "completed",
        format!("status={}", observed.status),
    )];
    if !expect.answer.is_empty() {
        let missing: Vec<&str> = expect
            .answer
            .iter()
            .filter(|needle| !driver::answer_matches(&observed.summary, needle))
            .map(String::as_str)
            .collect();
        gates.push(Gate::new(
            "answer_matches_live_api",
            missing.is_empty(),
            if missing.is_empty() {
                format!(
                    "summary carries {:?}, as a direct fetch reports them",
                    expect.answer
                )
            } else {
                format!("missing {missing:?} in summary: {}", observed.summary)
            },
        ));
    }
    if let Some(wanted) = expect.outcome_type {
        gates.push(Gate::new(
            "outcome_type",
            observed.outcome_type == wanted,
            format!("outcome_type={} (expected {wanted})", observed.outcome_type),
        ));
    }
    if let Some(unwanted) = expect.not_outcome_type {
        gates.push(Gate::new(
            "outcome_not_replay",
            observed.outcome_type != unwanted,
            format!(
                "outcome_type={} (must not be {unwanted})",
                observed.outcome_type
            ),
        ));
    }
    if expect.browserless {
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
        gates.push(Gate::new(
            "no_capture_trace",
            observed.capture_traces == 0,
            format!(
                "{} capture trace file(s) for this execution",
                observed.capture_traces
            ),
        ));
        let started = evidence::has_event(&observed.events, "recipe.replay.started");
        let completed = evidence::has_event(&observed.events, "recipe.replay.completed");
        gates.push(Gate::new(
            "timeline_recipe_replay",
            started && completed,
            format!("started={started} completed={completed}"),
        ));
    }
    gates
}

fn finish(id: &str, observed: Observed, gates: Vec<Gate>, recipe: Option<&Value>) -> Phase {
    let failed = gates.iter().any(|gate| !gate.passed);
    let diagnostics = failed.then(|| Diagnostics {
        recipe: recipe.cloned(),
        recipe_events: evidence::recipe_events(&observed.events),
        log_lines: observed
            .log_window
            .lines_mentioning(&[&observed.task_id, &observed.execution_id, "[API_MINING]"])
            .unwrap_or_default(),
        fixture_requests: Vec::new(),
        approvals: Vec::new(),
        outcome_details: None,
        site_sessions: None,
    });
    Phase {
        id: id.to_owned(),
        task_id: Some(observed.task_id),
        execution_id: Some(observed.execution_id),
        status: observed.status,
        outcome_type: observed.outcome_type,
        summary_excerpt: observed.summary.chars().take(400).collect(),
        duration_ms: observed.elapsed.as_millis() as u64,
        gates,
        diagnostics,
        error: None,
    }
}

/// The site could not be reached. The phase records why and asserts nothing.
fn unreachable_phase(id: &str, site: &str, error: &anyhow::Error) -> Phase {
    Phase {
        id: id.to_owned(),
        task_id: None,
        execution_id: None,
        status: "network_unavailable".to_owned(),
        outcome_type: String::new(),
        summary_excerpt: String::new(),
        duration_ms: 0,
        gates: Vec::new(),
        diagnostics: None,
        error: Some(format!(
            "{site} was unreachable, so this phase asserted nothing: {error:#}"
        )),
    }
}

fn errored_phase(id: &str, error: &anyhow::Error, started: Instant) -> Phase {
    Phase {
        id: id.to_owned(),
        task_id: None,
        execution_id: None,
        status: "error".to_owned(),
        outcome_type: String::new(),
        summary_excerpt: String::new(),
        duration_ms: started.elapsed().as_millis() as u64,
        gates: vec![Gate::new("phase_ran", false, format!("{error:#}"))],
        diagnostics: None,
        error: Some(format!("{error:#}")),
    }
}

async fn run_phase(
    ctx: &LiveCtx<'_>,
    id: &str,
    title: &str,
    description: &str,
    expect: Expect,
    recipe: Option<&Value>,
) -> Phase {
    run_phase_with(ctx, id, title, description, expect, recipe, |_| Vec::new()).await
}

/// `extra` sees the observation, so a case can add gates that need the
/// timeline without the phase runner knowing what they are.
async fn run_phase_with(
    ctx: &LiveCtx<'_>,
    id: &str,
    title: &str,
    description: &str,
    expect: Expect,
    recipe: Option<&Value>,
    extra: impl FnOnce(&Observed) -> Vec<Gate>,
) -> Phase {
    let started = Instant::now();
    match observe(ctx, title, description).await {
        Ok(observed) => {
            let mut gates = gates_for(&observed, &expect);
            gates.extend(extra(&observed));
            finish(id, observed, gates, recipe)
        },
        Err(error) => errored_phase(id, &error, started),
    }
}

// ---------------------------------------------------------------------------
// Independent fetches — plain HTTP, no browser, one request per phase
// ---------------------------------------------------------------------------

/// The points the Algolia API itself reports for the first story matching
/// `query`. The page the agent reads is the UI over this same endpoint.
pub async fn hn_top_story_points(http: &Client, query: &str) -> Result<String> {
    let url =
        format!("https://hn.algolia.com/api/v1/search?query={query}&tags=story&hitsPerPage=1");
    let body: Value = http
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    body.pointer("/hits/0/points")
        .and_then(Value::as_i64)
        .map(|points| points.to_string())
        .context("Algolia returned no story with points")
}

/// The elevation Open-Meteo reports for a point, exactly as its JSON spells
/// it. The same response carries the current temperature, but a temperature
/// moves between the cold run and the replay and is rendered rounded, so a
/// mismatch would say nothing about the rail. Elevation is stable, sits in
/// the same response, and still proves the recipe reached this origin.
/// The title of the top story, used where a case needs a task shape distinct
/// from the points one so it compiles a recipe of its own.
pub async fn hn_top_story_title(http: &Client, query: &str) -> Result<String> {
    let url =
        format!("https://hn.algolia.com/api/v1/search?query={query}&tags=story&hitsPerPage=1");
    let body: Value = http
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    body.pointer("/hits/0/title")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("Algolia returned no story with a title")
}

pub async fn open_meteo_elevation(http: &Client, latitude: f64, longitude: f64) -> Result<String> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}&current=temperature_2m"
    );
    let body: Value = http
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    body.get("elevation")
        .and_then(Value::as_number)
        // The JSON spelling, so `38.0` is not compared as `38`.
        .map(|elevation| elevation.to_string())
        .context("no elevation in the response")
}

/// The year the live article's infobox gives as the subject's first
/// appearance.
///
/// Two things defeat a plain text search here. The label is split across an
/// entity span, so it is not contiguous in the markup and cannot be found
/// until the markup is flattened; and the body of an article carries many
/// other years, so the answer has to be read out of that one row rather than
/// out of the document. An earlier version of this eval searched the whole
/// document for a hardcoded year, which passed on an unrelated mention and
/// graded the correct answer as wrong.
pub async fn wikipedia_first_appeared_year(http: &Client, page: &str) -> Result<String> {
    let url = format!("https://en.wikipedia.org/api/rest_v1/page/html/{page}");
    let html = http
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    first_appeared_year(&html)
        .with_context(|| format!("no first-appeared row in the {page} infobox"))
}

/// Split from the fetch so the parse is covered without the network.
fn first_appeared_year(html: &str) -> Option<String> {
    let text = flatten_markup(html);
    let row = text.find("First appeared")?;
    let tail: String = text[row..].chars().take(120).collect();
    year_in(&tail)
}

/// Markup flattened to its text: tags dropped, non-breaking and ordinary
/// whitespace folded together, runs collapsed. Enough to read a label and the
/// value beside it without taking on an HTML parser.
fn flatten_markup(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut last_was_space = false;
    for ch in html.chars() {
        let out = match ch {
            '<' => {
                in_tag = true;
                continue;
            },
            // A tag boundary separates words, so it reads as whitespace.
            '>' if in_tag => {
                in_tag = false;
                ' '
            },
            _ if in_tag => continue,
            c if c.is_whitespace() => ' ',
            c => c,
        };
        if out == ' ' {
            if !last_was_space {
                text.push(' ');
            }
            last_was_space = true;
        } else {
            text.push(out);
            last_was_space = false;
        }
    }
    text
}

/// The first standalone four-digit year in `text`. Digits are taken in runs so
/// a longer number never contributes a four-digit window.
fn year_in(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let digits = &text[start..index];
        if digits.len() == 4 && matches!(digits.parse::<u16>(), Ok(1800..=2200)) {
            return Some(digits.to_owned());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

fn new_case(id: &str, site: &str, origin: &str) -> Case {
    Case {
        id: id.to_owned(),
        site: site.to_owned(),
        origin: origin.to_owned(),
        passed: false,
        recipe_id: None,
        phases: Vec::new(),
        error: None,
    }
}

/// The fixture preamble names a fixture; these sites are real, and the
/// caveats that matter here are different: read the page now (the values move
/// between runs) and never log in anywhere.
const LIVE_PREAMBLE: &str = "API-mining evaluation against a public site. Read the page with the browser TOOL (browser__open and friends) — not web_search, not web_fetch, and not the shell: do not run curl, wget, or a browser binary yourself, because a page fetched outside the browser tool is invisible to this evaluation. These pages change between runs, so read the page now; never answer from memory of an earlier run. Never log in, never submit a form, and never modify anything on the site — every task here is read-only. This runs unattended with no human to answer questions, so never pause for user input. Report the answer as a plain value in your final summary. ";

fn describe(body: &str) -> String {
    format!("{LIVE_PREAMBLE}{body}")
}

/// Wait for the cold run's recipe and record whether it compiled.
async fn bind_recipe(ctx: &LiveCtx<'_>, case: &mut Case, task_id: &str) -> Option<Value> {
    match ctx
        .magician
        .wait_for_recipe_bound_to(task_id, RECIPE_WAIT)
        .await
    {
        Ok((recipe_id, recipe)) => {
            case.recipe_id = Some(recipe_id);
            if let Some(cold) = case.phases.last_mut() {
                cold.gates.push(Gate::new(
                    "recipe_compiled",
                    true,
                    format!("versions={}", version_count(&recipe)),
                ));
            }
            Some(recipe)
        },
        Err(error) => {
            if let Some(cold) = case.phases.last_mut() {
                cold.gates
                    .push(Gate::new("recipe_compiled", false, format!("{error:#}")));
            }
            None
        },
    }
}

const GMAIL_ORIGIN: &str = "https://mail.google.com";

/// p5 — a signed-in, identity-bearing origin. Opt-in only.
///
/// Two things are under test, and the second is the one with a bad answer.
/// Whether mining can learn a signed-in RPC surface at all is the interesting
/// question. Whether replay on such an origin stays shut until an operator
/// opens it is the important one: a recipe that replays a mailbox unattended,
/// against an origin nobody audited, is a worse outcome than no recipe. So the
/// case proves refusal first and only then grants and proves the capability.
///
/// The mailbox cannot be fetched independently, so unlike every other live case
/// there is no outside oracle. The claim under test is narrower and still worth
/// asserting: the browserless replay returns what the browser returned.
async fn gmail_case(ctx: &LiveCtx<'_>) -> Case {
    let mut case = new_case("p5", "gmail", GMAIL_ORIGIN);
    let title = "Subject of the newest message in the Gmail inbox";
    let description = describe(
        "Open https://mail.google.com/mail/u/0/#inbox with the browser tool, passing \
         connection_mode \"cdp\" so it attaches to the already signed-in Chrome. Do not attempt \
         to sign in, and do not use any other connection mode. Read the inbox list and find the \
         subject line of the newest message. Your final summary must be that subject line and \
         nothing else: do not describe how you read it, which tool or connection mode you used, \
         or what steps you took. A summary that narrates its own method states something the \
         evidence check cannot confirm, and the answer is refused along with it. This is \
         strictly read-only: do not open, reply to, forward, archive, delete, label, mark or \
         otherwise change anything in the mailbox.",
    );
    case.phases
        .push(run_phase(ctx, "cold", title, &description, Expect::default(), None).await);
    let Some(task_id) = case.phases[0].task_id.clone() else {
        return case;
    };
    let Some(recipe) = bind_recipe(ctx, &mut case, &task_id).await else {
        return case;
    };

    // Ungranted: the origin defaults to replay-refused, and must still be.
    case.phases.push(
        run_phase(
            ctx,
            "warm_ungranted",
            title,
            &description,
            Expect {
                not_outcome_type: Some(RECIPE_REPLAY),
                ..Expect::default()
            },
            Some(&recipe),
        )
        .await,
    );

    // The operator step this case exists to make visible.
    if let Err(error) = ctx.magician.allow_origin_replay(GMAIL_ORIGIN).await {
        case.error = Some(format!(
            "could not grant replay for {GMAIL_ORIGIN}: {error:#}"
        ));
        return case;
    }

    // Compare against what the browser itself reported, and only when that
    // answer is short enough to be the bare subject the task asked for; a long
    // summary means the agent narrated instead, and asserting on it would be
    // asserting on prose.
    let reported = case.phases[0].summary_excerpt.trim().to_owned();
    let answer = if reported.is_empty() || reported.chars().count() > 120 {
        Vec::new()
    } else {
        vec![reported]
    };
    case.phases.push(
        run_phase(
            ctx,
            "warm_granted",
            title,
            &description,
            Expect {
                answer,
                outcome_type: Some(RECIPE_REPLAY),
                browserless: true,
                ..Expect::default()
            },
            Some(&recipe),
        )
        .await,
    );
    case
}

/// The operator's real Keka tenant, e.g. `https://<company>.keka.com`. Set
/// `MAGICIAN_KEKA_ORIGIN` for live runs; the default is a placeholder.
fn keka_origin() -> String {
    std::env::var("MAGICIAN_KEKA_ORIGIN")
        .unwrap_or_else(|_| "https://example.keka.com".to_string())
        .trim_end_matches('/')
        .to_string()
}

/// The write cases permit exactly one named state change and forbid everything
/// else, unlike the read preamble which forbids all mutation. Kept separate so
/// a read case can never accidentally inherit permission to write.
fn describe_write(body: &str) -> String {
    format!(
        "API-mining evaluation against a signed-in site, using an account the operator has \
         authorised. Read the page with the browser TOOL (browser__open and friends) with \
         connection_mode \"cdp\" so it attaches to the already signed-in Chrome — not \
         web_search, not web_fetch, and not the shell. Never sign in; the session is already \
         open. Perform ONLY the single state change named below and nothing else: do not \
         repeat it, do not touch any other control, do not navigate elsewhere, and do not \
         read, open, reply to, or change anything else on the site. This runs unattended with \
         no human to answer questions, so never pause for user input. {body}"
    )
}

/// p6 — a signed-in origin AND a write: clock out of Keka web attendance.
/// Opt-in only, because it acts on a real HR system of record.
///
/// A write replay is not a read replay with the verb changed. Two things it
/// exercises that no read case does. The origin is identity-bearing and the
/// step mutates, so replay is gated twice — the origin must be opened AND each
/// write shape cleared — where a read needs neither. And the action is a
/// stateful toggle: the cold run clocks out, so by the time the recipe replays
/// the same shape the account is already clocked out. Whether Keka accepts a
/// second clock-out or rejects it is Keka's business rule, not the rail's; what
/// the rail must show either way is that it fired Keka's authenticated write
/// with the captured bearer and no browser. The phase asserts that browserless
/// shape; a Keka rejection surfaces as a non-replay outcome and is read from
/// the log rather than pre-judged here.
async fn keka_case(ctx: &LiveCtx<'_>) -> Case {
    let keka_origin = keka_origin();
    let mut case = new_case("p6", "keka", &keka_origin);
    let title = "Clock out of Keka web attendance";
    let description = describe_write(&format!(
        "On {keka_origin}/#/home/dashboard there is a single attendance clock \
         button. If it reads \"Web Clock-out\" or \"Clock-out\", click it exactly once to clock \
         out, then stop. If it already reads \"Web Clock-In\", the account is already clocked \
         out — do not click, and report that. After acting, report only the resulting button \
         label, nothing else.",
    ));
    case.phases
        .push(run_phase(ctx, "cold", title, &description, Expect::default(), None).await);
    let Some(task_id) = case.phases[0].task_id.clone() else {
        return case;
    };
    let Some(recipe) = bind_recipe(ctx, &mut case, &task_id).await else {
        return case;
    };
    // The operator opens this identity-bearing write origin for replay. Every
    // origin is replay-refused until this call; a signed-in write origin is
    // exactly the kind that must not open on its own.
    if let Err(error) = ctx.magician.allow_origin_replay(&keka_origin).await {
        case.error = Some(format!(
            "could not grant replay for {keka_origin}: {error:#}"
        ));
        return case;
    }
    // The same clock-out shape, replayed with no browser. The write-shape
    // approval this raises is answered inside observe().
    case.phases.push(
        run_phase(
            ctx,
            "warm_granted",
            title,
            &description,
            Expect {
                outcome_type: Some(RECIPE_REPLAY),
                browserless: true,
                ..Expect::default()
            },
            Some(&recipe),
        )
        .await,
    );
    case
}

pub async fn run_case(id: &str, ctx: &LiveCtx<'_>) -> Case {
    match id {
        "p1" => hn_case(ctx).await,
        "p2" => open_meteo_case(ctx).await,
        "p3" => wikipedia_case(ctx).await,
        "p4" => drift_case(ctx).await,
        "p5" => gmail_case(ctx).await,
        "p6" => keka_case(ctx).await,
        other => {
            let mut case = new_case(other, "", "");
            case.error = Some(format!("unknown live case `{other}`"));
            case
        },
    }
}

const HN_TITLE: &str = "Points of the top Hacker News story about {q}";
/// The drift case needs a recipe it can safely mutate, so it must compile one
/// of its own. Asking for the same field as the points case no longer does
/// that: once the search term became a slot, one recipe legitimately covers
/// every query of that shape, and the drift case was served by the points
/// case's recipe instead of compiling anything. Forcing drift on a shared
/// recipe would corrupt the case that owns it.
const HN_TITLE_FIELD_TASK: &str = "Title of the top Hacker News story about {q}";
const HN_BODY_FIELD_TASK: &str =
    "Open https://hn.algolia.com/?q={q} and report the title of the first story result. \
     Report the title only.";
const HN_BODY: &str =
    "Open https://hn.algolia.com/?q={q} and report the points of the first story result.";

/// p1 — a real UI whose XHR hits a documented public JSON API.
async fn hn_case(ctx: &LiveCtx<'_>) -> Case {
    let mut case = new_case("p1", "hn.algolia", "https://hn.algolia.com");
    let cold_points = match hn_top_story_points(&ctx.http, "rust").await {
        Ok(points) => points,
        Err(error) => {
            case.phases
                .push(unreachable_phase("cold", "hn.algolia.com", &error));
            return case;
        },
    };
    let title = HN_TITLE.replace("{q}", "rust");
    let description = describe(&HN_BODY.replace("{q}", "rust"));
    case.phases.push(
        run_phase(
            ctx,
            "cold",
            &title,
            &description,
            Expect {
                answer: vec![cold_points],
                ..Expect::default()
            },
            None,
        )
        .await,
    );
    let Some(task_id) = case.phases[0].task_id.clone() else {
        return case;
    };
    let Some(recipe) = bind_recipe(ctx, &mut case, &task_id).await else {
        return case;
    };

    // The same task again: the rail must answer it over HTTP, and the answer
    // must still agree with what the API reports right now.
    match hn_top_story_points(&ctx.http, "rust").await {
        Ok(points) => case.phases.push(
            run_phase(
                ctx,
                "warm",
                &title,
                &description,
                Expect {
                    answer: vec![points],
                    outcome_type: Some(RECIPE_REPLAY),
                    browserless: true,
                    ..Expect::default()
                },
                Some(&recipe),
            )
            .await,
        ),
        Err(error) => case
            .phases
            .push(unreachable_phase("warm", "hn.algolia.com", &error)),
    }

    // A different query travels the same shape with a new input.
    match hn_top_story_points(&ctx.http, "golang").await {
        Ok(points) => {
            let title = HN_TITLE.replace("{q}", "golang");
            let description = describe(&HN_BODY.replace("{q}", "golang"));
            case.phases.push(
                run_phase(
                    ctx,
                    "variant",
                    &title,
                    &description,
                    Expect {
                        answer: vec![points],
                        outcome_type: Some(RECIPE_REPLAY),
                        browserless: true,
                        ..Expect::default()
                    },
                    Some(&recipe),
                )
                .await,
            )
        },
        Err(error) => case
            .phases
            .push(unreachable_phase("variant", "hn.algolia.com", &error)),
    }
    case
}

/// Berlin, to match the city Open-Meteo's own documentation uses.
const BERLIN: (f64, f64) = (52.52, 13.41);

/// p2 — a second origin, so per-origin recipes and policy are exercised.
async fn open_meteo_case(ctx: &LiveCtx<'_>) -> Case {
    let mut case = new_case("p2", "open-meteo", "https://open-meteo.com");
    let expected = match open_meteo_elevation(&ctx.http, BERLIN.0, BERLIN.1).await {
        Ok(elevation) => elevation,
        Err(error) => {
            case.phases
                .push(unreachable_phase("cold", "api.open-meteo.com", &error));
            return case;
        },
    };
    let title = "Elevation Open-Meteo reports for Berlin";
    // Point at the JSON the service actually serves, not at its docs page.
    // The docs page never dependably renders this value: a live run scraped a
    // plausible but wrong number from unrelated page content, and the
    // grounded-completion verifier rightly refused to let the agent claim it.
    // A task whose answer is not visibly on the page measures the verifier,
    // not the mining rail.
    let description = describe(
        "Open https://api.open-meteo.com/v1/forecast?latitude=52.52&longitude=13.41\
         &current=temperature_2m and report the value of the `elevation` field in the JSON the \
         page shows. Report the number only.",
    );
    case.phases.push(
        run_phase(
            ctx,
            "cold",
            title,
            &description,
            Expect {
                answer: vec![expected],
                ..Expect::default()
            },
            None,
        )
        .await,
    );
    let Some(task_id) = case.phases[0].task_id.clone() else {
        return case;
    };
    let Some(recipe) = bind_recipe(ctx, &mut case, &task_id).await else {
        return case;
    };
    match open_meteo_elevation(&ctx.http, BERLIN.0, BERLIN.1).await {
        Ok(elevation) => case.phases.push(
            run_phase(
                ctx,
                "warm",
                title,
                &description,
                Expect {
                    answer: vec![elevation],
                    outcome_type: Some(RECIPE_REPLAY),
                    browserless: true,
                    ..Expect::default()
                },
                Some(&recipe),
            )
            .await,
        ),
        Err(error) => case
            .phases
            .push(unreachable_phase("warm", "api.open-meteo.com", &error)),
    }
    case
}

const RUST_PAGE: &str = "Rust_(programming_language)";

/// p3 — a server-rendered article: the answer lives in the document, not in
/// an XHR, which is the shape a JSON-only miner cannot serve.
async fn wikipedia_case(ctx: &LiveCtx<'_>) -> Case {
    let mut case = new_case("p3", "wikipedia", "https://en.wikipedia.org");
    // Read the expected year off the live infobox rather than holding one as a
    // constant. The article's prose and its infobox disagree about which year
    // counts as the first appearance, so a constant graded the row the task
    // actually names as wrong.
    let year = match wikipedia_first_appeared_year(&ctx.http, RUST_PAGE).await {
        Ok(year) => year,
        Err(error) => {
            case.phases
                .push(unreachable_phase("cold", "en.wikipedia.org", &error));
            return case;
        },
    };
    let title = "Year the Rust programming language first appeared";
    let description = describe(
        "Open https://en.wikipedia.org/wiki/Rust_(programming_language) and report the year the \
         infobox gives on its first-appeared row. Report the year only.",
    );
    case.phases.push(
        run_phase(
            ctx,
            "cold",
            title,
            &description,
            Expect {
                answer: vec![year.clone()],
                ..Expect::default()
            },
            None,
        )
        .await,
    );
    // Mining cannot serve this shape today, and the reason is deliberate rather
    // than accidental. The answer is a fragment of prose inside one cell of a
    // large rendered document, and locating a value in text requires the body
    // to be small, the value to be an element's WHOLE text, and that element's
    // tag to occur exactly once in the document — guards that exist so a replay
    // can never silently extract a different element. A value-anchored
    // extractor would pass this case and make recipes that quietly return the
    // wrong thing, which is a worse failure than not compiling.
    //
    // So the case asserts what the product does do — read a server-rendered
    // document and answer correctly — and records whether a recipe appeared as
    // a diagnostic rather than a gate. Asserting the limitation must persist
    // would turn a future improvement into a red suite.
    let Some(task_id) = case.phases[0].task_id.clone() else {
        return case;
    };
    let bound = ctx
        .magician
        .wait_for_recipe_bound_to(&task_id, Duration::from_secs(20))
        .await
        .is_ok();
    if let Some(cold) = case.phases.last_mut() {
        cold.gates.push(Gate::new(
            "document_answer_read_without_mining",
            true,
            format!(
                "recipe_bound={bound}; a value embedded in prose inside a large \
                 server-rendered document is not minable today — the text locator \
                 caps the body size, requires the value to be an element's whole \
                 text, and requires that tag to occur once in the document"
            ),
        ));
    }
    case
}

/// Point the recipe's answer extractor at a path no response carries, the way
/// a site's schema change would.
fn force_drift(
    runtime_root: &Path,
    magician: &Magician,
    recipe_id: &str,
) -> Result<(PathBuf, Vec<u8>)> {
    let path = runtime_root
        .join("scopes")
        .join(&magician.principal)
        .join(&magician.workspace)
        .join("api_mining")
        .join("recipes")
        .join(format!("{recipe_id}.json"));
    let original = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let mut recipe: Value = serde_json::from_slice(&original)?;
    let current = recipe
        .get("current_version")
        .and_then(Value::as_u64)
        .context("recipe omitted current_version")?;
    let version = recipe
        .get_mut("versions")
        .and_then(Value::as_array_mut)
        .context("recipe omitted versions")?
        .iter_mut()
        .find(|version| version.get("version").and_then(Value::as_u64) == Some(current))
        .context("current recipe version not found")?;
    let extractor = version
        .get_mut("answer_spec")
        .and_then(Value::as_array_mut)
        .and_then(|answers| answers.first_mut())
        .and_then(|answer| answer.get_mut("extractor"))
        .and_then(Value::as_object_mut)
        .context("recipe has no answer extractor to drift")?;
    if !extractor.contains_key("path") {
        bail!("the drift case needs a JSON-path answer extractor");
    }
    extractor.insert(
        "path".into(),
        Value::String("$.__forced_task_recipe_drift__".into()),
    );
    let temporary = path.with_extension("json.drift-tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&recipe)?)?;
    fs::rename(&temporary, &path)?;
    Ok((path, original))
}

/// p4 — a recipe whose extractor stops matching must hand back to the browser,
/// recompile, and replay browserlessly again on the new version.
async fn drift_case(ctx: &LiveCtx<'_>) -> Case {
    let mut case = new_case("p4", "hn.algolia", "https://hn.algolia.com");
    let query = "wasm";
    let story_title = match hn_top_story_title(&ctx.http, query).await {
        Ok(story_title) => story_title,
        Err(error) => {
            case.phases
                .push(unreachable_phase("cold", "hn.algolia.com", &error));
            return case;
        },
    };
    let title = HN_TITLE_FIELD_TASK.replace("{q}", query);
    let description = describe(&HN_BODY_FIELD_TASK.replace("{q}", query));
    case.phases.push(
        run_phase(
            ctx,
            "cold",
            &title,
            &description,
            Expect {
                answer: vec![story_title],
                ..Expect::default()
            },
            None,
        )
        .await,
    );
    let Some(task_id) = case.phases[0].task_id.clone() else {
        return case;
    };
    let Some(recipe) = bind_recipe(ctx, &mut case, &task_id).await else {
        return case;
    };
    let Some(recipe_id) = case.recipe_id.clone() else {
        return case;
    };

    let before_versions = version_count(&recipe);
    if let Err(error) = force_drift(ctx.runtime_root, ctx.magician, &recipe_id) {
        case.error = Some(format!("could not force drift: {error:#}"));
        return case;
    }

    // The extractor no longer matches, so the rail must notice and hand the
    // task back to the browser rather than answer from a broken recipe.
    match hn_top_story_title(&ctx.http, query).await {
        Ok(story_title) => {
            let phase = run_phase_with(
                ctx,
                "drift",
                &title,
                &description,
                Expect {
                    answer: vec![story_title],
                    not_outcome_type: Some(RECIPE_REPLAY),
                    ..Expect::default()
                },
                Some(&recipe),
                |observed| {
                    let failed = evidence::has_event(&observed.events, "recipe.replay.step.failed");
                    let handed =
                        evidence::has_event(&observed.events, "recipe.replay.fallback.handoff");
                    vec![Gate::new(
                        "drift_recorded",
                        failed || handed,
                        format!("step.failed={failed} fallback.handoff={handed}"),
                    )]
                },
            )
            .await;
            case.phases.push(phase);
        },
        Err(error) => {
            case.phases
                .push(unreachable_phase("drift", "hn.algolia.com", &error));
            return case;
        },
    }

    let healed = match ctx
        .magician
        .wait_for_recipe_version(&recipe_id, before_versions, RECIPE_WAIT)
        .await
    {
        Ok(detail) => {
            if let Some(drift) = case.phases.last_mut() {
                drift.gates.push(Gate::new(
                    "recompiled_new_version",
                    true,
                    format!("versions {before_versions} → {}", version_count(&detail)),
                ));
            }
            detail
        },
        Err(error) => {
            if let Some(drift) = case.phases.last_mut() {
                drift.gates.push(Gate::new(
                    "recompiled_new_version",
                    false,
                    format!("{error:#}"),
                ));
            }
            return case;
        },
    };

    match hn_top_story_title(&ctx.http, query).await {
        Ok(story_title) => case.phases.push(
            run_phase(
                ctx,
                "healed",
                &title,
                &description,
                Expect {
                    answer: vec![story_title],
                    outcome_type: Some(RECIPE_REPLAY),
                    browserless: true,
                    ..Expect::default()
                },
                Some(&healed),
            )
            .await,
        ),
        Err(error) => case
            .phases
            .push(unreachable_phase("healed", "hn.algolia.com", &error)),
    }
    case
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet_signals() -> BrowserSignals {
        BrowserSignals {
            browser_only_sequences: 3,
            router: serde_json::json!({}),
            magicutor_tabs: 2,
        }
    }

    #[test]
    fn live_cases_are_named_and_recognised() {
        for case in ALL_CASES {
            assert!(is_live_case(case), "{case}");
        }
        assert!(!is_live_case("c1"));
        assert_eq!(ALL_CASES.len(), 4);
    }

    /// Markup shaped like the live article: an unrelated year in the prose
    /// ahead of an infobox whose label is split by an entity span.
    fn infobox_fragment() -> String {
        [
            "<p>The first stable release was published in May 2015.</p>",
            "<table class=\"infobox\"><tr><th class=\"infobox-label\">Developer</th>",
            "<td class=\"infobox-data\">The Team</td></tr>",
            "<tr><th class=\"infobox-label\">First<span typeof=\"mw:Entity\">\u{a0}</span>",
            "appeared</th><td class=\"infobox-data\">January",
            "<span typeof=\"mw:Entity\">\u{a0}</span>20, 2012</td></tr></table>",
        ]
        .concat()
    }

    #[test]
    fn the_first_appeared_year_comes_from_the_row_not_from_the_article() {
        // The prose year appears first in the document, so a search over the
        // whole document returns it — which is the bug this replaced.
        let html = infobox_fragment();
        assert!(html.find("2015").unwrap() < html.find("2012").unwrap());
        assert_eq!(first_appeared_year(&html).as_deref(), Some("2012"));
    }

    #[test]
    fn a_label_split_by_an_entity_span_is_still_found() {
        // Contiguous in the rendered text, never in the markup.
        let html = infobox_fragment();
        assert!(!html.contains("First appeared"));
        assert!(flatten_markup(&html).contains("First appeared"));
    }

    #[test]
    fn a_document_without_the_row_yields_no_year() {
        assert_eq!(first_appeared_year("<p>Published in May 2015.</p>"), None);
    }

    #[test]
    fn a_longer_number_never_supplies_a_year() {
        assert_eq!(year_in("build 1234567 of 1999"), Some("1999".to_owned()));
        assert_eq!(year_in("id 88888888"), None);
        assert_eq!(year_in("20, 2012"), Some("2012".to_owned()));
    }

    #[test]
    fn an_unreachable_site_asserts_nothing() {
        let phase = unreachable_phase("warm", "example.test", &anyhow::anyhow!("dns"));
        assert!(
            phase.gates.is_empty(),
            "a network failure must not be a verdict"
        );
        assert_eq!(phase.status, "network_unavailable");
        assert!(phase.error.unwrap().contains("example.test"));
    }

    #[test]
    fn browserless_gates_read_counters_traces_and_the_timeline() {
        let observed = Observed {
            task_id: "t".into(),
            execution_id: "e".into(),
            status: "completed".into(),
            outcome_type: RECIPE_REPLAY.to_owned(),
            summary: "points: 412".into(),
            events: vec![
                serde_json::json!({"event_type":"recipe.replay","payload":{"kind":"recipe.replay.started"}}),
                serde_json::json!({"event_type":"recipe.replay","payload":{"kind":"recipe.replay.completed"}}),
            ],
            signals_before: quiet_signals(),
            signals_after: quiet_signals(),
            capture_traces: 0,
            log_window: LogWindow::open(Path::new("/nonexistent")),
            elapsed: Duration::from_secs(9),
        };
        let expect = Expect {
            answer: vec!["412".to_owned()],
            outcome_type: Some(RECIPE_REPLAY),
            browserless: true,
            ..Expect::default()
        };
        let gates = gates_for(&observed, &expect);
        assert!(gates.iter().all(|gate| gate.passed), "{gates:?}");
        // A capture trace means a browser ran, however good the answer looked.
        let with_trace = Observed {
            capture_traces: 1,
            ..observed
        };
        let gates = gates_for(&with_trace, &expect);
        assert!(gates
            .iter()
            .any(|gate| gate.id == "no_capture_trace" && !gate.passed));
    }
}
