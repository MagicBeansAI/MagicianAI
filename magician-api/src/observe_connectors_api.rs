//! Consent surface for the account-based WEG connectors (email, calendar).
//!
//! Mirrors the ambient consent API: a scoped, durable per-producer
//! [`ObserveConfig`] (enabled + chosen accounts + frequency + time) the user sets
//! from the `/observe` Email/Calendar cards, plus an account-discovery endpoint so
//! the cards can offer the accounts we already have. Generic by `{producer}`
//! (`email` | `calendar`) — same scalable shape as `evidence::tier_distill`.
//!
//! Nothing is captured until the user enables a producer AND picks ≥1
//! authenticated account; the (stubbed here, wired in the writer step) schedule
//! creation keys off that. Accounts come from `skillshub/operator-config.yaml`
//! `gws_accounts`/`agentmail_accounts`, filtered to authenticated ones, exactly
//! like `meetings_api::gws_calendar_accounts`. Magican's own identity
//! (`gws-presto`) is excluded from EMAIL discovery (Magican's mail is the Mail &
//! chat card's job) but offered for CALENDAR as the `envoy` ("Magican") lane, so
//! the owner can observe Magican's schedule alongside their own ("You") calendars.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
pub use magician::magician_v2::observe_connectors::{
    default_frequency, default_time, default_true, load_config, load_producer_observe_config,
    observe_namespace, ObserveConfig, CONFIG_NAME,
};
use serde::{Deserialize, Serialize};

use crate::scope::resolve_required_scope;
use magician::magician_v2::artifact_v2::models::{TaskLifecycle, TaskOutputMode, TaskSyncMode};
use magician::magician_v2::artifact_v2::service::{ArtifactV2Service, CreateTaskInput, ScopeRef};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifacts::durable_store::{
    open_local_durable_artifacts, DurableArtifactStore, DurableFrontmatter,
};
use magician_comms::channel_assist::channel_observe::account_lane;

pub struct ObserveApi {
    workspace_layout: ArtifactV2Workspace,
    artifact_v2_service: Arc<ArtifactV2Service>,
}

impl ObserveApi {
    pub fn new(
        workspace_layout: ArtifactV2Workspace,
        artifact_v2_service: Arc<ArtifactV2Service>,
    ) -> Self {
        Self {
            workspace_layout,
            artifact_v2_service,
        }
    }

    /// Re-render the existing calendar schedule after the shared catch-up
    /// window changes. Email has no digest task; message ingest owns it.
    pub async fn refresh_calendar_catch_up_window(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<(), String> {
        let store = open_local_durable_artifacts(&self.workspace_layout, principal, workspace)
            .map_err(|error| error.to_string())?;
        let namespace = observe_namespace("calendar");
        let mut config = load_config(&store, &namespace).await;
        if !config.enabled || config.accounts.is_empty() {
            return Ok(());
        }
        // Create and persist the replacement before retiring the old task. A
        // transient failure while editing the policy must not silently disable
        // an otherwise healthy calendar observer.
        let previous_task_id = config.schedule_task_id.clone();
        let mut replacement = config.clone();
        replacement.schedule_task_id = None;
        let Some(replacement_task_id) =
            ensure_schedule(self, principal, workspace, "calendar", &replacement).await
        else {
            return Err("could not create the replacement calendar schedule".to_string());
        };
        config.schedule_task_id = Some(replacement_task_id.clone());
        if let Err(error) = save_config(&store, &namespace, principal, &config).await {
            let scope = ScopeRef::system_internal_unauthenticated(
                &principal.to_string(),
                &workspace.to_string(),
            );
            let _ = self
                .artifact_v2_service
                .archive_task(&scope, &replacement_task_id)
                .await;
            return Err(error);
        }
        if let Some(previous_task_id) = previous_task_id.filter(|id| !id.is_empty()) {
            let scope = ScopeRef::system_internal_unauthenticated(
                &principal.to_string(),
                &workspace.to_string(),
            );
            self.artifact_v2_service
                .archive_task(&scope, &previous_task_id)
                .await
                .map_err(|error| {
                    format!(
                        "replacement calendar schedule is active, but the previous schedule could not be archived: {error}"
                    )
                })?;
        }
        Ok(())
    }
}

fn is_observe_producer(producer: &str) -> bool {
    matches!(producer, "email" | "calendar")
}

fn err_json(status: actix_web::http::StatusCode, message: impl std::fmt::Display) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({ "error": message.to_string() }))
}

async fn save_config(
    store: &DurableArtifactStore,
    namespace: &str,
    agent_hint: &str,
    config: &ObserveConfig,
) -> Result<(), String> {
    let body = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    let frontmatter = DurableFrontmatter {
        namespace: namespace.to_string(),
        name: CONFIG_NAME.to_string(),
        created_by: "observe".to_string(),
        last_updated_by: "observe".to_string(),
        last_updated: chrono::Utc::now(),
        content_type: Some("application/json".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: Some(agent_hint.to_string()),
        producer_stage: Some("observe_config".to_string()),
    };
    store
        .write(namespace, CONFIG_NAME, &body, frontmatter)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ─── account discovery ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ObserveAccount {
    /// Alias the `gmail`/`calendar` skills take (`work` | `personal` | `business`).
    pub name: String,
    /// Display email (from `operator-config.yaml` `expected_email`/`email`).
    pub email: String,
    /// `gmail` | `agentmail` | `calendar`.
    pub account_type: String,
    /// OAuth/profile authenticated for this scope.
    pub connected: bool,
    /// Assistance lane: `user_assist` (the owner's own accounts, "You") or
    /// `envoy` (Magican's own identity, "Magican"). Matches the mail/chat card's
    /// lane badges.
    pub lane: String,
}

/// Read the operator-config account registries, filtered to this scope's
/// authenticated ones. `gws-presto` (the agent's own identity) is excluded from
/// email but appended to calendar as the `envoy` lane.
fn discover_accounts(auth_root: &std::path::Path) -> (Vec<ObserveAccount>, Vec<ObserveAccount>) {
    let raw = std::fs::read_to_string(
        magician::magician_v2::artifact_v2::workspace::runtime_config_path(
            "operator-config.yaml",
            "skillshub/operator-config.yaml",
        ),
    )
    .unwrap_or_default();
    let cfg: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap_or(serde_yaml::Value::Null);

    let gws: Vec<(String, String, bool)> = cfg
        .get("gws_accounts")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|a| {
                    let name = a.get("name")?.as_str()?.to_string();
                    if name == "presto" {
                        return None; // the agent's identity, not an owner account
                    }
                    let email = a
                        .get("expected_email")
                        .and_then(|e| e.as_str())
                        .unwrap_or("")
                        .to_string();
                    let connected = auth_root.join(format!("gws-{name}")).exists();
                    Some((name, email, connected))
                })
                .collect()
        })
        .unwrap_or_default();

    // Lanes are data-driven (`account_lane` reads operator-config
    // `agent_accounts:` + shipped defaults) — no hardcoded `presto`/`envoy`.
    let mut email_accounts: Vec<ObserveAccount> = gws
        .iter()
        .map(|(name, email, connected)| ObserveAccount {
            name: name.clone(),
            email: email.clone(),
            account_type: "gmail".to_string(),
            connected: *connected,
            lane: account_lane("gmail", name).as_db_str().to_string(),
        })
        .collect();
    // AgentMail inboxes (Presto's agent-owned email) — keyed by env, treat as connected.
    if let Some(seq) = cfg.get("agentmail_accounts").and_then(|v| v.as_sequence()) {
        for a in seq {
            if let Some(name) = a.get("name").and_then(|n| n.as_str()) {
                email_accounts.push(ObserveAccount {
                    name: name.to_string(),
                    email: a
                        .get("email")
                        .and_then(|e| e.as_str())
                        .unwrap_or("")
                        .to_string(),
                    account_type: "agentmail".to_string(),
                    connected: true,
                    lane: account_lane("agentmail", name).as_db_str().to_string(),
                });
            }
        }
    }

    let mut calendar_accounts: Vec<ObserveAccount> = gws
        .into_iter()
        .map(|(name, email, connected)| ObserveAccount {
            lane: account_lane("calendar", &name).as_db_str().to_string(),
            name,
            email,
            account_type: "calendar".to_string(),
            connected,
        })
        .collect();
    // Presto's own Google calendar (`gws-presto`, e.g. reach.magican@gmail.com) — the
    // ENVOY lane. Excluded from email discovery (Presto's mail is the Mail &
    // chat card's job) but offered for calendar so the owner can observe
    // Presto's schedule alongside their own. Email comes from the `presto`
    // `gws_accounts` entry; connected iff its profile is authenticated.
    let presto_email = cfg
        .get("gws_accounts")
        .and_then(|v| v.as_sequence())
        .and_then(|seq| {
            seq.iter().find_map(|a| {
                if a.get("name").and_then(|n| n.as_str()) == Some("presto") {
                    Some(
                        a.get("expected_email")
                            .and_then(|e| e.as_str())
                            .unwrap_or("")
                            .to_string(),
                    )
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();
    calendar_accounts.push(ObserveAccount {
        email: presto_email,
        account_type: "calendar".to_string(),
        connected: auth_root.join("gws-presto").exists(),
        lane: account_lane("calendar", "presto").as_db_str().to_string(),
        name: "presto".to_string(),
    });

    (email_accounts, calendar_accounts)
}

#[derive(Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `GET /observe/accounts` — the accounts the Email/Calendar cards can offer.
pub async fn get_observe_accounts_handler(
    api: web::Data<ObserveApi>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let auth_root = api
        .workspace_layout
        .capability_auth_root(&principal, &workspace);
    let (email_accounts, calendar_accounts) = discover_accounts(&auth_root);
    HttpResponse::Ok().json(serde_json::json!({
        "email_accounts": email_accounts,
        "calendar_accounts": calendar_accounts,
    }))
}

// ─── per-producer config ─────────────────────────────────────────────────────

/// `GET /observe/{producer}/status` — the persisted consent config.
pub async fn get_observe_status_handler(
    api: web::Data<ObserveApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let producer = path.into_inner();
    if !is_observe_producer(&producer) {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            format!("unknown observe producer: {producer}"),
        );
    }
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let store = match open_local_durable_artifacts(&api.workspace_layout, &principal, &workspace) {
        Ok(store) => store,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let config = load_config(&store, &observe_namespace(&producer)).await;
    HttpResponse::Ok().json(config)
}

#[derive(Deserialize)]
pub struct ObserveConfigUpdate {
    pub enabled: bool,
    #[serde(default)]
    pub accounts: Vec<String>,
    #[serde(default = "default_frequency")]
    pub frequency: String,
    #[serde(default = "default_time")]
    pub time: String,
    #[serde(default = "default_true")]
    pub suppress_sensitive: bool,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `PUT /observe/{producer}/config` — set consent (enabled + accounts + cadence).
/// Validates chosen accounts against the authenticated ones, then (writer step)
/// creates/updates/cancels the scheduled writer task. Refuses to enable with no
/// authenticated account selected.
pub async fn put_observe_config_handler(
    api: web::Data<ObserveApi>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ObserveConfigUpdate>,
) -> HttpResponse {
    let producer = path.into_inner();
    if !is_observe_producer(&producer) {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            format!("unknown observe producer: {producer}"),
        );
    }
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    let auth_root = api
        .workspace_layout
        .capability_auth_root(&principal, &workspace);
    let (email_accounts, calendar_accounts) = discover_accounts(&auth_root);
    let available: std::collections::HashSet<String> = if producer == "email" {
        email_accounts
    } else {
        calendar_accounts
    }
    .into_iter()
    .filter(|a| a.connected)
    .map(|a| a.name)
    .collect();

    // Intersect the submitted accounts with the authenticated ones — the consent
    // gate: only accounts the user picked AND that are actually connected.
    let accounts: Vec<String> = body
        .accounts
        .into_iter()
        .map(|a| a.trim().to_string())
        .filter(|a| available.contains(a))
        .collect();

    if body.enabled && accounts.is_empty() {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            "select at least one connected account before enabling",
        );
    }

    let store = match open_local_durable_artifacts(&api.workspace_layout, &principal, &workspace) {
        Ok(store) => store,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let namespace = observe_namespace(&producer);
    let mut config = load_config(&store, &namespace).await;
    config.enabled = body.enabled;
    config.accounts = accounts;
    config.frequency = body.frequency;
    config.time = body.time;
    config.suppress_sensitive = body.suppress_sensitive;

    // Create/update/cancel the scheduled writer task from (enabled, accounts,
    // frequency, time) and stash its id on the config.
    config.schedule_task_id =
        ensure_schedule(api.get_ref(), &principal, &workspace, &producer, &config).await;

    if let Err(err) = save_config(&store, &namespace, &principal, &config).await {
        return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
    }

    // Adapter (unified observe+assist U2): mirror this producer's accounts into
    // the unified `channel_observe` config the workers now read, so old-UI edits
    // reach them until U4 replaces this surface. Best-effort — a legacy save
    // must not fail on the mirror.
    if let Err(err) = magician_comms::channel_assist::channel_observe::upsert_producer_channels(
        &api.workspace_layout,
        &principal,
        &workspace,
        &producer,
        &config,
    )
    .await
    {
        tracing::warn!(producer, %err, "channel_observe upsert (observe PUT adapter) failed");
    }

    HttpResponse::Ok().json(config)
}

/// Compiled fallback for the writer task description (if the prompt store is
/// unavailable). Mirrors `observe_writer_v1.0.0.json` minus the per-producer vars.
const WRITER_FALLBACK: &str = "You are running on a schedule to build METADATA-ONLY work-evidence. For each chosen account, read the recent window via the appropriate skill, roll it up per day, and persist one row per (account, day) into the producer's user-memory tier via `update_memory_tier` (single key `<prefix>:<account>:<YYYY-MM-DD>`, with `account`, `day`, `source_type`, the metadata fields, and a one-line summary). Never store bodies or sensitive contents; skip accounts with no real activity. FINAL STEP: after all rows are written, call `distill_evidence` with the producer name (\"email\" or \"calendar\") to promote them into reviewable work-evidence.";

fn bounded_writer_description(rendered: &str, window_days: u32, max_items: usize) -> String {
    format!(
        "{rendered}\n\nHARD OBSERVATION BOUNDS: inspect no data older than {window_days} day(s) and no more than {max_items} source item(s) across this run. Stop cleanly when either bound is reached."
    )
}

/// Per-producer variable bindings for the generic `observe_writer` prompt.
fn writer_vars(
    producer: &str,
    accounts: &[String],
    window_days: u32,
    max_items: usize,
) -> std::collections::HashMap<String, String> {
    let mut vars = std::collections::HashMap::new();
    vars.insert("accounts".to_string(), accounts.join(", "));
    vars.insert("window_days".to_string(), window_days.to_string());
    vars.insert("max_items".to_string(), max_items.to_string());
    // The lane name the writer passes to `distill_evidence` as its final step.
    vars.insert("producer".to_string(), producer.to_string());
    if producer == "calendar" {
        vars.insert("producer_label".to_string(), "your calendar".to_string());
        vars.insert("skill".to_string(), "calendar".to_string());
        // The tier names are the shared tier-name contract (plan 3.1
        // prerequisite (b)) — same strings, one home.
        vars.insert(
            "tier".to_string(),
            magician::magician_v2::evidence::tier_contracts::CALENDAR_EVIDENCE_TIER.to_string(),
        );
        vars.insert("key_prefix".to_string(), "cal".to_string());
        vars.insert("source_type".to_string(), "calendar_capture".to_string());
        vars.insert(
            "fields_hint".to_string(),
            "the event titles, attendee names/domains, and the event count".to_string(),
        );
        vars.insert(
            "fields_keys".to_string(),
            "\"event_titles\": [..], \"attendees\": [..], \"event_count\": N".to_string(),
        );
    } else {
        vars.insert("producer_label".to_string(), "your email".to_string());
        vars.insert("skill".to_string(), "gmail".to_string());
        vars.insert(
            "tier".to_string(),
            magician::magician_v2::evidence::tier_contracts::EMAIL_EVIDENCE_TIER.to_string(),
        );
        vars.insert("key_prefix".to_string(), "email".to_string());
        vars.insert("source_type".to_string(), "email_capture".to_string());
        vars.insert(
            "fields_hint".to_string(),
            "the subjects, sender names/domains, and the thread count".to_string(),
        );
        vars.insert(
            "fields_keys".to_string(),
            "\"subjects\": [..], \"senders\": [..], \"thread_count\": N".to_string(),
        );
    }
    vars
}

/// Whether a producer still creates a scheduled digest writer task. Unified
/// observe+assist U3: the EMAIL digest is RETIRED — channel-assist's evidence
/// bridge now owns the email→WEG feed (local summaries, not metadata-only
/// digest rows), so no new "Observe email" task is created (and any prior one
/// is archived by the teardown in `ensure_schedule`). Calendar keeps its
/// digest (events, not messages — no channel-assist ingest).
pub fn producer_has_digest(producer: &str) -> bool {
    producer != "email"
}

/// Create / update / cancel the per-producer scheduled WRITER task from the config.
/// Tears down any prior task, then (if still enabled with accounts AND the
/// producer still has a digest — see [`producer_has_digest`]) creates a fresh
/// `executive-assistant` task on a `Cron` from `frequency`+`time` whose description
/// is the rendered `observe_writer` prompt — it reads the chosen accounts via the
/// gmail/calendar skills and persists metadata-only rows via `update_memory_tier`
/// into `user.{producer}_evidence`, which `tier_distill` turns into evidence.
async fn ensure_schedule(
    api: &ObserveApi,
    principal: &str,
    workspace: &str,
    producer: &str,
    config: &ObserveConfig,
) -> Option<String> {
    let scope =
        ScopeRef::system_internal_unauthenticated(&principal.to_string(), &workspace.to_string());
    // Tear down the prior schedule, then recreate if still enabled — a simple,
    // race-free "replace" so a changed cadence/account set takes effect cleanly.
    if let Some(old) = config.schedule_task_id.as_deref().filter(|s| !s.is_empty()) {
        let _ = api.artifact_v2_service.archive_task(&scope, old).await;
    }
    // Unified observe+assist U3: the email digest is retired (bridge owns the
    // email→WEG feed). The teardown above archives any pre-existing "Observe
    // email" task on this save; we never (re)create one.
    if !producer_has_digest(producer) {
        return None;
    }
    if !config.enabled || config.accounts.is_empty() {
        return None;
    }
    let catch_up = magician::magician_v2::observe_catchup::load_observe_catch_up_policy(
        &api.workspace_layout,
        principal,
        workspace,
    )
    .await;
    let window_days = if catch_up.enabled {
        catch_up.lookback_days
    } else {
        1
    };
    let rendered = magician::magician_v2::prompts::rendered_prompt_or(
        "observe_writer",
        "1.0.0",
        writer_vars(
            producer,
            &config.accounts,
            window_days,
            catch_up.max_items_per_source,
        ),
        WRITER_FALLBACK,
    )
    .await;
    // Append the invariant outside the editable prompt template so both the
    // managed prompt and compiled fallback obey the same owner-selected cap.
    let description =
        bounded_writer_description(&rendered, window_days, catch_up.max_items_per_source);
    let schedule = serde_json::json!({
        "kind": "cron",
        "cron": cron_from(&config.frequency, &config.time),
    });
    let input = CreateTaskInput {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        title: format!("Observe {producer}"),
        description,
        agent_id: "executive-assistant".to_string(),
        goal_id: None,
        ui_thread_id: "general".to_string(),
        priority: None,
        due_date: None,
        tags: Vec::new(),
        created_by: "observe".to_string(),
        depends_on: Vec::new(),
        approved: true,
        schedule: Some(schedule),
        output_mode: TaskOutputMode::default(),
        chat_session_id: None,
        lifecycle: TaskLifecycle::Internal,
        sync_mode: TaskSyncMode::Deferred,
    };
    match api.artifact_v2_service.create_task(input).await {
        Ok(task) => Some(task.manifest.task_id),
        Err(error) => {
            tracing::warn!(producer, %error, "observe schedule create failed");
            None
        },
    }
}

/// Translate a card's `frequency` + `HH:MM` `time` into a cron expression.
/// `daily` → `M H * * *`; `twice-daily` → `M H,H+12 * * *`; `hourly` → `M * * * *`.
pub fn cron_from(frequency: &str, time: &str) -> String {
    let (hour, minute) = parse_hhmm(time).unwrap_or((7, 0));
    match frequency {
        "hourly" => format!("{minute} * * * *"),
        "twice-daily" => format!("{minute} {hour},{} * * *", (hour + 12) % 24),
        _ => format!("{minute} {hour} * * *"),
    }
}

fn parse_hhmm(time: &str) -> Option<(u32, u32)> {
    let (h, m) = time.trim().split_once(':')?;
    let hour: u32 = h.parse().ok()?;
    let minute: u32 = m.parse().ok()?;
    if hour < 24 && minute < 60 {
        Some((hour, minute))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_from_translates_cadence() {
        assert_eq!(cron_from("daily", "07:00"), "0 7 * * *");
        assert_eq!(cron_from("twice-daily", "07:30"), "30 7,19 * * *");
        assert_eq!(cron_from("hourly", "09:15"), "15 * * * *");
        // bad time falls back to 07:00
        assert_eq!(cron_from("daily", "nope"), "0 7 * * *");
        assert_eq!(cron_from("twice-daily", "23:00"), "0 23,11 * * *");
    }

    #[test]
    fn only_email_and_calendar_are_observe_producers() {
        assert!(is_observe_producer("email"));
        assert!(is_observe_producer("calendar"));
        assert!(!is_observe_producer("meeting"));
        assert!(!is_observe_producer("screen"));
    }

    #[test]
    fn email_digest_is_retired_calendar_keeps_its_digest() {
        // U3: channel-assist's evidence bridge owns the email→WEG feed now.
        assert!(!producer_has_digest("email"));
        assert!(producer_has_digest("calendar"));
    }

    #[test]
    fn default_config_is_disabled_and_safe() {
        let c = ObserveConfig::default();
        assert!(!c.enabled);
        assert!(c.accounts.is_empty());
        assert!(c.suppress_sensitive);
        assert_eq!(c.frequency, "daily");
        assert_eq!(c.time, "07:00");
    }

    #[test]
    fn calendar_writer_bounds_survive_prompt_store_fallbacks() {
        let description = bounded_writer_description("managed or fallback prompt", 7, 50);
        assert!(description.contains("no data older than 7 day(s)"));
        assert!(description.contains("no more than 50 source item(s)"));
        assert!(description.starts_with("managed or fallback prompt"));
    }
}
