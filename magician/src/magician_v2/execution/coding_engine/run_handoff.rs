//! Magician's own record of a coding run's turns, handed to an engine that
//! has not seen them.
//!
//! Each engine keeps its own native session, and a session only ever holds
//! the turns that ran in it. When a follow-up switches engine (Pi → Claude),
//! or returns to an engine whose session missed turns another engine ran
//! (Pi → Claude → Pi), the new turn would otherwise see only the files. So
//! every turn is recorded here, engine-independent — what was asked, what the
//! engine answered, what it changed — and a turn that cannot resume a session
//! holding those turns gets them in its prompt, budgeted to the target model's
//! context window: the newest turns verbatim, older ones shortened, the oldest
//! one line each.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Most turns kept per run (the oldest drop first).
const MAX_TURNS: usize = 200;
/// Stored request / answer length per turn.
const STORED_TEXT_CHARS: usize = 8_000;
/// Stored changed-file paths per turn.
const STORED_FILES: usize = 30;
/// Share of the target model's window the handoff may use, and its ceiling.
const WINDOW_SHARE_PERCENT: u64 = 25;
const MAX_HANDOFF_TOKENS: u64 = 40_000;
/// A resumed session at or above this share of the target window starts
/// fresh with a handoff instead (a switch to a smaller model).
pub const RESUME_LIMIT_PERCENT: u64 = 80;
/// Rough characters per token for budgeting (no tokenizer here).
const CHARS_PER_TOKEN: u64 = 4;
/// Window assumed when neither the profile nor the router declares one.
pub const DEFAULT_PI_CONTEXT_WINDOW: u64 = 128_000;
/// Engine CLIs pick their own default model, so its window is not known here.
pub const DEFAULT_ENGINE_CONTEXT_WINDOW: u64 = 200_000;

/// One recorded turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffTurn {
    pub task_id: String,
    pub engine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The engine session the turn ran in, when the engine reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub request: String,
    #[serde(default)]
    pub answer: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// `completed` or `failed`.
    pub outcome: String,
    /// The session's context size after the turn, when the engine reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    pub at_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunHandoff {
    #[serde(default)]
    pub turns: Vec<HandoffTurn>,
}

/// What session the turn about to run resumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeTarget {
    /// A new session: it has seen none of the run.
    Fresh,
    /// This engine session.
    Session(String),
    /// The engine's most recent session for the run (Pi's `--continue`).
    LatestOf(String),
}

/// The handoff for one turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HandoffPlan {
    /// Appended to the engine's prompt; `None` when the session has seen
    /// every earlier turn.
    pub block: Option<String>,
    /// Earlier turns the session has not seen.
    pub unseen: usize,
    pub verbatim: usize,
    pub shortened: usize,
    pub listed: usize,
    pub omitted: usize,
    pub budget_tokens: u64,
    /// Engines the unseen turns ran on, in first-seen order.
    pub from_engines: Vec<String>,
}

fn store_path(scope_root: &Path, root_task_id: &str) -> PathBuf {
    let safe: String = root_task_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    scope_root
        .join("coding_engine")
        .join("run_handoffs")
        .join(format!("{safe}.json"))
}

/// The run's recorded turns (empty when none yet, or unreadable).
pub fn load(scope_root: &Path, root_task_id: &str) -> RunHandoff {
    std::fs::read(store_path(scope_root, root_task_id))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn bounded(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars).collect();
    format!("{cut}…")
}

/// Record a finished turn. A retry of the same task replaces its entry.
pub fn record(scope_root: &Path, root_task_id: &str, mut turn: HandoffTurn) -> std::io::Result<()> {
    turn.request = bounded(&turn.request, STORED_TEXT_CHARS);
    turn.answer = bounded(&turn.answer, STORED_TEXT_CHARS);
    turn.files.truncate(STORED_FILES);
    let mut handoff = load(scope_root, root_task_id);
    handoff
        .turns
        .retain(|existing| existing.task_id != turn.task_id);
    handoff.turns.push(turn);
    let excess = handoff.turns.len().saturating_sub(MAX_TURNS);
    handoff.turns.drain(..excess);
    let path = store_path(scope_root, root_task_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&handoff).map_err(std::io::Error::other)?;
    crate::magician_v2::artifact_v2::io::write_bytes_atomic_sync(&path, &bytes)
        .map_err(std::io::Error::other)
}

impl RunHandoff {
    /// The context size of `engine`'s latest recorded turn.
    pub fn latest_context_tokens(&self, engine: &str) -> Option<u64> {
        self.turns
            .iter()
            .rev()
            .find(|turn| turn.engine == engine)
            .and_then(|turn| turn.context_tokens)
    }
}

/// The context window a handoff is budgeted against.
pub fn context_window(declared: Option<u64>, engine_is_pi: bool) -> u64 {
    declared
        .filter(|tokens| *tokens > 0)
        .unwrap_or(if engine_is_pi {
            DEFAULT_PI_CONTEXT_WINDOW
        } else {
            DEFAULT_ENGINE_CONTEXT_WINDOW
        })
}

/// Whether a session holding `context_tokens` is too large to resume into a
/// model with `window` tokens.
pub fn too_large_to_resume(context_tokens: u64, window: u64) -> bool {
    context_tokens.saturating_mul(100) >= window.saturating_mul(RESUME_LIMIT_PERCENT)
}

/// Plan the handoff for `current_task_id`, about to run with `resume`.
pub fn plan(
    handoff: &RunHandoff,
    current_task_id: Option<&str>,
    resume: &ResumeTarget,
    window_tokens: u64,
) -> HandoffPlan {
    let earlier: Vec<&HandoffTurn> = handoff
        .turns
        .iter()
        .filter(|turn| Some(turn.task_id.as_str()) != current_task_id)
        .collect();
    // The last turn the resumed session ran: everything after it is unseen.
    let seen_until = match resume {
        ResumeTarget::Fresh => None,
        ResumeTarget::Session(id) => earlier
            .iter()
            .rposition(|turn| turn.session_id.as_deref() == Some(id.as_str())),
        ResumeTarget::LatestOf(engine) => earlier.iter().rposition(|turn| &turn.engine == engine),
    };
    let unseen: Vec<&HandoffTurn> = match seen_until {
        Some(index) => earlier[index + 1..].to_vec(),
        // Resuming a session none of the recorded turns ran in: treat as
        // fresh, the safe direction.
        None => earlier.clone(),
    };
    let budget_tokens =
        (window_tokens.saturating_mul(WINDOW_SHARE_PERCENT) / 100).min(MAX_HANDOFF_TOKENS);
    let mut plan = HandoffPlan {
        unseen: unseen.len(),
        budget_tokens,
        ..HandoffPlan::default()
    };
    if unseen.is_empty() {
        return plan;
    }
    for turn in &unseen {
        if !plan.from_engines.contains(&turn.engine) {
            plan.from_engines.push(turn.engine.clone());
        }
    }
    let budget_chars = (budget_tokens * CHARS_PER_TOKEN) as usize;
    let fresh = seen_until.is_none();
    // The run's goal, when the session has never seen the first turn.
    let goal = if fresh {
        earlier.first().map(|first| {
            format!(
                "Run goal (first request): {}\n",
                bounded(&first.request, 2_000)
            )
        })
    } else {
        None
    };
    let mut used = goal.as_ref().map_or(0, String::len);

    // Newest first: verbatim while it fits 70% of the budget, then shortened,
    // then one line each, then omitted.
    let mut rendered: Vec<(usize, String)> = Vec::new();
    let numbered: Vec<(usize, &HandoffTurn)> = unseen
        .iter()
        .enumerate()
        .map(|(index, turn)| (index + 1 + seen_until.map_or(0, |seen| seen + 1), *turn))
        .collect();
    for (number, turn) in numbered.iter().rev() {
        let verbatim = render_turn(*number, turn, 3_000, 4_000, true);
        let shortened = render_turn(*number, turn, 240, 400, false);
        let line = format!(
            "- Turn {number} ({}): {}{}\n",
            turn.engine,
            bounded(&turn.request, 100),
            if turn.outcome == "failed" {
                " — failed"
            } else {
                ""
            }
        );
        if used + verbatim.len() <= budget_chars * 7 / 10 {
            used += verbatim.len();
            plan.verbatim += 1;
            rendered.push((*number, verbatim));
        } else if used + shortened.len() <= budget_chars * 9 / 10 {
            used += shortened.len();
            plan.shortened += 1;
            rendered.push((*number, shortened));
        } else if used + line.len() <= budget_chars {
            used += line.len();
            plan.listed += 1;
            rendered.push((*number, line));
        } else {
            plan.omitted += 1;
        }
    }
    rendered.sort_by_key(|(number, _)| *number);

    let mut block = String::from("## Earlier turns in this run (handed over by Magician)\n");
    block.push_str(if fresh {
        "This engine starts a fresh session, so it has not seen this run's earlier turns. "
    } else {
        "This engine resumes its session, which has not seen the turns below (another engine ran them). "
    });
    block.push_str(&format!(
        "{} earlier turn(s) ran on {}. Treat the current files and git history as the source of truth; this is what was asked and answered.\n\n",
        plan.unseen,
        plan.from_engines.join(", ")
    ));
    if let Some(goal) = goal {
        block.push_str(&goal);
        block.push('\n');
    }
    if plan.omitted > 0 {
        block.push_str(&format!(
            "({} older turn(s) left out to fit the context window.)\n",
            plan.omitted
        ));
    }
    for (_, text) in rendered {
        block.push_str(&text);
    }
    plan.block = Some(block);
    plan
}

fn render_turn(
    number: usize,
    turn: &HandoffTurn,
    request_chars: usize,
    answer_chars: usize,
    with_files: bool,
) -> String {
    let mut out = format!(
        "### Turn {number} — {}{}{}\n",
        turn.engine,
        turn.model
            .as_deref()
            .map(|model| format!(" ({model})"))
            .unwrap_or_default(),
        if turn.outcome == "failed" {
            " — failed"
        } else {
            ""
        }
    );
    out.push_str(&format!(
        "Asked: {}\n",
        bounded(&turn.request, request_chars)
    ));
    if !turn.answer.trim().is_empty() {
        out.push_str(&format!(
            "Answered: {}\n",
            bounded(&turn.answer, answer_chars)
        ));
    }
    if with_files && !turn.files.is_empty() {
        out.push_str(&format!("Changed: {}\n", turn.files.join(", ")));
    }
    if let Some(commit) = turn.commit.as_deref() {
        out.push_str(&format!(
            "Commit: {}\n",
            commit.chars().take(12).collect::<String>()
        ));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(task: &str, engine: &str, session: Option<&str>, request: &str) -> HandoffTurn {
        HandoffTurn {
            task_id: task.into(),
            engine: engine.into(),
            profile_id: None,
            model: None,
            session_id: session.map(str::to_string),
            request: request.into(),
            answer: format!("did {request}"),
            files: vec!["greet.py".into()],
            commit: None,
            outcome: "completed".into(),
            context_tokens: None,
            at_ms: 0,
        }
    }

    fn run(turns: Vec<HandoffTurn>) -> RunHandoff {
        RunHandoff { turns }
    }

    #[test]
    fn same_session_needs_no_handoff() {
        let handoff = run(vec![turn("t1", "claude", Some("s1"), "add wave")]);
        let plan = plan(
            &handoff,
            Some("t2"),
            &ResumeTarget::Session("s1".into()),
            200_000,
        );
        assert_eq!(plan.unseen, 0);
        assert!(plan.block.is_none());
    }

    #[test]
    fn a_switched_engine_gets_every_earlier_turn() {
        let handoff = run(vec![
            turn("t1", "pi", Some("p1"), "add wave"),
            turn("t2", "pi", Some("p1"), "add docstring"),
        ]);
        let plan = plan(&handoff, Some("t3"), &ResumeTarget::Fresh, 200_000);
        assert_eq!((plan.unseen, plan.verbatim), (2, 2));
        let block = plan.block.unwrap();
        assert!(block.contains("starts a fresh session"));
        assert!(block.contains("Run goal (first request): add wave"));
        assert!(block.find("Turn 1").unwrap() < block.find("Turn 2").unwrap());
    }

    #[test]
    fn returning_to_an_engine_hands_over_only_what_it_missed() {
        // Pi → Claude → Pi: Pi's session saw turn 1, not turn 2.
        let handoff = run(vec![
            turn("t1", "pi", Some("p1"), "add wave"),
            turn("t2", "claude", Some("c1"), "rename wave"),
        ]);
        let plan = plan(
            &handoff,
            Some("t3"),
            &ResumeTarget::LatestOf("pi".into()),
            128_000,
        );
        assert_eq!(plan.unseen, 1);
        assert_eq!(plan.from_engines, vec!["claude"]);
        let block = plan.block.unwrap();
        assert!(block.contains("resumes its session"));
        assert!(block.contains("Turn 2"));
        assert!(!block.contains("Turn 1 "));
        assert!(!block.contains("Run goal"));
    }

    #[test]
    fn a_small_window_shortens_then_lists_then_omits_the_oldest() {
        let long = "x".repeat(2_500);
        let turns: Vec<HandoffTurn> = (1..=40)
            .map(|n| turn(&format!("t{n}"), "pi", None, &format!("{n} {long}")))
            .collect();
        let plan = plan(&run(turns), Some("t41"), &ResumeTarget::Fresh, 32_000);
        assert_eq!(plan.unseen, 40);
        assert!(plan.verbatim >= 1 && plan.verbatim < 40, "{plan:?}");
        assert!(plan.shortened + plan.listed + plan.omitted > 0, "{plan:?}");
        let block = plan.block.unwrap();
        assert!(block.len() as u64 <= plan.budget_tokens * CHARS_PER_TOKEN + 2_500);
        // The newest turn is always in full.
        assert!(block.contains("### Turn 40"));
    }

    #[test]
    fn windows_and_the_resume_limit() {
        assert_eq!(context_window(None, true), DEFAULT_PI_CONTEXT_WINDOW);
        assert_eq!(
            context_window(Some(0), false),
            DEFAULT_ENGINE_CONTEXT_WINDOW
        );
        assert_eq!(context_window(Some(1_050_000), true), 1_050_000);
        assert!(too_large_to_resume(110_000, 128_000));
        assert!(!too_large_to_resume(90_000, 128_000));
    }

    #[test]
    fn records_replace_a_retried_task_and_survive_a_reload() {
        let tmp = tempfile::tempdir().unwrap();
        record(tmp.path(), "root", turn("t1", "pi", None, "first")).unwrap();
        record(tmp.path(), "root", turn("t1", "pi", None, "first, retried")).unwrap();
        record(tmp.path(), "root", turn("t2", "claude", None, "second")).unwrap();
        let handoff = load(tmp.path(), "root");
        let requests: Vec<&str> = handoff.turns.iter().map(|t| t.request.as_str()).collect();
        assert_eq!(requests, vec!["first, retried", "second"]);
    }
}
