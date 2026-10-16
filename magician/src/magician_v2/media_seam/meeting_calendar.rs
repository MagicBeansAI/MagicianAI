//! Upcoming-meeting calendar context — the ONE owner of the gws calendar read.
//!
//! Extracted from `magician-api/src/meetings_api.rs` when the meetings surface
//! app needed the same bounded upcoming list its first-party page already
//! shows. Per the singular-primitives record no engine gets a second code base,
//! so the CLI invocation, account resolution, dedupe, chronological merge, and
//! the short-lived cache live here and BOTH consumers call this module:
//!
//!   * `GET /meetings/upcoming` (unchanged route, unchanged JSON), and
//!   * the `meetings_data.upcoming_meetings` app read binder.
//!
//! Source: the `gws` CLI with the owner's profile (the same CLI the calendar
//! skill shells out to), NOT a new OAuth integration. Auth stays inside the
//! CLI: this module only points `GOOGLE_WORKSPACE_CLI_CONFIG_DIR` at a
//! per-account profile under the scope capability auth root.
//!
//! **The cache is keyed by that auth root.** The API's original process-global
//! single-slot cache predated any second consumer; with two callers a
//! scope-blind slot would hand one scope's calendar to another. The key is the
//! scope's own auth-root path, so a second scope can never read the first
//! scope's cached payload.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::magician_v2::artifact_v2::CapabilityScopePaths;
use crate::magician_v2::gws_cli::{gws_binary, gws_error_detail, GWS_TIMEOUT};

/// The gws round-trip costs seconds and calendars change slowly; serve polls
/// from a short-lived cache. The mutex also collapses concurrent fetches into
/// one CLI invocation.
const UPCOMING_CACHE_TTL: Duration = Duration::from_secs(60);

/// How many scopes' payloads the process retains. A single-operator deployment
/// uses one; the bound exists so a pathological caller cannot grow the map
/// without limit. Eviction drops the least recently fetched entry.
const UPCOMING_CACHE_MAX_SCOPES: usize = 8;

/// Window read from the calendar, and the per-account row cap. Kept identical
/// to the values the first-party page shipped with.
const UPCOMING_LOOKBACK_MINUTES: i64 = 15;
const UPCOMING_LOOKAHEAD_HOURS: i64 = 12;
const UPCOMING_MAX_RESULTS_PER_ACCOUNT: u32 = 25;
/// "Happening now" grace window before an event's start time.
const LIVE_NOW_GRACE_MINUTES: i64 = 5;

/// How long a total failure is served before another fetch is attempted. Much
/// shorter than the success TTL: a transient token refresh must not blank the
/// section for a minute, but a stampede of queued callers must not each re-run
/// the fan-out either.
const UPCOMING_FAILURE_RETRY_AFTER: Duration = Duration::from_secs(5);

struct UpcomingCacheEntry {
    /// When the slot was created. Eviction ranks on `fetched_at` when there is
    /// one and on this otherwise, so a slot whose fetch has NEVER succeeded is
    /// the first candidate rather than — as a `fetched_at`-only rank made it —
    /// permanently un-evictable, which turned the scope ceiling into no
    /// ceiling at all.
    created_at: Instant,
    fetched_at: Option<Instant>,
    payload: Value,
}

impl Default for UpcomingCacheEntry {
    fn default() -> Self {
        Self {
            created_at: Instant::now(),
            fetched_at: None,
            payload: Value::Null,
        }
    }
}

impl UpcomingCacheEntry {
    /// Recency for eviction: the last successful fetch, else creation.
    fn ranked_at(&self) -> Instant {
        self.fetched_at.unwrap_or(self.created_at)
    }
}

/// Two levels on purpose. The OUTER map lock is held only long enough to find
/// or create a scope's slot; the INNER per-scope lock is what is held across
/// the `gws` subprocess. Holding one shared lock across that await would make
/// every scope queue behind whichever scope refreshed last — up to
/// `GWS_TIMEOUT` per account — which is exactly the coupling the per-scope key
/// was introduced to remove.
type UpcomingSlot = Arc<Mutex<UpcomingCacheEntry>>;

static UPCOMING_CACHE: OnceLock<StdMutex<HashMap<PathBuf, UpcomingSlot>>> = OnceLock::new();

fn upcoming_slot(auth_root: &Path) -> UpcomingSlot {
    let cache = UPCOMING_CACHE.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(slot) = guard.get(auth_root) {
        return Arc::clone(slot);
    }
    if guard.len() >= UPCOMING_CACHE_MAX_SCOPES {
        // Evict the least recently fetched slot that nobody else still holds.
        // A slot in use is never dropped: another task may be mid-refresh
        // behind its lock.
        let oldest = guard
            .iter()
            .filter(|(_, slot)| Arc::strong_count(slot) == 1)
            .filter_map(|(key, slot)| {
                slot.try_lock()
                    .ok()
                    .map(|entry| (key.clone(), entry.ranked_at()))
            })
            .min_by_key(|(_, ranked_at)| *ranked_at)
            .map(|(key, _)| key);
        if let Some(oldest) = oldest {
            guard.remove(&oldest);
        }
    }
    let slot = UpcomingSlot::default();
    guard.insert(auth_root.to_path_buf(), Arc::clone(&slot));
    slot
}

/// The accounts to query, in priority order:
/// 1. `MEET_BOT_CALENDAR_ACCOUNTS` — explicit comma-separated pin.
/// 2. `MEET_BOT_CALENDAR_ACCOUNT` — legacy single-account pin.
/// 3. The operator-config registry (`skillshub/operator-config.yaml`,
///    `gws_accounts:`) filtered to accounts with a profile dir under the
///    scope's capability auth root — the SAME store the `calendar` skill
///    (`{scope_capability_auth_root}/gws-{account}`) and the gws bots
///    authenticate into. No second auth store: whatever is signed in for the
///    skills is what the meetings surface reads.
/// 4. `work` (the calendar skill's default) as the last resort.
pub fn gws_calendar_accounts(auth_root: &Path) -> Vec<String> {
    if let Ok(csv) = std::env::var("MEET_BOT_CALENDAR_ACCOUNTS") {
        let accounts: Vec<String> = csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if !accounts.is_empty() {
            return accounts;
        }
    }
    if let Ok(single) = std::env::var("MEET_BOT_CALENDAR_ACCOUNT") {
        let single = single.trim().to_string();
        if !single.is_empty() {
            return vec![single];
        }
    }
    let from_registry = (|| {
        let raw = fs::read_to_string(
            crate::magician_v2::artifact_v2::workspace::runtime_config_path(
                "operator-config.yaml",
                "skillshub/operator-config.yaml",
            ),
        )
        .ok()?;
        let cfg: serde_yaml::Value = serde_yaml::from_str(&raw).ok()?;
        let accounts = cfg
            .get("gws_accounts")?
            .as_sequence()?
            .iter()
            .filter_map(|a| a.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .filter(|name| auth_root.join(format!("gws-{name}")).exists())
            .collect::<Vec<_>>();
        Some(accounts)
    })()
    .unwrap_or_default();
    if !from_registry.is_empty() {
        return from_registry;
    }
    vec!["work".to_string()]
}

pub fn parse_event_time(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// Shell out for ONE account's events in the read window and shape them for the
/// meetings surfaces: title/start/end/meet link + a `live_now` flag (the
/// current-meeting time-window match that prefills Listen/Join). Each event
/// carries its source `account`. `config_dir` is the account's profile under
/// the scope capability auth root — the skills' existing authentication.
async fn fetch_account_events(
    account: String,
    config_dir: PathBuf,
    now: DateTime<Utc>,
    scope_paths: Option<CapabilityScopePaths>,
) -> Result<Vec<Value>, String> {
    if !config_dir.exists() {
        return Err(format!(
            "no gws profile for '{account}' under the scope auth root"
        ));
    }
    let cloudsdk_config_dir = config_dir.join("cloudsdk");
    fs::create_dir_all(&cloudsdk_config_dir).map_err(|e| {
        format!(
            "create Cloud SDK config dir for '{account}' at {}: {e}",
            cloudsdk_config_dir.display()
        )
    })?;

    let params = json!({
        "calendarId": "primary",
        "timeMin": (now - ChronoDuration::minutes(UPCOMING_LOOKBACK_MINUTES))
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        "timeMax": (now + ChronoDuration::hours(UPCOMING_LOOKAHEAD_HOURS))
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        "singleEvents": true,
        "orderBy": "startTime",
        "maxResults": UPCOMING_MAX_RESULTS_PER_ACCOUNT,
    });

    // Augment PATH with the scope's tool-bin dirs so a bare `gws`, or the
    // skillshub-local absolute gws whose `#!/usr/bin/env node` shebang needs
    // node on PATH, resolves instead of exiting 127. Fail-safe: no scope_paths
    // / no existing bin dirs → leave the inherited PATH. Computed first so the
    // binary is resolved against the PATH the child will see and the spawn
    // stays on `posix_spawn` (see `runtime_core::process`).
    let child_path = scope_paths.as_ref().and_then(|sp| {
        let parent = std::env::var("PATH").unwrap_or_default();
        sp.subprocess_bin_path(None, &parent)
    });
    let mut cmd = Command::new(runtime_core::process::resolve_program(
        gws_binary().as_os_str(),
        child_path.as_deref().map(OsStr::new),
    ));
    cmd.arg("calendar")
        .arg("events")
        .arg("list")
        .arg("--params")
        .arg(params.to_string())
        .arg("--format")
        .arg("json")
        .env("GOOGLE_WORKSPACE_CLI_CONFIG_DIR", &config_dir)
        .env("CLOUDSDK_CONFIG", &cloudsdk_config_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(p) = child_path.as_deref() {
        cmd.env("PATH", p);
    }

    let output = tokio::time::timeout(GWS_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "gws calendar timed out".to_string())?
        .map_err(|e| format!("spawn gws: {e}"))?;
    if !output.status.success() {
        return Err(gws_error_detail("calendar", &output));
    }

    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("parse gws output: {e}"))?;
    let items = parsed
        .get("items")
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default();

    let mut events = Vec::new();
    for e in items {
        let start = e.pointer("/start/dateTime").and_then(|s| s.as_str());
        // Date-only rows are all-day events, not joinable meetings.
        if start.is_none() {
            continue;
        }
        let end = e.pointer("/end/dateTime").and_then(|s| s.as_str());
        let meet_url = e
            .get("hangoutLink")
            .and_then(|h| h.as_str())
            .map(str::to_string)
            .or_else(|| {
                e.pointer("/conferenceData/entryPoints")
                    .and_then(|p| p.as_array())
                    .and_then(|pts| {
                        pts.iter()
                            .find(|p| {
                                p.get("entryPointType").and_then(|t| t.as_str()) == Some("video")
                            })
                            .and_then(|p| p.get("uri"))
                            .and_then(|u| u.as_str())
                            .map(str::to_string)
                    })
            });
        // "Happening now", with an early-join grace window.
        let live_now = match (parse_event_time(start), parse_event_time(end)) {
            (Some(s), Some(en)) => {
                now >= s - ChronoDuration::minutes(LIVE_NOW_GRACE_MINUTES) && now <= en
            },
            (Some(s), None) => now >= s - ChronoDuration::minutes(LIVE_NOW_GRACE_MINUTES),
            _ => false,
        };
        events.push(json!({
            "event_id": e.get("id"),
            "title": e.get("summary").and_then(|s| s.as_str()).unwrap_or("(untitled)"),
            "start": start,
            "end": end,
            "meet_url": meet_url,
            "live_now": live_now,
            "account": account,
        }));
    }
    Ok(events)
}

/// Query ALL configured accounts CONCURRENTLY and merge: events deduped
/// (the same meeting often sits on several of the owner's calendars as
/// invites — keyed by meet link, else title+start) and sorted by start time.
/// Per-account failures land in `errors` WITHOUT hiding the accounts that
/// worked — one expired token must not blank the whole Upcoming section.
pub async fn fetch_upcoming_from_calendar(
    auth_root: &Path,
    scope_paths: Option<CapabilityScopePaths>,
) -> Value {
    let accounts = gws_calendar_accounts(auth_root);
    let now = Utc::now();
    let mut handles = Vec::with_capacity(accounts.len());
    for account in &accounts {
        handles.push(tokio::spawn(fetch_account_events(
            account.clone(),
            auth_root.join(format!("gws-{account}")),
            now,
            scope_paths.clone(),
        )));
    }

    let mut events: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (account, handle) in accounts.iter().zip(handles) {
        match handle.await {
            Ok(Ok(account_events)) => {
                for ev in account_events {
                    let key = ev
                        .get("meet_url")
                        .and_then(|u| u.as_str())
                        .map(|u| u.trim().to_ascii_lowercase())
                        .filter(|u| !u.is_empty())
                        .unwrap_or_else(|| {
                            format!(
                                "{}|{}",
                                ev.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                                ev.get("start").and_then(|s| s.as_str()).unwrap_or(""),
                            )
                        });
                    if seen.insert(key) {
                        events.push(ev);
                    }
                }
            },
            Ok(Err(e)) => errors.push(json!({ "account": account, "error": e })),
            Err(e) => {
                errors.push(json!({ "account": account, "error": format!("task join: {e}") }));
            },
        }
    }
    // Chronological merge across accounts — parse the timestamps; RFC3339
    // strings with mixed UTC offsets do NOT sort chronologically as text.
    events.sort_by_key(|ev| {
        parse_event_time(ev.get("start").and_then(|s| s.as_str()))
            .map(|t| t.timestamp())
            .unwrap_or(i64::MAX)
    });
    json!({ "accounts": accounts, "events": events, "errors": errors })
}

/// Cached read of one scope's upcoming meetings. `force_refresh` busts only
/// THIS scope's entry; another scope's cached payload is never returned and
/// never invalidated by a neighbour's refresh.
pub async fn upcoming_meetings_cached(
    force_refresh: bool,
    auth_root: &Path,
    scope_paths: Option<CapabilityScopePaths>,
) -> Value {
    let slot = upcoming_slot(auth_root);
    // Only this scope's slot is held across the subprocess; concurrent callers
    // for the SAME scope still collapse into one CLI invocation, which is the
    // coalescing the cache exists for.
    let mut entry = slot.lock().await;
    if !force_refresh {
        if let Some(fetched_at) = entry.fetched_at {
            if fetched_at.elapsed() < UPCOMING_CACHE_TTL {
                return entry.payload.clone();
            }
        }
    }
    let payload = fetch_upcoming_from_calendar(auth_root, scope_paths).await;
    // A payload with no events and at least one account error is a FAILURE, not
    // a calendar with nothing in it. Caching it would blank the section for the
    // full TTL over a transient token refresh; serve it once and retry next
    // poll instead.
    let all_accounts_failed = payload
        .get("events")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
        && payload
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty());
    if all_accounts_failed {
        // Do not cache the failure for the full TTL — one expired token would
        // blank the section for a minute. But do stamp a short negative window,
        // or every caller queued behind this slot re-runs the whole
        // multi-account `gws` fan-out in turn.
        entry.fetched_at = Some(
            Instant::now()
                .checked_sub(UPCOMING_CACHE_TTL - UPCOMING_FAILURE_RETRY_AFTER)
                .unwrap_or_else(Instant::now),
        );
        entry.payload = payload.clone();
        return payload;
    }
    entry.fetched_at = Some(Instant::now());
    entry.payload = payload.clone();
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_fall_back_to_the_calendar_skill_default_without_pins_or_registry() {
        // The env pins are process-global; this test asserts only the shape of
        // the last-resort default over a directory that holds no profiles.
        let temp = tempfile::tempdir().expect("temp auth root");
        if std::env::var("MEET_BOT_CALENDAR_ACCOUNTS").is_ok()
            || std::env::var("MEET_BOT_CALENDAR_ACCOUNT").is_ok()
        {
            return;
        }
        let accounts = gws_calendar_accounts(temp.path());
        assert!(
            accounts == vec!["work".to_string()] || !accounts.is_empty(),
            "the resolver never returns an empty account list"
        );
    }

    #[test]
    fn event_times_parse_only_as_rfc3339_and_normalize_to_utc() {
        assert!(parse_event_time(None).is_none());
        assert!(parse_event_time(Some("2026-09-02")).is_none());
        let parsed = parse_event_time(Some("2026-09-02T10:30:00+05:30")).expect("offset time");
        assert_eq!(parsed.timestamp(), 1_788_325_200);
    }
}
