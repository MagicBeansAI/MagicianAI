//! Source-specific actions exposed by the Today projection.
//!
//! This deliberately mirrors the channel action-adapter pattern without
//! coupling Today to channel provider/account semantics. A source adapter
//! advertises actions, supplies durable linkage, and translates a selected
//! action into a source-agnostic execution plan. The API executes that plan.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::{
    models::{TaskLifecycle, TaskListItemV3, TaskOutputMode, TaskSyncMode, TaskTagRecord},
    CreateTaskInput,
};

use super::types::FeedAction;

pub const CREATE_TASK_ACTION_ID: &str = "create_task";

/// Source kind for acknowledged, unlinked action-kind Thinking Map nodes
/// surfaced as Today candidates (Live Thinking Map plan Phase 8).
pub const THINKING_MAP_ACTION_SOURCE_KIND: &str = "thinking_map_action";

/// Source kind for material monitor updates projected into Today `Changed`
/// (Recurring Monitors Phase 3, plan §9.2).
pub const MONITOR_UPDATE_SOURCE_KIND: &str = "monitor_update";

/// Navigation action id on monitor Today cards: open the canonical monitor
/// surface. The web reads `/tasks?type=monitors&selected={task_id}
/// [&update={update_id}]` (`taskRoutes.ts monitorsTaskRoute`) — the deep
/// link must target that route, not the plain task view.
pub const OPEN_TASK_ACTION_ID: &str = "open_task";

/// Canonical monitor deep link (mirror of the web's `monitorsTaskRoute`):
/// `/tasks?type=monitors&selected={task_id}[&update={update_id}]`.
pub fn monitor_deep_link(task_id: &str, update_id: Option<&str>) -> String {
    let mut url = format!(
        "/tasks?type=monitors&selected={}",
        urlencoding::encode(task_id)
    );
    if let Some(update_id) = update_id
        .map(str::trim)
        .filter(|update_id| !update_id.is_empty())
    {
        url.push_str(&format!("&update={}", urlencoding::encode(update_id)));
    }
    url
}

/// Durable machine-readable provenance line written into the description of a
/// task promoted from a Thinking Map node. Mirrors the meeting adapter's
/// `Meeting action source:` marker so reconciliation can match a task back to
/// its source node even if the map link commit was interrupted or the task was
/// renamed.
pub fn thinking_map_node_source_marker(map_id: &str, node_id: &str) -> String {
    format!("Thinking map node source: {map_id}:{node_id}")
}

#[derive(Debug, Clone)]
pub struct TodayActionSubject {
    pub item_id: String,
    pub principal: String,
    pub workspace: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_url: Option<String>,
    pub thread_id: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone)]
pub struct TodayTaskActionPlan {
    pub task_id: String,
    pub input: CreateTaskInput,
}

/// Plan for promoting a Thinking Map node into a task. Execution goes through
/// the EXISTING governed promote flow (`POST
/// /thinking-maps/{map}/nodes/{node}/promote {target:"task"}` semantics —
/// idempotent, provenance-preserving, records the `promoted_ref` link on the
/// node), NOT a second bespoke task-creation path.
#[derive(Debug, Clone)]
pub struct TodayThinkingMapPromotionPlan {
    pub map_id: String,
    pub node_id: String,
}

#[derive(Debug, Clone)]
pub enum TodayActionPlan {
    EnsureTask(TodayTaskActionPlan),
    PromoteThinkingMapNode(TodayThinkingMapPromotionPlan),
}

pub trait TodayActionAdapter: Send + Sync {
    fn source_kind(&self) -> &'static str;
    fn available_actions(&self, subject: &TodayActionSubject) -> Vec<FeedAction>;
    fn linked_task_ids(&self, subject: &TodayActionSubject) -> Vec<String>;
    fn task_is_linked(&self, subject: &TodayActionSubject, task: &TaskListItemV3) -> bool;
    fn plan_action(&self, action_id: &str, subject: &TodayActionSubject)
        -> Result<TodayActionPlan>;
}

struct MeetingActionAdapter;

static MEETING_ACTION_ADAPTER: MeetingActionAdapter = MeetingActionAdapter;

struct ThinkingMapActionAdapter;

static THINKING_MAP_ACTION_ADAPTER: ThinkingMapActionAdapter = ThinkingMapActionAdapter;

struct MonitorActionAdapter;

static MONITOR_ACTION_ADAPTER: MonitorActionAdapter = MonitorActionAdapter;

pub fn today_action_adapter(source_kind: &str) -> Option<&'static dyn TodayActionAdapter> {
    match source_kind {
        "meeting_action" => Some(&MEETING_ACTION_ADAPTER),
        THINKING_MAP_ACTION_SOURCE_KIND => Some(&THINKING_MAP_ACTION_ADAPTER),
        MONITOR_UPDATE_SOURCE_KIND => Some(&MONITOR_ACTION_ADAPTER),
        _ => None,
    }
}

impl TodayActionAdapter for MeetingActionAdapter {
    fn source_kind(&self) -> &'static str {
        "meeting_action"
    }

    fn available_actions(&self, subject: &TodayActionSubject) -> Vec<FeedAction> {
        vec![FeedAction {
            id: CREATE_TASK_ACTION_ID.to_string(),
            label: "Create task".to_string(),
            action_type: Some("today_source_action".to_string()),
            payload: json!({
                "method": "POST",
                "endpoint": format!(
                    "/api/magician/v2/today/items/{}/actions/{CREATE_TASK_ACTION_ID}",
                    subject.item_id
                ),
                "result_navigation": "task",
                "icon": "checklist"
            }),
        }]
    }

    fn linked_task_ids(&self, subject: &TodayActionSubject) -> Vec<String> {
        let mut task_ids = vec![meeting_action_task_id(&subject.source_id)];
        if let Some(explicit) = metadata_string(&subject.metadata, "linked_task_id") {
            if explicit.starts_with("task_") && !task_ids.contains(&explicit) {
                task_ids.push(explicit);
            }
        }
        task_ids
    }

    fn task_is_linked(&self, subject: &TodayActionSubject, task: &TaskListItemV3) -> bool {
        if self.linked_task_ids(subject).contains(&task.id) {
            return true;
        }
        let source_marker = format!("Meeting action source: {}", subject.source_id);
        if task
            .description
            .lines()
            .any(|line| line.trim() == source_marker)
        {
            return true;
        }
        let action_text = metadata_string(&subject.metadata, "action_item").unwrap_or_else(|| {
            subject
                .title
                .trim_start_matches("Meeting action: ")
                .to_string()
        });
        subject.thread_id.as_deref().is_some_and(|thread_id| {
            task.ui_thread_id == thread_id
                && normalized_action_text(&task.title) == normalized_action_text(&action_text)
        })
    }

    fn plan_action(
        &self,
        action_id: &str,
        subject: &TodayActionSubject,
    ) -> Result<TodayActionPlan> {
        if action_id != CREATE_TASK_ACTION_ID {
            return Err(anyhow!("unsupported Today meeting action `{action_id}`"));
        }

        let action_text = metadata_string(&subject.metadata, "action_item").unwrap_or_else(|| {
            subject
                .title
                .trim_start_matches("Meeting action: ")
                .to_string()
        });
        if action_text.trim().is_empty() {
            return Err(anyhow!("meeting action text is missing"));
        }

        let meeting_title = metadata_string(&subject.metadata, "meeting_title");
        let meeting_date = metadata_string(&subject.metadata, "meeting_date");
        let task_id = meeting_action_task_id(&subject.source_id);
        let mut description = vec![
            "OWNER REQUEST".to_string(),
            "Track and complete this action captured from a meeting.".to_string(),
            String::new(),
            "MEETING ACTION".to_string(),
            action_text.clone(),
            String::new(),
            "SOURCE METADATA".to_string(),
            format!("Meeting action source: {}", subject.source_id),
        ];
        if let Some(title) = meeting_title {
            description.push(format!("Meeting: {title}"));
        }
        if let Some(date) = meeting_date {
            description.push(format!("Meeting date: {date}"));
        }
        if let Some(thread_id) = subject.thread_id.as_deref() {
            description.push(format!("Meeting thread: {thread_id}"));
        }
        if let Some(source_url) = subject.source_url.as_deref() {
            description.push(format!("Meeting page: {source_url}"));
        }

        Ok(TodayActionPlan::EnsureTask(TodayTaskActionPlan {
            task_id,
            input: CreateTaskInput {
                principal: subject.principal.clone(),
                workspace: subject.workspace.clone(),
                title: bounded(&action_text, 160),
                description: description.join("\n"),
                agent_id: "personal-assistant".to_string(),
                goal_id: None,
                ui_thread_id: subject
                    .thread_id
                    .clone()
                    .unwrap_or_else(|| "general".to_string()),
                priority: Some("normal".to_string()),
                due_date: None,
                tags: vec![
                    TaskTagRecord {
                        id: "meeting-action".to_string(),
                        name: "meeting-action".to_string(),
                        color: None,
                    },
                    TaskTagRecord {
                        id: "today".to_string(),
                        name: "today".to_string(),
                        color: None,
                    },
                ],
                created_by: "today_meeting_action".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: TaskLifecycle::Persistent,
                sync_mode: TaskSyncMode::Deferred,
            },
        }))
    }
}

impl TodayActionAdapter for ThinkingMapActionAdapter {
    fn source_kind(&self) -> &'static str {
        THINKING_MAP_ACTION_SOURCE_KIND
    }

    fn available_actions(&self, subject: &TodayActionSubject) -> Vec<FeedAction> {
        vec![FeedAction {
            id: CREATE_TASK_ACTION_ID.to_string(),
            label: "Create task".to_string(),
            action_type: Some("today_source_action".to_string()),
            payload: json!({
                "method": "POST",
                "endpoint": format!(
                    "/api/magician/v2/today/items/{}/actions/{CREATE_TASK_ACTION_ID}",
                    subject.item_id
                ),
                "result_navigation": "task",
                "icon": "checklist"
            }),
        }]
    }

    fn linked_task_ids(&self, subject: &TodayActionSubject) -> Vec<String> {
        metadata_string(&subject.metadata, "linked_task_id")
            .filter(|task_id| task_id.starts_with("task_"))
            .into_iter()
            .collect()
    }

    fn task_is_linked(&self, subject: &TodayActionSubject, task: &TaskListItemV3) -> bool {
        if self.linked_task_ids(subject).contains(&task.id) {
            return true;
        }
        let (Some(map_id), Some(node_id)) = (
            metadata_string(&subject.metadata, "map_id"),
            metadata_string(&subject.metadata, "node_id"),
        ) else {
            return false;
        };
        let source_marker = thinking_map_node_source_marker(&map_id, &node_id);
        if task
            .description
            .lines()
            .any(|line| line.trim() == source_marker)
        {
            return true;
        }
        let label = metadata_string(&subject.metadata, "node_label")
            .unwrap_or_else(|| subject.title.trim_start_matches("Map action: ").to_string());
        task.ui_thread_id == format!("thinking-map-{map_id}")
            && normalized_action_text(&task.title) == normalized_action_text(&label)
    }

    fn plan_action(
        &self,
        action_id: &str,
        subject: &TodayActionSubject,
    ) -> Result<TodayActionPlan> {
        if action_id != CREATE_TASK_ACTION_ID {
            return Err(anyhow!(
                "unsupported Today thinking-map action `{action_id}`"
            ));
        }
        let map_id = metadata_string(&subject.metadata, "map_id")
            .ok_or_else(|| anyhow!("thinking-map Today item is missing map_id metadata"))?;
        let node_id = metadata_string(&subject.metadata, "node_id")
            .ok_or_else(|| anyhow!("thinking-map Today item is missing node_id metadata"))?;
        Ok(TodayActionPlan::PromoteThinkingMapNode(
            TodayThinkingMapPromotionPlan { map_id, node_id },
        ))
    }
}

/// Recurring Monitors Phase 3 — Today `Changed` cards for material monitor
/// updates. Unlike the meeting/thinking-map adapters, the linked task ALWAYS
/// exists already (the monitor IS the task), so the only action is
/// navigation to it; nothing is created server-side. Item/update ids are
/// blake3-derived upstream (`mu_<hash of the §7.4 dedupe key>`), mirroring
/// the meeting adapter's derived-id discipline, and reconciliation runs on
/// the `monitor_task_id` metadata.
impl TodayActionAdapter for MonitorActionAdapter {
    fn source_kind(&self) -> &'static str {
        MONITOR_UPDATE_SOURCE_KIND
    }

    fn available_actions(&self, subject: &TodayActionSubject) -> Vec<FeedAction> {
        let Some(task_id) = monitor_subject_task_id(subject) else {
            return Vec::new();
        };
        let update_id = metadata_string(&subject.metadata, "update_id");
        vec![FeedAction {
            id: OPEN_TASK_ACTION_ID.to_string(),
            label: "Open monitor".to_string(),
            action_type: Some(OPEN_TASK_ACTION_ID.to_string()),
            payload: json!({
                // Canonical monitor route incl. the exact update when known
                // (web `monitorsTaskRoute` — NOT the plain /tasks view).
                "url": monitor_deep_link(&task_id, update_id.as_deref()),
                "task_id": task_id,
                "update_id": update_id,
                "change_fingerprint": metadata_string(&subject.metadata, "change_fingerprint"),
                "result_navigation": "task",
                "icon": "monitor"
            }),
        }]
    }

    fn linked_task_ids(&self, subject: &TodayActionSubject) -> Vec<String> {
        monitor_subject_task_id(subject).into_iter().collect()
    }

    fn task_is_linked(&self, subject: &TodayActionSubject, task: &TaskListItemV3) -> bool {
        monitor_subject_task_id(subject).is_some_and(|task_id| task_id == task.id)
    }

    fn plan_action(
        &self,
        action_id: &str,
        _subject: &TodayActionSubject,
    ) -> Result<TodayActionPlan> {
        // `open_task` is client-side navigation to an EXISTING task — there
        // is nothing to execute server-side, and monitors must never mint a
        // second task from an update card.
        Err(anyhow!(
            "monitor Today action `{action_id}` is navigation-only; the monitor task already exists"
        ))
    }
}

fn monitor_subject_task_id(subject: &TodayActionSubject) -> Option<String> {
    metadata_string(&subject.metadata, "monitor_task_id")
        .filter(|task_id| task_id.starts_with("task_"))
}

fn meeting_action_task_id(source_id: &str) -> String {
    let digest = blake3::hash(source_id.trim().as_bytes()).to_hex();
    format!("task_meeting_action_{}", &digest[..32])
}

fn metadata_string(value: &Value, key: &str) -> Option<String> {
    value
        .as_object()
        .and_then(|record| record.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn bounded(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

fn normalized_action_text(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("Meeting action: ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn subject() -> TodayActionSubject {
        TodayActionSubject {
            item_id: "today:followups:meeting_action:weekly".to_string(),
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            source_kind: "meeting_action".to_string(),
            source_id: "meeting:weekly:action:0".to_string(),
            source_url: Some("/meetings/weekly".to_string()),
            thread_id: Some("meeting-weekly".to_string()),
            title: "Meeting action: Send launch notes".to_string(),
            summary: Some("From Weekly Review".to_string()),
            metadata: json!({
                "action_item": "Send launch notes",
                "meeting_title": "Weekly Review",
                "meeting_date": "2026-07-15"
            }),
        }
    }

    #[test]
    fn meeting_adapter_advertises_generic_action_contract() {
        let subject = subject();
        let adapter = today_action_adapter(&subject.source_kind).expect("meeting adapter");
        assert_eq!(adapter.source_kind(), "meeting_action");
        let actions = adapter.available_actions(&subject);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].id, CREATE_TASK_ACTION_ID);
        assert_eq!(
            actions[0].action_type.as_deref(),
            Some("today_source_action")
        );
        assert_eq!(actions[0].payload["method"], json!("POST"));
        assert_eq!(actions[0].payload["result_navigation"], json!("task"));
    }

    #[test]
    fn meeting_task_plan_is_stable_and_carries_source_metadata() {
        let subject = subject();
        let adapter = today_action_adapter(&subject.source_kind).expect("meeting adapter");
        let first = adapter.linked_task_ids(&subject);
        let second = adapter.linked_task_ids(&subject);
        assert_eq!(first, second);
        assert_eq!(first.len(), 1);
        assert!(first[0].starts_with("task_meeting_action_"));

        let TodayActionPlan::EnsureTask(plan) = adapter
            .plan_action(CREATE_TASK_ACTION_ID, &subject)
            .expect("task plan")
        else {
            panic!("meeting adapter must plan a task ensure");
        };
        assert_eq!(plan.task_id, first[0]);
        assert_eq!(plan.input.title, "Send launch notes");
        assert_eq!(plan.input.ui_thread_id, "meeting-weekly");
        assert!(plan.input.description.contains("Meeting: Weekly Review"));
        assert!(plan.input.description.contains("Meeting date: 2026-07-15"));
        assert!(plan
            .input
            .description
            .contains("Meeting page: /meetings/weekly"));
        assert!(plan
            .input
            .description
            .contains("Meeting action source: meeting:weekly:action:0"));
    }

    #[test]
    fn meeting_adapter_respects_explicit_task_linkage() {
        let mut subject = subject();
        subject.metadata["linked_task_id"] = json!("task_existing");
        let adapter = today_action_adapter(&subject.source_kind).expect("meeting adapter");
        let ids = adapter.linked_task_ids(&subject);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"task_existing".to_string()));
    }

    fn thinking_map_subject() -> TodayActionSubject {
        TodayActionSubject {
            item_id: "today:followups:thinking_map_action:tm_map1_node_n1".to_string(),
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            source_kind: THINKING_MAP_ACTION_SOURCE_KIND.to_string(),
            source_id: "thinking_map:map1:node:n1".to_string(),
            source_url: Some("/thinking-maps/map1?node=n1".to_string()),
            thread_id: Some("thinking-map-map1".to_string()),
            title: "Map action: Ship the launch checklist".to_string(),
            summary: Some("From Launch planning".to_string()),
            metadata: json!({
                "map_id": "map1",
                "node_id": "n1",
                "node_label": "Ship the launch checklist",
                "map_title": "Launch planning"
            }),
        }
    }

    #[test]
    fn thinking_map_adapter_advertises_generic_action_contract() {
        let subject = thinking_map_subject();
        let adapter = today_action_adapter(&subject.source_kind).expect("thinking map adapter");
        assert_eq!(adapter.source_kind(), THINKING_MAP_ACTION_SOURCE_KIND);
        let actions = adapter.available_actions(&subject);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].id, CREATE_TASK_ACTION_ID);
        assert_eq!(
            actions[0].action_type.as_deref(),
            Some("today_source_action")
        );
        assert_eq!(actions[0].payload["method"], json!("POST"));
        assert_eq!(
            actions[0].payload["endpoint"],
            json!(format!(
                "/api/magician/v2/today/items/{}/actions/create_task",
                subject.item_id
            ))
        );
        assert_eq!(actions[0].payload["result_navigation"], json!("task"));
    }

    #[test]
    fn thinking_map_plan_routes_through_governed_promotion() {
        let subject = thinking_map_subject();
        let adapter = today_action_adapter(&subject.source_kind).expect("thinking map adapter");
        let TodayActionPlan::PromoteThinkingMapNode(plan) = adapter
            .plan_action(CREATE_TASK_ACTION_ID, &subject)
            .expect("promotion plan")
        else {
            panic!("thinking-map adapter must plan a governed promotion, not a bespoke task");
        };
        assert_eq!(plan.map_id, "map1");
        assert_eq!(plan.node_id, "n1");

        assert!(adapter.plan_action("open_map", &subject).is_err());
        let mut missing = thinking_map_subject();
        missing.metadata = json!({});
        assert!(adapter
            .plan_action(CREATE_TASK_ACTION_ID, &missing)
            .is_err());
    }

    #[test]
    fn thinking_map_adapter_matches_linked_id_marker_or_same_thread_title() {
        let subject = thinking_map_subject();
        let adapter = today_action_adapter(&subject.source_kind).expect("thinking map adapter");
        let task = |id: &str, title: &str, description: &str, thread_id: &str| {
            serde_json::from_value::<TaskListItemV3>(json!({
                "id": id,
                "title": title,
                "description": description,
                "status": "pending",
                "agent_id": "personal-assistant",
                "ui_thread_id": thread_id,
                "created_at": "2026-07-22T10:00:00Z",
                "updated_at": "2026-07-22T10:00:00Z"
            }))
            .expect("task fixture")
        };

        // Explicit promoted-ref linkage via metadata.
        let mut linked = thinking_map_subject();
        linked.metadata["linked_task_id"] = json!("task_promoted");
        assert_eq!(adapter.linked_task_ids(&linked), vec!["task_promoted"]);
        assert!(adapter.task_is_linked(
            &linked,
            &task("task_promoted", "Renamed later", "", "elsewhere")
        ));

        // Durable provenance marker in the task description.
        assert!(adapter.task_is_linked(
            &subject,
            &task(
                "task_provenance",
                "A renamed task",
                "Thinking map node source: map1:n1",
                "another-thread"
            )
        ));

        // Same map thread + same normalized title.
        assert!(adapter.task_is_linked(
            &subject,
            &task(
                "task_thread",
                "Ship the launch checklist",
                "",
                "thinking-map-map1"
            )
        ));
        assert!(!adapter.task_is_linked(
            &subject,
            &task(
                "task_other",
                "Ship the launch checklist",
                "",
                "another-thread"
            )
        ));
    }

    fn monitor_subject() -> TodayActionSubject {
        TodayActionSubject {
            item_id: "today:changed:monitor_update:mu_71d3f6a2c4e89b10".to_string(),
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            source_kind: MONITOR_UPDATE_SOURCE_KIND.to_string(),
            source_id: "mu_71d3f6a2c4e89b10".to_string(),
            source_url: Some(
                "/tasks?type=monitors&selected=task_monitor_1&update=mu_71d3f6a2c4e89b10"
                    .to_string(),
            ),
            thread_id: None,
            title: "Acme pricing: Pro plan +$10/month".to_string(),
            summary: Some("Two material changes on the pricing page.".to_string()),
            metadata: json!({
                "monitor_task_id": "task_monitor_1",
                "update_id": "mu_71d3f6a2c4e89b10",
                "change_fingerprint": "chg_71d3f6a2c4e89b10"
            }),
        }
    }

    #[test]
    fn monitor_adapter_is_registered_and_navigation_only() {
        let subject = monitor_subject();
        let adapter = today_action_adapter(&subject.source_kind).expect("monitor adapter");
        assert_eq!(adapter.source_kind(), MONITOR_UPDATE_SOURCE_KIND);

        // One open-task action, deep-linked to the CANONICAL monitor route
        // (the web reads /tasks?type=monitors&selected=…&update=… via
        // taskRoutes.ts monitorsTaskRoute — never the plain task view).
        let actions = adapter.available_actions(&subject);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].id, OPEN_TASK_ACTION_ID);
        assert_eq!(actions[0].action_type.as_deref(), Some(OPEN_TASK_ACTION_ID));
        assert_eq!(
            actions[0].payload["url"],
            json!("/tasks?type=monitors&selected=task_monitor_1&update=mu_71d3f6a2c4e89b10")
        );
        assert_eq!(actions[0].payload["task_id"], json!("task_monitor_1"));
        assert_eq!(
            actions[0].payload["update_id"],
            json!("mu_71d3f6a2c4e89b10")
        );
        assert_eq!(
            actions[0].payload["change_fingerprint"],
            json!("chg_71d3f6a2c4e89b10")
        );

        // Without a known update id the deep link omits &update=.
        let mut no_update = monitor_subject();
        no_update.metadata = json!({ "monitor_task_id": "task_monitor_1" });
        let actions = adapter.available_actions(&no_update);
        assert_eq!(
            actions[0].payload["url"],
            json!("/tasks?type=monitors&selected=task_monitor_1")
        );
        assert_eq!(actions[0].payload["update_id"], json!(null));

        // The helper is the single deep-link builder (URL-encodes both ids).
        assert_eq!(
            monitor_deep_link("task_monitor_1", Some("mu_x")),
            "/tasks?type=monitors&selected=task_monitor_1&update=mu_x"
        );
        assert_eq!(
            monitor_deep_link("task_monitor_1", Some("  ")),
            "/tasks?type=monitors&selected=task_monitor_1"
        );

        // The linked task is the monitor task itself — never a new task.
        assert_eq!(adapter.linked_task_ids(&subject), vec!["task_monitor_1"]);
        assert!(adapter.plan_action(OPEN_TASK_ACTION_ID, &subject).is_err());
        assert!(adapter
            .plan_action(CREATE_TASK_ACTION_ID, &subject)
            .is_err());

        // Metadata-based reconciliation.
        let task = serde_json::from_value::<TaskListItemV3>(json!({
            "id": "task_monitor_1",
            "title": "Watch the Acme pricing page",
            "description": "",
            "status": "pending",
            "agent_id": "personal-assistant",
            "ui_thread_id": "general",
            "created_at": "2026-07-22T10:00:00Z",
            "updated_at": "2026-07-22T10:00:00Z"
        }))
        .expect("task fixture");
        assert!(adapter.task_is_linked(&subject, &task));
        let mut other = task.clone();
        other.id = "task_other".to_string();
        assert!(!adapter.task_is_linked(&subject, &other));

        // Malformed metadata degrades to no actions rather than bad links.
        let mut missing = monitor_subject();
        missing.metadata = json!({});
        assert!(adapter.available_actions(&missing).is_empty());
        assert!(adapter.linked_task_ids(&missing).is_empty());
    }

    #[test]
    fn meeting_adapter_matches_only_durable_provenance_or_exact_same_thread_title() {
        let subject = subject();
        let adapter = today_action_adapter(&subject.source_kind).expect("meeting adapter");
        let task = |id: &str, title: &str, description: &str, thread_id: &str| {
            serde_json::from_value::<TaskListItemV3>(json!({
                "id": id,
                "title": title,
                "description": description,
                "status": "pending",
                "agent_id": "personal-assistant",
                "ui_thread_id": thread_id,
                "created_at": "2026-07-15T10:00:00Z",
                "updated_at": "2026-07-15T10:00:00Z"
            }))
            .expect("task fixture")
        };

        assert!(adapter.task_is_linked(
            &subject,
            &task("task_external", "Send launch notes", "", "meeting-weekly")
        ));
        assert!(!adapter.task_is_linked(
            &subject,
            &task("task_other", "Send launch notes", "", "another-thread")
        ));
        assert!(adapter.task_is_linked(
            &subject,
            &task(
                "task_provenance",
                "A renamed task",
                "Meeting action source: meeting:weekly:action:0",
                "another-thread"
            )
        ));
    }
}
