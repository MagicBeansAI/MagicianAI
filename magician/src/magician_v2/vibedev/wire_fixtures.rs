//! The VibeDev wire fixtures the mobile apps decode.
//!
//! `magician/tests/fixtures/vibedev/*.json` are read by `MagiosTests`, the
//! magdroid bridge tests and the web tests; these tests keep each file equal
//! to what the server really serializes, from the same types and folds the
//! endpoints use. When a shape changes on purpose, regenerate with
//! `UPDATE_VIBEDEV_FIXTURES=1` and update the clients in the same change.

use std::path::PathBuf;

use serde_json::{json, Value};

use super::crews::{CrewLane, CrewMode, CrewStatus, LaneRole, LaneStatus, VibeDevCrew};
use super::live_runs::{LiveRun, LiveRunNotice, LiveRunState, WaitingKind};
use super::run_detail::{
    fold_timeline, ChangedFile, FilePatch, RepoSuggestion, RunChanges, RunTurn, RunTurns,
    TurnTimeline,
};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vibedev")
}

/// Compare `value` with the named fixture, or write it when regenerating.
pub(crate) fn check_fixture(name: &str, value: &Value) {
    let path = fixture_dir().join(name);
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(value).expect("fixture json")
    );
    if std::env::var_os("UPDATE_VIBEDEV_FIXTURES").is_some() {
        std::fs::create_dir_all(fixture_dir()).expect("fixture dir");
        std::fs::write(&path, rendered).expect("write fixture");
        return;
    }
    let stored = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: {error} (regenerate with UPDATE_VIBEDEV_FIXTURES=1)",
            path.display()
        )
    });
    let stored: Value = serde_json::from_str(&stored).expect("stored fixture parses");
    assert_eq!(
        &stored, value,
        "{name} no longer matches what the server sends; regenerate with UPDATE_VIBEDEV_FIXTURES=1 and update the mobile clients"
    );
}

fn live_run(id: &str, title: &str, state: LiveRunState, lane: Option<&str>) -> LiveRun {
    LiveRun {
        run_id: id.to_string(),
        live_task_id: id.to_string(),
        project_id: "proj_shop".to_string(),
        project_name: "Shop".to_string(),
        title: title.to_string(),
        latest_turn_title: None,
        state,
        status: match state {
            LiveRunState::Done => "completed",
            LiveRunState::Failed => "failed",
            _ => "running",
        }
        .to_string(),
        step: (state == LiveRunState::Working).then(|| "Editing src/api/auth/codes.ts".to_string()),
        summary: (state == LiveRunState::Done)
            .then(|| "Search uses a trigram index: 900 ms to 38 ms.".to_string()),
        completion_kind: None,
        open_items: Vec::new(),
        turns: 1,
        started_at: "2026-10-13T09:12:00Z".to_string(),
        cost_usd: (state == LiveRunState::Done).then_some(0.88),
        approval_id: (state == LiveRunState::NeedsYou)
            .then(|| "ccp-6f1d2a90-3b7c-4e55-9a10-2f4c8d7e1b30".to_string()),
        approval_respond: (state == LiveRunState::NeedsYou)
            .then(|| {
                super::live_runs::diff_review_respond_handle(
                    "ccp-6f1d2a90-3b7c-4e55-9a10-2f4c8d7e1b30",
                )
            })
            .flatten(),
        approval_task_id: None,
        waiting_kind: (state == LiveRunState::NeedsYou).then_some(WaitingKind::DiffReview),
        notice: None,
        crew_id: lane.map(|_| "crew_7f3a".to_string()),
        lane: lane.map(str::to_string),
        updated_at: "2026-10-13T09:41:00Z".to_string(),
        last_progress_at: Some("2026-10-13T09:40:30Z".to_string()),
    }
}

#[test]
fn live_runs_fixture() {
    let runs = vec![
        live_run(
            "task_ask",
            "Add dark-mode screenshots",
            LiveRunState::NeedsYou,
            None,
        ),
        live_run(
            "task_api",
            "API · Login with email codes",
            LiveRunState::Working,
            Some("API"),
        ),
        live_run(
            "task_search",
            "Speed up product search",
            LiveRunState::Done,
            None,
        ),
        // An Autopilot run that ended and filed its owner briefing: done, with
        // a notice to acknowledge, never needs-you.
        LiveRun {
            summary: Some("Bumped the Pi engine to 1.3.0.".to_string()),
            cost_usd: Some(0.42),
            waiting_kind: Some(WaitingKind::Notice),
            notice: Some(LiveRunNotice {
                request_id: "vibedev-run-ended:task_report:exec_d278".to_string(),
                option_id: "acknowledge".to_string(),
                title: Some("Autopilot run finished: Pi 1.3.0 live test".to_string()),
                message:
                    "Autopilot run finished: Pi 1.3.0 live test. Open it to review the result."
                        .to_string(),
                severity: Some("info".to_string()),
                respond: super::live_runs::notice_respond_handle(
                    "vibedev-run-ended:task_report:exec_d278",
                    "acknowledge",
                    "Acknowledge",
                    Some("exec_d278"),
                ),
                created_at_ms: 0,
            }),
            ..live_run(
                "task_report",
                "Pi 1.3.0 live test",
                LiveRunState::Done,
                None,
            )
        },
    ];
    let counts = super::live_runs::state_counts(&runs);
    let counts: std::collections::BTreeMap<_, _> = counts.into_iter().collect();
    check_fixture("live_runs.json", &json!({ "runs": runs, "counts": counts }));
}

#[test]
fn run_turns_fixture() {
    let turns = RunTurns {
        run_id: "task_search".to_string(),
        project_id: "proj_shop".to_string(),
        project_name: "Shop".to_string(),
        title: "VibeDev · Speed up product search".to_string(),
        status: "completed".to_string(),
        trusted: true,
        work_location: None,
        branch: Some("vibedev/shop-search01".to_string()),
        crew_id: None,
        lane: None,
        turns: vec![
            RunTurn {
                task_id: "task_search".to_string(),
                prompt: "Product search takes 900 ms on large catalogs. Make it fast.".to_string(),
                mode: "build".to_string(),
                status: "completed".to_string(),
                created_at: "2026-10-13T09:00:00Z".to_string(),
                updated_at: "2026-10-13T09:17:00Z".to_string(),
                summary: Some("Search uses a trigram index: 900 ms to 38 ms.".to_string()),
                engine: Some("codex".to_string()),
                coding_route_id: None,
                cost_usd: Some(0.61),
                commit: Some("4be1c09d2a7f".to_string()),
                files_changed: Some(2),
                completion_kind: None,
                open_items: Vec::new(),
            },
            RunTurn {
                task_id: "task_search_2".to_string(),
                prompt: "Also cache the empty-query result for 60 seconds.".to_string(),
                mode: "build".to_string(),
                status: "running".to_string(),
                created_at: "2026-10-13T09:20:00Z".to_string(),
                updated_at: "2026-10-13T09:24:00Z".to_string(),
                summary: None,
                engine: Some("codex".to_string()),
                coding_route_id: None,
                cost_usd: None,
                commit: None,
                files_changed: None,
                completion_kind: None,
                open_items: Vec::new(),
            },
        ],
    };
    check_fixture("run_turns.json", &serde_json::to_value(turns).unwrap());
}

#[test]
fn turn_timeline_fixture() {
    let event = |kind: &str, data: Value, at: i64| json!({ "event_type": kind, "data": data, "timestamp_ms": at });
    let events = vec![
        event(
            "coding.started",
            json!({ "engine": "codex" }),
            1_760_346_000_000,
        ),
        event(
            "coding.thinking",
            json!({ "delta": "Most time is a full scan on products.title." }),
            1_760_346_001_000,
        ),
        event(
            "coding.tool.started",
            json!({ "tool_name": "bash", "tool_call_id": "t1", "args": { "command": "npm run profile -- search shoe" } }),
            1_760_346_002_000,
        ),
        event(
            "coding.tool.finished",
            json!({ "tool_name": "bash", "tool_call_id": "t1", "is_error": false }),
            1_760_346_009_000,
        ),
        event(
            "coding.tool.started",
            json!({ "tool_name": "edit", "tool_call_id": "t2", "args": { "path": "db/migrations/0042_title_trigram.sql" } }),
            1_760_346_010_000,
        ),
        event(
            "coding.tool.finished",
            json!({ "tool_name": "edit", "tool_call_id": "t2", "is_error": false }),
            1_760_346_011_000,
        ),
        event(
            "coding.tool.started",
            json!({ "tool_name": "bash", "tool_call_id": "t3", "args": { "command": "npm test" } }),
            1_760_346_012_000,
        ),
        event(
            "coding.tool.finished",
            json!({ "tool_name": "bash", "tool_call_id": "t3", "is_error": true, "result": "1 failing: search returns stale rows" }),
            1_760_346_030_000,
        ),
        event(
            "coding.message",
            json!({ "delta": "Search now uses a trigram index: 900 ms to 38 ms." }),
            1_760_346_040_000,
        ),
        event(
            "coding.completed",
            json!({ "assistant_text": "Added the index and a migration.", "committed": true }),
            1_760_346_041_000,
        ),
    ];
    let timeline = TurnTimeline {
        task_id: "task_search".to_string(),
        cards: fold_timeline(&events),
        truncated: false,
    };
    check_fixture(
        "turn_timeline.json",
        &serde_json::to_value(timeline).unwrap(),
    );
}

#[test]
fn run_changes_fixture() {
    let changes = RunChanges {
        run_id: "task_search".to_string(),
        turn: Some("task_search".to_string()),
        source: "branch".to_string(),
        files: vec![
            ChangedFile {
                path: "db/migrations/0042_title_trigram.sql".to_string(),
                status: "A".to_string(),
                additions: 3,
                deletions: 0,
            },
            ChangedFile {
                path: "src/api/search.ts".to_string(),
                status: "M".to_string(),
                additions: 5,
                deletions: 2,
            },
        ],
        patch: Some(FilePatch {
            path: "src/api/search.ts".to_string(),
            unified_diff: "diff --git a/src/api/search.ts b/src/api/search.ts\nindex 1a2b3c4..5d6e7f8 100644\n--- a/src/api/search.ts\n+++ b/src/api/search.ts\n@@ -10,4 +10,7 @@ export async function search(q: string) {\n   const term = q.trim();\n-  const rows = await db.products.scan();\n-  return rows.filter((row) => row.title.includes(term));\n+  if (!term) return cachedEmpty();\n+  return db.products.query(\n+    'SELECT * FROM products WHERE title % $1 ORDER BY similarity(title, $1) DESC',\n+    [term],\n+  );\n }\n".to_string(),
            truncated: false,
        }),
    };
    check_fixture("run_changes.json", &serde_json::to_value(changes).unwrap());
}

#[test]
fn crews_fixture() {
    let lane =
        |name: &str, role: LaneRole, status: LaneStatus, run: Option<&str>, owns: &[&str]| {
            CrewLane {
                lane: name.to_string(),
                brief: format!("{name}: build its part of login with email codes."),
                owns: owns.iter().map(|path| path.to_string()).collect(),
                coding_profile_id: None,
                run_task_id: run.map(str::to_string),
                status,
                role,
            }
        };
    let running = VibeDevCrew {
        crew_id: "crew_7f3a".to_string(),
        goal: "Login with email codes instead of passwords".to_string(),
        status: CrewStatus::Running,
        mode: CrewMode::Split,
        lanes: vec![
            lane(
                "API",
                LaneRole::Work,
                LaneStatus::Done,
                Some("task_api"),
                &["src/api/auth"],
            ),
            lane(
                "Login form",
                LaneRole::Work,
                LaneStatus::Running,
                Some("task_form"),
                &["web/src/login"],
            ),
        ],
        review: true,
        winner: None,
        choose_request_id: None,
        resolve_request_id: None,
        budget_usd: Some(6.0),
        spent_usd: Some(1.48),
        start_request_id: None,
        branch: None,
        conflict: None,
        pull_request_request_id: None,
        pull_request_url: None,
        note: Some("1 of 2 lanes: 1 done, 1 running".to_string()),
        created_at_ms: 1_760_346_000_000,
        updated_at_ms: 1_760_346_600_000,
    };
    let race = VibeDevCrew {
        crew_id: "crew_race".to_string(),
        goal: "Make checkout one page".to_string(),
        status: CrewStatus::Choosing,
        mode: CrewMode::Race,
        lanes: vec![
            lane(
                "Attempt 1 · claude-code",
                LaneRole::Work,
                LaneStatus::Done,
                Some("task_r1"),
                &[],
            ),
            lane(
                "Attempt 2 · codex",
                LaneRole::Work,
                LaneStatus::Failed,
                Some("task_r2"),
                &[],
            ),
        ],
        review: false,
        winner: None,
        choose_request_id: Some("vibedev-crew-crew_race-choose".to_string()),
        resolve_request_id: None,
        budget_usd: None,
        spent_usd: Some(2.1),
        start_request_id: None,
        branch: None,
        conflict: None,
        pull_request_request_id: None,
        pull_request_url: None,
        note: Some("Pick the attempt to keep".to_string()),
        created_at_ms: 1_760_340_000_000,
        updated_at_ms: 1_760_345_000_000,
    };
    check_fixture("crews.json", &json!({ "crews": [running, race] }));
}

#[test]
fn repos_suggest_fixture() {
    let repos = vec![
        RepoSuggestion {
            path: "/Users/owner/code/shop".to_string(),
            name: "shop".to_string(),
            project: Some("Shop".to_string()),
        },
        RepoSuggestion {
            path: "/Users/owner/code/billing".to_string(),
            name: "billing".to_string(),
            project: None,
        },
    ];
    check_fixture("repos_suggest.json", &json!({ "repos": repos }));
}
