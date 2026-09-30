//! Crew listing/projection shaping — the surface half of the
//! `/api/magician/v2/agents` CRUD family (plan workstream 3.5).
//!
//! Everything here is pure response shaping extracted verbatim from
//! `web_api`: the record/list wire shapes, the system-agent directory the
//! listing appends, the pagination policy, and the live-task selection the
//! hydrated runtime-state projection reports. The handlers keep the HTTP
//! mapping (ETag, 304, error envelopes) and the async hydration fan-out.

use chrono::{DateTime, Utc};
use magician::magician_v2::agents::{AgentDefinition, DefinitionRecord};
use serde::{Deserialize, Serialize};

/// Maximum accepted `limit` for the crew listing endpoint.
pub const AGENT_LIST_LIMIT_MAX: usize = 500;

/// Query contract for `GET /api/magician/v2/agents`.
#[derive(Debug, Clone, Deserialize)]
pub struct ListAgentDefinitionsQuery {
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One crew record as the CRUD surface serves it: the definition plus its
/// optimistic-concurrency version/etag and the hydrated runtime projection.
#[derive(Debug, Clone, Serialize)]
pub struct AgentDefinitionRecordResponse {
    pub definition: AgentDefinition,
    pub version: u32,
    pub etag: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_goal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_cycle_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_execution_id: Option<String>,
}

impl From<DefinitionRecord> for AgentDefinitionRecordResponse {
    fn from(value: DefinitionRecord) -> Self {
        let version = value.definition.version;
        let etag = value.etag();
        Self {
            definition: value.definition,
            version,
            etag,
            created_at: value.created_at,
            updated_at: value.updated_at,
            status: None,
            current_goal_id: None,
            current_cycle_id: None,
            current_execution_id: None,
        }
    }
}

/// The crew listing envelope: hydrated agent records, the system-agent
/// directory the UI renders beside them, and the pre-pagination total.
#[derive(Debug, Clone, Serialize)]
pub struct AgentDefinitionListResponse {
    pub agents: Vec<AgentDefinitionRecordResponse>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub system_agents: Vec<SystemAgentDescriptor>,
    pub total_count: usize,
}

/// A pipeline-stage agent that has no user-visible definition record; the
/// listing surfaces it as directory metadata only.
#[derive(Debug, Clone, Serialize)]
pub struct SystemAgentDescriptor {
    pub agent_id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

pub const SYSTEM_AGENT_DIRECTORY: &[SystemAgentDescriptor] = &[
    SystemAgentDescriptor {
        agent_id: "system:intent-classifier",
        name: "Magic Intent Classifier",
        description: "Classifies user intent (new task, slot answer, status query, cancellation, \
                      etc.) and produces QueryAnalysis + IntentClassification artifacts. First \
                      stage of the pipeline.",
    },
    SystemAgentDescriptor {
        agent_id: "system:planner",
        name: "Magic Planner",
        description: "Builds execution-oriented plans from normalized user intent and available \
                      artifacts.",
    },
    SystemAgentDescriptor {
        agent_id: "system:answer-interpreter",
        name: "Magic Answer Interpreter",
        description: "Converts conversational answers into planner-ready artifacts.",
    },
    SystemAgentDescriptor {
        agent_id: "system:elicitor",
        name: "Magic Elicitor",
        description: "Collects missing context through focused user clarification prompts.",
    },
    SystemAgentDescriptor {
        agent_id: "system:slot-extractor",
        name: "Magic Slot Extractor",
        description: "Identifies and materialises required inputs for planning.",
    },
    SystemAgentDescriptor {
        agent_id: "system:query-rewriter",
        name: "Magic Query Rewriter",
        description: "Reframes raw user requests into clearer, planner-compatible task statements.",
    },
];

/// Whether an agent id belongs to the pipeline-only system directory (and so
/// never appears as a crew member record).
pub fn is_system_agent_id(agent_id: &str) -> bool {
    SYSTEM_AGENT_DIRECTORY
        .iter()
        .any(|descriptor| descriptor.agent_id == agent_id)
}

/// The rejected pagination request: the limit was `0` or above
/// [`AGENT_LIST_LIMIT_MAX`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidListPaginationLimit {
    pub max_limit: usize,
}

/// Normalize the listing query's pagination, rejecting a `limit` of `0` or
/// above [`AGENT_LIST_LIMIT_MAX`]. The handler maps the rejection onto the
/// `invalid_pagination_limit` 400 envelope.
pub fn validate_agent_list_pagination(
    query: Option<&ListAgentDefinitionsQuery>,
) -> Result<(usize, Option<usize>), InvalidListPaginationLimit> {
    let offset = query.and_then(|q| q.offset).unwrap_or(0);
    let limit = query.and_then(|q| q.limit);
    if limit.is_some_and(|v| v == 0 || v > AGENT_LIST_LIMIT_MAX) {
        return Err(InvalidListPaginationLimit {
            max_limit: AGENT_LIST_LIMIT_MAX,
        });
    }
    Ok((offset, limit))
}

/// Apply the normalized pagination to a fully hydrated listing response.
/// `total_count` reflects the full set before pagination.
pub fn apply_agent_list_pagination(
    mut response: AgentDefinitionListResponse,
    offset: usize,
    limit: Option<usize>,
) -> AgentDefinitionListResponse {
    response.total_count = response.agents.len();
    if offset > 0 {
        response.agents = response.agents.into_iter().skip(offset).collect();
    }
    if let Some(limit) = limit {
        response.agents.truncate(limit);
    }
    response
}

/// The runtime status a live task reports for the projection: only tasks
/// with an active root execution count, mapped to `running`/`paused`.
pub fn live_task_runtime_status(
    task: &magician::magician_v2::artifact_v2::models::TaskListItemV3,
) -> Option<&'static str> {
    task.active_root_execution_id.as_ref()?;

    match task.status.as_str() {
        "planning" | "running" => Some("running"),
        "paused" => Some("paused"),
        _ => None,
    }
}

fn is_task_more_recent(
    candidate: &magician::magician_v2::artifact_v2::models::TaskListItemV3,
    current: &magician::magician_v2::artifact_v2::models::TaskListItemV3,
) -> bool {
    candidate.updated_at > current.updated_at
        || (candidate.updated_at == current.updated_at && candidate.id > current.id)
}

/// The most recent live (running preferred over paused) task for one agent,
/// used to report the hydrated `status`/`current_execution_id` projection.
pub fn most_recent_live_task_for_agent<'a>(
    tasks: &'a [magician::magician_v2::artifact_v2::models::TaskListItemV3],
    agent_id: &str,
) -> Option<(
    &'static str,
    &'a magician::magician_v2::artifact_v2::models::TaskListItemV3,
)> {
    let mut latest_running = None;
    let mut latest_paused = None;

    for task in tasks.iter().filter(|task| task.agent_id == agent_id) {
        match live_task_runtime_status(task) {
            Some("running") => {
                if latest_running.is_none_or(|current| is_task_more_recent(task, current)) {
                    latest_running = Some(task);
                }
            },
            Some("paused") => {
                if latest_paused.is_none_or(|current| is_task_more_recent(task, current)) {
                    latest_paused = Some(task);
                }
            },
            _ => {},
        }
    }

    latest_running
        .map(|task| ("running", task))
        .or_else(|| latest_paused.map(|task| ("paused", task)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_accepts_absent_and_bounded_limits() {
        assert_eq!(validate_agent_list_pagination(None), Ok((0, None)));
        let query = ListAgentDefinitionsQuery {
            offset: Some(3),
            limit: Some(AGENT_LIST_LIMIT_MAX),
        };
        assert_eq!(
            validate_agent_list_pagination(Some(&query)),
            Ok((3, Some(AGENT_LIST_LIMIT_MAX)))
        );
    }

    #[test]
    fn pagination_rejects_zero_and_over_max_limits() {
        for limit in [0, AGENT_LIST_LIMIT_MAX + 1] {
            let query = ListAgentDefinitionsQuery {
                offset: None,
                limit: Some(limit),
            };
            assert_eq!(
                validate_agent_list_pagination(Some(&query)),
                Err(InvalidListPaginationLimit {
                    max_limit: AGENT_LIST_LIMIT_MAX
                })
            );
        }
    }

    #[test]
    fn pagination_truncates_but_reports_the_full_total() {
        let response = AgentDefinitionListResponse {
            agents: (0..5)
                .map(|_| AgentDefinitionRecordResponse::from(sample_record("a")))
                .collect(),
            system_agents: SYSTEM_AGENT_DIRECTORY.to_vec(),
            total_count: 0,
        };
        let paged = apply_agent_list_pagination(response, 1, Some(2));
        assert_eq!(paged.total_count, 5);
        assert_eq!(paged.agents.len(), 2);
    }

    #[test]
    fn system_directory_ids_are_excluded_from_crew_membership() {
        assert!(is_system_agent_id("system:planner"));
        assert!(!is_system_agent_id("agent-a"));
        // Every directory entry is itself recognized.
        for descriptor in SYSTEM_AGENT_DIRECTORY {
            assert!(is_system_agent_id(descriptor.agent_id));
        }
    }

    fn sample_record(agent_id: &str) -> DefinitionRecord {
        let yaml = format!(
            r#"
agent_id: "{agent_id}"
name: "Sample"
persona: "Test persona"
tools: []
"#
        );
        DefinitionRecord {
            definition: AgentDefinition::from_yaml_str(&yaml).expect("definition should parse"),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn record_response_projects_version_and_etag_from_the_definition() {
        let response = AgentDefinitionRecordResponse::from(sample_record("agent-a"));
        assert_eq!(response.definition.agent_id, "agent-a");
        assert_eq!(response.version, response.definition.version);
        assert!(!response.etag.is_empty());
        assert!(response.status.is_none());
        assert!(response.current_execution_id.is_none());
    }
}
