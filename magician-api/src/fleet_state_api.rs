//! Scoped Town Square read model.
//!
//! `GET /api/magician/v2/fleet-state` composes existing authoritative stores
//! into one timestamped snapshot. Each section carries availability metadata;
//! an unavailable or intentionally partial source is never represented as an
//! authoritative empty result.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{SecondsFormat, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::scope::resolve_required_scope;
use magician::magician_v2::agents::{
    definition_store::{AgentDefinitionStore, DefinitionRecord},
    types::AgentKind,
};
use magician::magician_v2::artifact_v2::{
    models::{ExecutionTreeRecord, TaskListItemV3},
    workspace::ArtifactV2Workspace,
    ArtifactV2Service, ScopeRef, V3ReadApi,
};
use magician::magician_v2::feed::{
    FeedAction, FeedItem, FeedItemStatus, FeedItemType, FeedQuery, FeedStore,
};
use magician::magician_v2::harness::program_doc::{
    list_program_docs, read_program_doc, ProgramDocSummary,
};
use magician::magician_v2::resource_authority::{
    gate::SystemFreezeState, scoped_authority::ScopedAuthorityResolver,
};
use magician::magician_v2::social::types::Post;

const FLEET_STATE_SCHEMA_VERSION: &str = "fleet_state.v1alpha1";

#[derive(Debug, Deserialize)]
pub struct FleetStateScopeQuery {
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FleetSectionStatus {
    Available,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FleetSectionAvailability {
    pub status: FleetSectionStatus,
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

impl FleetSectionAvailability {
    fn available(source: &str) -> Self {
        Self {
            status: FleetSectionStatus::Available,
            sources: vec![source.to_string()],
            limitations: Vec::new(),
        }
    }

    fn partial(sources: &[&str], limitations: Vec<String>) -> Self {
        Self {
            status: FleetSectionStatus::Partial,
            sources: sources.iter().map(|source| (*source).to_string()).collect(),
            limitations,
        }
    }

    fn unavailable(source: &str, limitation: &str) -> Self {
        Self {
            status: FleetSectionStatus::Unavailable,
            sources: vec![source.to_string()],
            limitations: vec![limitation.to_string()],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetAvailability {
    pub citizens: FleetSectionAvailability,
    pub guilds: FleetSectionAvailability,
    pub quests: FleetSectionAvailability,
    pub attention: FleetSectionAvailability,
    pub handoffs: FleetSectionAvailability,
    pub deliveries: FleetSectionAvailability,
    pub economy: FleetSectionAvailability,
    pub social: FleetSectionAvailability,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetScope {
    pub principal: String,
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CitizenCurrentWork {
    pub quest_id: String,
    pub title: String,
    pub status: String,
    pub execution_id: Option<String>,
    pub current_step: Option<String>,
    pub current_substep: Option<String>,
    pub is_blocked: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FleetCitizen {
    pub citizen_id: String,
    pub display_name: String,
    pub aliases: Vec<String>,
    pub role: AgentKind,
    pub description: String,
    pub version: u32,
    pub disabled: bool,
    pub is_primary: bool,
    /// Exact program references declared by autonomous focus areas.
    pub program_refs: Vec<String>,
    pub current_work: Vec<CitizenCurrentWork>,
    pub social_mood_valence: Option<f64>,
    pub social_mood_energy: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FleetGuild {
    pub program_id: String,
    pub title: String,
    /// The authoritative managed mission section, when the document has one.
    pub missions_markdown: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FleetAttentionAction {
    pub id: String,
    pub label: String,
    /// Open action discriminator from the authoritative feed row.
    #[serde(rename = "type")]
    pub action_type: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FleetAttentionItem {
    pub id: String,
    pub kind: FeedItemType,
    pub title: String,
    pub summary: Option<String>,
    pub task_id: Option<String>,
    pub citizen_id: Option<String>,
    pub actions: Vec<FleetAttentionAction>,
    /// Legacy feed actions without an explicit type are counted, not guessed.
    pub untyped_action_count: usize,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FleetCitizenIdentity {
    pub citizen_id: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FleetHandoff {
    pub quest_id: String,
    pub parent_execution_id: String,
    pub child_execution_id: Option<String>,
    pub parent_step_id: String,
    pub from: FleetCitizenIdentity,
    pub to: FleetCitizenIdentity,
    pub reason: String,
    pub status: String,
    pub active: bool,
    pub outcome_type: Option<String>,
    pub requested_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FleetDelivery {
    pub id: String,
    pub kind: FeedItemType,
    pub title: String,
    pub summary: Option<String>,
    pub status: FeedItemStatus,
    pub task_id: Option<String>,
    pub citizen_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FleetEconomyAccount {
    pub account_id: String,
    pub commodity: String,
    pub balance: Decimal,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetEconomy {
    pub enabled: bool,
    pub freeze: SystemFreezeState,
    pub accounts: Vec<FleetEconomyAccount>,
    pub active_reservation_count: usize,
    pub journal_entry_count: usize,
    pub spend_token_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetStateResponse {
    pub schema_version: String,
    pub generated_at: String,
    pub scope: FleetScope,
    pub availability: FleetAvailability,
    pub citizens: Vec<FleetCitizen>,
    pub guilds: Vec<FleetGuild>,
    /// Canonical v3 task list rows. Keeping the rich task shape avoids a lossy
    /// second task-state vocabulary in the first projection.
    pub quests: Vec<TaskListItemV3>,
    pub attention: Vec<FleetAttentionItem>,
    pub handoffs: Vec<FleetHandoff>,
    pub deliveries: Vec<FleetDelivery>,
    pub economy: Option<FleetEconomy>,
    pub ambient_social_activity: Vec<FleetAmbientSocialPost>,
}

/// Public-feed row the Floor can treat as a proved talk event.
///
/// `post_id` is the stable identity. `parent_id` is set on a reply so a thread
/// keeps one venue; roots serialize it as JSON `null`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FleetAmbientSocialPost {
    pub post_id: String,
    pub parent_id: Option<String>,
    pub author_id: String,
    pub display_name: String,
    pub recent_activity: String,
    pub created_at: String,
}

/// How many square posts the fleet view carries. Matches what the retired
/// projection took, so the payload's shape does not change with its source.
const AMBIENT_SQUARE_POSTS: usize = 10;

fn project_ambient_social_post(post: &Post, display_name: String) -> FleetAmbientSocialPost {
    FleetAmbientSocialPost {
        post_id: post.post_id.clone(),
        parent_id: post.parent_id.clone(),
        author_id: post.author_id.clone(),
        display_name,
        recent_activity: post.body.chars().take(50).collect::<String>(),
        created_at: post.created_at.clone(),
    }
}

#[derive(Clone)]
pub struct FleetStateApi {
    workspace: ArtifactV2Workspace,
    definition_store: Option<Arc<AgentDefinitionStore>>,
    task_service: Option<Arc<ArtifactV2Service>>,
    feed_store: Option<FeedStore>,
    authority_resolver: Option<Arc<dyn ScopedAuthorityResolver>>,
    /// The Town Square package's reader. Since the engine retirement the
    /// package's entity store owns the corpus; the old `SocialStoreRegistry`
    /// still had rows, so reading it did not fail — it reported the square
    /// frozen at the moment of retirement as `available`.
    square: Option<crate::social_api::SocialApi>,
}

impl FleetStateApi {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self {
            workspace,
            definition_store: None,
            task_service: None,
            feed_store: None,
            authority_resolver: None,
            square: None,
        }
    }

    pub fn with_definition_store(mut self, definition_store: Arc<AgentDefinitionStore>) -> Self {
        self.definition_store = Some(definition_store);
        self
    }

    pub fn with_task_service(mut self, task_service: Arc<ArtifactV2Service>) -> Self {
        self.task_service = Some(task_service);
        self
    }

    pub fn with_feed_store(mut self, feed_store: FeedStore) -> Self {
        self.feed_store = Some(feed_store);
        self
    }

    pub fn with_authority_resolver(
        mut self,
        authority_resolver: Arc<dyn ScopedAuthorityResolver>,
    ) -> Self {
        self.authority_resolver = Some(authority_resolver);
        self
    }

    pub fn with_square(mut self, square: crate::social_api::SocialApi) -> Self {
        self.square = Some(square);
        self
    }

    /// Project the square through the Town Square package's own reader.
    ///
    /// The fleet view used to read `SocialStoreRegistry` directly. That store
    /// stopped receiving writes when the first-party engine retired, so the
    /// read kept succeeding against a corpus frozen at that moment and the
    /// section reported `available`. A stale answer that looks healthy is
    /// worse than an error, which is why this now goes through the package.
    async fn read_square_projection(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Option<
        Result<crate::social_api::SquareFleetProjection, crate::social_api::SquareProjectionError>,
    > {
        let square = self.square.as_ref()?;
        let now = Utc::now();
        let run_ref = match magician::magician_v2::apps::models::AppReference::parse(format!(
            "run:fleet-state:{}",
            uuid::Uuid::new_v4()
        )) {
            Ok(run_ref) => run_ref,
            Err(error) => {
                tracing::warn!(%error, "[FLEET-STATE] invalid square projection run reference");
                return Some(Err(crate::social_api::SquareProjectionError::Unavailable));
            },
        };
        let authenticated = match crate::apps_api::system_worker_scope_for_actor(
            principal,
            workspace,
            "worker:fleet-state",
            &run_ref,
            now,
        ) {
            Ok(authenticated) => authenticated,
            Err(error) => {
                tracing::warn!(principal, workspace, %error, "[FLEET-STATE] invalid square scope");
                return Some(Err(crate::social_api::SquareProjectionError::Unavailable));
            },
        };
        Some(
            square
                .fleet_projection(authenticated, AMBIENT_SQUARE_POSTS)
                .await,
        )
    }

    pub async fn snapshot(&self, principal: String, workspace: String) -> FleetStateResponse {
        let scope =
            ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());

        // These three inputs are independent scoped reads. Run them together
        // so an absent/cold social database cannot sit serially behind the
        // task and definition stores on every Town Square refresh.
        let (
            (quests, quests_availability),
            (definition_records, mut citizens_availability),
            scoped_social,
        ) = tokio::join!(
            self.read_quests(&scope),
            self.read_citizen_definitions(&principal, &workspace),
            self.read_square_projection(&principal, &workspace)
        );
        let quests_are_available = quests_availability.status != FleetSectionStatus::Unavailable;
        let citizen_names = definition_records
            .iter()
            .map(|record| {
                (
                    record.definition.agent_id.clone(),
                    record.definition.name.clone(),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut citizens = project_citizens(definition_records, &quests);

        // Enrich citizens with the square's moods. One batch map rather than a
        // lookup per citizen.
        if let Some(Ok(projection)) = scoped_social.as_ref() {
            let state_map: HashMap<_, _> = projection
                .states
                .iter()
                .map(|state| (state.member_id.clone(), state))
                .collect();
            for citizen in &mut citizens {
                if let Some(state) = state_map.get(&citizen.citizen_id) {
                    citizen.social_mood_valence = Some(state.valence);
                    citizen.social_mood_energy = Some(state.energy);
                }
            }
        }

        if citizens_availability.status == FleetSectionStatus::Available && !quests_are_available {
            citizens_availability = FleetSectionAvailability::partial(
                &["agent_definition_store"],
                vec!["current_work_unavailable".to_string()],
            );
        }

        let (
            (guilds, guilds_availability),
            (attention, attention_availability),
            (handoffs, handoffs_availability),
            (deliveries, deliveries_availability),
            (economy, economy_availability),
        ) = tokio::join!(
            self.read_guilds(&principal, &workspace),
            self.read_attention(&principal, &workspace),
            self.read_handoffs(&scope, &quests, &citizen_names, quests_are_available),
            self.read_deliveries(&principal, &workspace),
            self.read_economy(&principal, &workspace)
        );

        let mut ambient_social_activity = Vec::new();
        // The section id stays `social_store` for wire compatibility; the
        // reason codes now name what is actually being read.
        let social_availability = match scoped_social.as_ref() {
            None => {
                FleetSectionAvailability::unavailable("social_store", "town_square_unconfigured")
            },
            Some(Err(crate::social_api::SquareProjectionError::NotInstalled)) => {
                // A deployment that has not enabled the Town Square package is
                // not degraded, so this is reported distinctly from a failure.
                FleetSectionAvailability::unavailable("social_store", "town_square_not_installed")
            },
            Some(Err(crate::social_api::SquareProjectionError::Unavailable)) => {
                tracing::warn!(
                    principal = %scope.principal(),
                    workspace = %scope.workspace(),
                    "[FLEET-STATE] town square projection unavailable"
                );
                FleetSectionAvailability::unavailable("social_store", "town_square_unreadable")
            },
            Some(Ok(projection)) => {
                let members_map = projection
                    .members
                    .iter()
                    .map(|member| (member.member_id.clone(), member))
                    .collect::<HashMap<String, _>>();
                for post in &projection.posts {
                    let display_name = members_map
                        .get(&post.author_id)
                        .map(|member| member.display_name.clone())
                        .unwrap_or_default();
                    ambient_social_activity.push(project_ambient_social_post(post, display_name));
                }
                FleetSectionAvailability::available("social_store")
            },
        };

        FleetStateResponse {
            schema_version: FLEET_STATE_SCHEMA_VERSION.to_string(),
            generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            scope: FleetScope {
                principal,
                workspace,
            },
            availability: FleetAvailability {
                citizens: citizens_availability,
                guilds: guilds_availability,
                quests: quests_availability,
                attention: attention_availability,
                handoffs: handoffs_availability,
                deliveries: deliveries_availability,
                economy: economy_availability,
                social: social_availability,
            },
            citizens,
            guilds,
            quests,
            attention,
            handoffs,
            deliveries,
            economy,
            ambient_social_activity,
        }
    }

    async fn read_quests(
        &self,
        scope: &ScopeRef,
    ) -> (Vec<TaskListItemV3>, FleetSectionAvailability) {
        let Some(service) = self.task_service.as_ref() else {
            return (
                Vec::new(),
                FleetSectionAvailability::unavailable(
                    "artifact_v3_task_index",
                    "service_not_configured",
                ),
            );
        };
        match service.list_tasks(scope).await {
            Ok(quests) => (
                quests,
                FleetSectionAvailability::available("artifact_v3_task_index"),
            ),
            Err(error) => {
                tracing::warn!(
                    principal = %scope.principal(),
                    workspace = %scope.workspace(),
                    error = %error,
                    "[FLEET-STATE] failed to read quests"
                );
                (
                    Vec::new(),
                    FleetSectionAvailability::unavailable(
                        "artifact_v3_task_index",
                        "source_read_failed",
                    ),
                )
            },
        }
    }

    async fn read_citizen_definitions(
        &self,
        principal: &str,
        workspace: &str,
    ) -> (Vec<DefinitionRecord>, FleetSectionAvailability) {
        let Some(store) = self.definition_store.as_ref() else {
            return (
                Vec::new(),
                FleetSectionAvailability::unavailable(
                    "agent_definition_store",
                    "service_not_configured",
                ),
            );
        };
        match store
            .for_scope(principal, workspace)
            .list_definitions()
            .await
        {
            Ok(records) => (
                records,
                FleetSectionAvailability::available("agent_definition_store"),
            ),
            Err(error) => {
                tracing::warn!(
                    principal,
                    workspace,
                    error = %error,
                    "[FLEET-STATE] failed to read citizen definitions"
                );
                (
                    Vec::new(),
                    FleetSectionAvailability::unavailable(
                        "agent_definition_store",
                        "source_read_failed",
                    ),
                )
            },
        }
    }

    async fn read_guilds(
        &self,
        principal: &str,
        workspace: &str,
    ) -> (Vec<FleetGuild>, FleetSectionAvailability) {
        let summaries = match list_program_docs(&self.workspace, principal, workspace).await {
            Ok(summaries) => summaries,
            Err(error) => {
                tracing::warn!(
                    principal,
                    workspace,
                    error = %error,
                    "[FLEET-STATE] failed to list program documents"
                );
                return (
                    Vec::new(),
                    FleetSectionAvailability::unavailable(
                        "program_documents",
                        "source_read_failed",
                    ),
                );
            },
        };

        let mut guilds = Vec::with_capacity(summaries.len());
        let mut unreadable_documents = 0usize;
        for ProgramDocSummary { name, title } in summaries {
            match read_program_doc(&self.workspace, principal, workspace, &name).await {
                Ok(Some(document)) => guilds.push(FleetGuild {
                    program_id: name,
                    title,
                    missions_markdown: document.missions_section,
                }),
                Ok(None) | Err(_) => {
                    unreadable_documents += 1;
                    guilds.push(FleetGuild {
                        program_id: name,
                        title,
                        missions_markdown: None,
                    });
                },
            }
        }

        let availability = if unreadable_documents == 0 {
            FleetSectionAvailability::available("program_documents")
        } else {
            FleetSectionAvailability::partial(
                &["program_documents"],
                vec!["some_program_documents_unreadable".to_string()],
            )
        };
        (guilds, availability)
    }

    async fn read_attention(
        &self,
        principal: &str,
        workspace: &str,
    ) -> (Vec<FleetAttentionItem>, FleetSectionAvailability) {
        let Some(store) = self.feed_store.as_ref() else {
            return (
                Vec::new(),
                FleetSectionAvailability::unavailable("feed_store", "service_not_configured"),
            );
        };
        let items = match store
            .list_items(FeedQuery {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                limit: usize::MAX,
                status: Some(FeedItemStatus::NeedsAction),
                ..Default::default()
            })
            .await
        {
            Ok(items) => items,
            Err(error) => {
                tracing::warn!(
                    principal,
                    workspace,
                    error = %error,
                    "[FLEET-STATE] failed to read attention feed rows"
                );
                return (
                    Vec::new(),
                    FleetSectionAvailability::unavailable("feed_store", "source_read_failed"),
                );
            },
        };

        let (attention, _) = project_attention_items(items);
        (
            attention,
            FleetSectionAvailability::partial(
                &["feed_store"],
                vec![
                    "generated_attention_projections_not_composed".to_string(),
                    "stale_task_references_not_checked".to_string(),
                    "feed_store_query_capped_at_1000".to_string(),
                ],
            ),
        )
    }

    async fn read_handoffs(
        &self,
        scope: &ScopeRef,
        quests: &[TaskListItemV3],
        citizen_names: &HashMap<String, String>,
        quests_are_available: bool,
    ) -> (Vec<FleetHandoff>, FleetSectionAvailability) {
        if !quests_are_available {
            return (
                Vec::new(),
                FleetSectionAvailability::unavailable(
                    "artifact_v3_execution_tree",
                    "quest_source_unavailable",
                ),
            );
        }
        let Some(service) = self.task_service.as_ref() else {
            return (
                Vec::new(),
                FleetSectionAvailability::unavailable(
                    "artifact_v3_execution_tree",
                    "service_not_configured",
                ),
            );
        };

        let mut trees = Vec::new();
        let mut read_failures = 0usize;
        for quest in quests
            .iter()
            .filter(|quest| quest_has_active_execution(quest))
        {
            match service.get_execution_tree(scope, &quest.id).await {
                Ok(tree) => trees.push(tree),
                Err(error) => {
                    read_failures += 1;
                    tracing::warn!(
                        principal = %scope.principal(),
                        workspace = %scope.workspace(),
                        quest_id = %quest.id,
                        error = %error,
                        "[FLEET-STATE] failed to read execution tree"
                    );
                },
            }
        }

        let (handoffs, unresolved_identities) = project_handoffs(&trees, citizen_names);
        let mut limitations = Vec::new();
        if read_failures > 0 {
            limitations.push("some_execution_trees_unreadable".to_string());
        }
        if unresolved_identities > 0 {
            limitations.push("some_child_execution_identities_unresolved".to_string());
        }
        if citizen_names.is_empty() && !handoffs.is_empty() {
            limitations.push("agent_display_identity_unavailable".to_string());
        }
        let availability = if limitations.is_empty() {
            FleetSectionAvailability::available("artifact_v3_execution_tree")
        } else {
            FleetSectionAvailability::partial(&["artifact_v3_execution_tree"], limitations)
        };
        (handoffs, availability)
    }

    async fn read_deliveries(
        &self,
        principal: &str,
        workspace: &str,
    ) -> (Vec<FleetDelivery>, FleetSectionAvailability) {
        let Some(store) = self.feed_store.as_ref() else {
            return (
                Vec::new(),
                FleetSectionAvailability::unavailable("feed_store", "service_not_configured"),
            );
        };
        match store
            .list_items(FeedQuery {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                limit: usize::MAX,
                ..Default::default()
            })
            .await
        {
            Ok(items) => (
                items
                    .into_iter()
                    .filter(|item| {
                        matches!(
                            &item.item_type,
                            FeedItemType::DataDelivery | FeedItemType::RoutineResult
                        )
                    })
                    .map(project_delivery)
                    .collect(),
                FleetSectionAvailability::partial(
                    &["feed_store"],
                    vec!["feed_store_query_capped_at_1000".to_string()],
                ),
            ),
            Err(error) => {
                tracing::warn!(
                    principal,
                    workspace,
                    error = %error,
                    "[FLEET-STATE] failed to read delivery feed rows"
                );
                (
                    Vec::new(),
                    FleetSectionAvailability::unavailable("feed_store", "source_read_failed"),
                )
            },
        }
    }

    async fn read_economy(
        &self,
        principal: &str,
        workspace: &str,
    ) -> (Option<FleetEconomy>, FleetSectionAvailability) {
        let Some(resolver) = self.authority_resolver.as_ref() else {
            return (
                None,
                FleetSectionAvailability::unavailable(
                    "resource_authority",
                    "service_not_configured",
                ),
            );
        };
        let bundle = resolver.resolve_scope(principal, workspace).await;
        let (mut accounts, active_reservation_count, journal_entry_count) = {
            let ledger = bundle.ledger.read().await;
            (
                ledger
                    .accounts
                    .values()
                    .map(|account| FleetEconomyAccount {
                        account_id: account.id.clone(),
                        commodity: account.commodity.clone(),
                        balance: account.cached_balance,
                    })
                    .collect::<Vec<_>>(),
                ledger.active_reservations.len(),
                ledger.journal.len(),
            )
        };
        accounts.sort_by(|left, right| left.account_id.cmp(&right.account_id));
        let spend_token_count = bundle.token_store.read().await.tokens.len();
        let freeze = bundle.system_freeze.read().await.clone();
        (
            Some(FleetEconomy {
                enabled: bundle.config.enabled,
                freeze,
                accounts,
                active_reservation_count,
                journal_entry_count,
                spend_token_count,
            }),
            FleetSectionAvailability::available("resource_authority"),
        )
    }
}

fn project_citizens(
    records: Vec<DefinitionRecord>,
    quests: &[TaskListItemV3],
) -> Vec<FleetCitizen> {
    let mut current_work_by_agent: HashMap<String, Vec<CitizenCurrentWork>> = HashMap::new();
    for quest in quests.iter().filter(|quest| quest_is_current_work(quest)) {
        current_work_by_agent
            .entry(quest.agent_id.clone())
            .or_default()
            .push(CitizenCurrentWork {
                quest_id: quest.id.clone(),
                title: quest.title.clone(),
                status: quest.status.clone(),
                execution_id: quest.active_root_execution_id.clone(),
                current_step: quest.current_step_title.clone(),
                current_substep: quest.current_substep_title.clone(),
                is_blocked: quest_needs_attention(quest),
                updated_at: quest.updated_at.clone(),
            });
    }
    for current_work in current_work_by_agent.values_mut() {
        current_work.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    }

    let mut citizens = records
        .into_iter()
        .map(|record| {
            let definition = record.definition;
            let mut program_refs = definition
                .autonomous_config
                .as_ref()
                .map(|config| {
                    config
                        .focus_areas
                        .iter()
                        .filter_map(|focus| focus.program.as_deref())
                        .map(str::trim)
                        .filter(|program| !program.is_empty())
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            program_refs.sort();
            program_refs.dedup();
            let current_work = current_work_by_agent
                .remove(&definition.agent_id)
                .unwrap_or_default();
            FleetCitizen {
                citizen_id: definition.agent_id,
                display_name: definition.name,
                aliases: definition.aliases,
                role: definition.kind,
                description: definition.description,
                version: definition.version,
                disabled: definition.disabled,
                is_primary: definition.is_primary,
                program_refs,
                current_work,
                social_mood_valence: None,
                social_mood_energy: None,
            }
        })
        .collect::<Vec<_>>();
    citizens.sort_by(|left, right| left.citizen_id.cmp(&right.citizen_id));
    citizens
}

fn quest_is_current_work(quest: &TaskListItemV3) -> bool {
    if is_terminal_status(&quest.status) {
        return false;
    }
    if quest.active_root_execution_id.is_some() || quest_needs_attention(quest) {
        return true;
    }
    matches!(
        quest.status.trim().to_ascii_lowercase().as_str(),
        "planning"
            | "running"
            | "in_progress"
            | "active"
            | "paused"
            | "waiting"
            | "waiting_for_user"
            | "blocked"
            | "delivering"
            | "synthesizing"
    )
}

fn quest_needs_attention(quest: &TaskListItemV3) -> bool {
    if quest.is_blocked || quest.pending_question.is_some() || !quest.pending_questions.is_empty() {
        return true;
    }
    matches!(
        quest.status.trim().to_ascii_lowercase().as_str(),
        "waiting" | "waiting_for_user" | "blocked"
    )
}

fn quest_has_active_execution(quest: &TaskListItemV3) -> bool {
    quest.active_root_execution_id.is_some()
}

fn project_attention_items(items: Vec<FeedItem>) -> (Vec<FleetAttentionItem>, usize) {
    let mut omitted_untyped_actions = 0usize;
    let attention = items
        .into_iter()
        .map(|item| {
            let (actions, omitted) = project_attention_actions(item.actions);
            omitted_untyped_actions += omitted;
            FleetAttentionItem {
                id: item.id,
                kind: item.item_type,
                title: item.title,
                summary: item.summary,
                task_id: item.task_id,
                citizen_id: item.agent_id,
                actions,
                untyped_action_count: omitted,
                created_at: item.created_at,
                updated_at: item.updated_at,
            }
        })
        .collect();
    (attention, omitted_untyped_actions)
}

fn project_attention_actions(actions: Vec<FeedAction>) -> (Vec<FleetAttentionAction>, usize) {
    let mut omitted = 0usize;
    let actions = actions
        .into_iter()
        .filter_map(|action| {
            let action_type = action
                .action_type
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            match action_type {
                Some(action_type) => Some(FleetAttentionAction {
                    id: action.id,
                    label: action.label,
                    action_type: action_type.to_string(),
                    payload: action.payload,
                }),
                None => {
                    omitted += 1;
                    None
                },
            }
        })
        .collect();
    (actions, omitted)
}

fn project_handoffs(
    trees: &[ExecutionTreeRecord],
    citizen_names: &HashMap<String, String>,
) -> (Vec<FleetHandoff>, usize) {
    let mut handoffs = Vec::new();
    let mut unresolved_identities = 0usize;
    let mut seen = HashSet::new();

    for tree in trees {
        let node_agents = tree
            .nodes
            .iter()
            .map(|node| (node.execution_id.clone(), node.agent_id.clone()))
            .collect::<HashMap<_, _>>();
        for parent in &tree.nodes {
            for delegation in &parent.delegation_summary.delegations {
                let to_agent_id = delegation.child_agent_id.clone().or_else(|| {
                    delegation
                        .child_execution_id
                        .as_ref()
                        .and_then(|execution_id| node_agents.get(execution_id).cloned())
                });
                let Some(to_agent_id) = to_agent_id else {
                    unresolved_identities += 1;
                    continue;
                };
                let dedupe_key = (
                    tree.task_id.clone(),
                    parent.execution_id.clone(),
                    delegation.parent_step_id.clone(),
                    delegation.child_execution_id.clone(),
                    delegation.requested_at.clone(),
                );
                if !seen.insert(dedupe_key) {
                    continue;
                }
                handoffs.push(FleetHandoff {
                    quest_id: tree.task_id.clone(),
                    parent_execution_id: parent.execution_id.clone(),
                    child_execution_id: delegation.child_execution_id.clone(),
                    parent_step_id: delegation.parent_step_id.clone(),
                    from: citizen_identity(&parent.agent_id, citizen_names),
                    to: citizen_identity(&to_agent_id, citizen_names),
                    reason: delegation.sub_goal.clone(),
                    status: delegation.status.clone(),
                    active: !is_terminal_status(&delegation.status),
                    outcome_type: delegation.outcome_type.clone(),
                    requested_at: delegation.requested_at.clone(),
                    updated_at: delegation.updated_at.clone(),
                });
            }
        }
    }
    handoffs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    (handoffs, unresolved_identities)
}

fn citizen_identity(
    citizen_id: &str,
    citizen_names: &HashMap<String, String>,
) -> FleetCitizenIdentity {
    FleetCitizenIdentity {
        citizen_id: citizen_id.to_string(),
        display_name: citizen_names.get(citizen_id).cloned(),
    }
}

fn is_terminal_status(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed" | "succeeded" | "failed" | "cancelled" | "canceled"
    )
}

fn project_delivery(item: FeedItem) -> FleetDelivery {
    FleetDelivery {
        id: item.id,
        kind: item.item_type,
        title: item.title,
        summary: item.summary,
        status: item.status,
        task_id: item.task_id,
        citizen_id: item.agent_id,
        created_at: item.created_at,
        updated_at: item.updated_at,
        metadata: item.metadata,
    }
}

pub async fn get_fleet_state_handler(
    api: web::Data<Arc<FleetStateApi>>,
    req: HttpRequest,
    query: web::Query<FleetStateScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    HttpResponse::Ok().json(api.snapshot(principal, workspace).await)
}

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/fleet-state", web::get().to(get_fleet_state_handler));
}

#[cfg(test)]
mod tests {
    use actix_web::{http::StatusCode, test as actix_test, App};
    use chrono::Utc;
    use magician::magician_v2::agents::definition_store::DefinitionRecord;
    use magician::magician_v2::artifact_v2::models::TaskListItemV3;

    use serde_json::json;
    use serde_json::Value;

    use super::*;

    use magician::magician_v2::agents::types::AgentDefinition;
    use magician::magician_v2::artifact_v2::models::{
        DelegationReadinessEntry, DelegationReadinessRecord, ExecutionTreeNode,
    };

    fn task(value: Value) -> TaskListItemV3 {
        serde_json::from_value(value).expect("task fixture must deserialize")
    }

    #[test]
    fn ambient_social_projection_keeps_post_and_parent_ids() {
        let post = magician::magician_v2::social::types::Post {
            post_id: "p1".into(),
            author_id: "cto".into(),
            surface: "feed".into(),
            group_id: None,
            post_type: "reply".into(),
            body: "hello there this is more than fifty characters of social chatter body".into(),
            parent_id: Some("root".into()),
            created_at: "2026-08-21T10:00:00Z".into(),
        };
        let row = project_ambient_social_post(&post, "CTO".into());
        assert_eq!(row.post_id, "p1");
        assert_eq!(row.parent_id.as_deref(), Some("root"));
        assert_eq!(row.author_id, "cto");
        assert_eq!(row.display_name, "CTO");
        assert_eq!(row.recent_activity.chars().count(), 50);
        assert_eq!(row.created_at, "2026-08-21T10:00:00Z");
    }

    fn definition_record(agent_id: &str, name: &str) -> DefinitionRecord {
        let definition = AgentDefinition::from_yaml_str(&format!(
            r#"
agent_id: "{agent_id}"
name: "{name}"
persona: "Test persona"
tools: []
"#
        ))
        .expect("definition fixture must deserialize");
        DefinitionRecord {
            definition,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn citizens_expose_stable_identity_and_current_work() {
        let quests = vec![
            task(json!({
                "id": "quest-active",
                "title": "Ship projection",
                "description": "",
                "status": "running",
                "agent_id": "agent-a",
                "ui_thread_id": "general",
                "active_root_execution_id": "exec-1",
                "latest_root_execution_id": "exec-1",
                "last_completed_root_execution_id": null,
                "completion_summary": null,
                "completion_outcome": null,
                "created_at": "2026-07-10T08:00:00Z",
                "updated_at": "2026-07-10T09:00:00Z"
            })),
            task(json!({
                "id": "quest-done",
                "title": "Old work",
                "description": "",
                "status": "completed",
                "agent_id": "agent-a",
                "ui_thread_id": "general",
                "active_root_execution_id": "exec-stale",
                "latest_root_execution_id": "exec-0",
                "last_completed_root_execution_id": "exec-0",
                "completion_summary": "done",
                "completion_outcome": "success",
                "created_at": "2026-07-09T08:00:00Z",
                "updated_at": "2026-07-09T09:00:00Z"
            })),
        ];
        let citizens = project_citizens(vec![definition_record("agent-a", "Alpha")], &quests);

        assert_eq!(citizens[0].citizen_id, "agent-a");
        assert_eq!(citizens[0].display_name, "Alpha");
        assert_eq!(citizens[0].current_work.len(), 1);
        assert_eq!(citizens[0].current_work[0].quest_id, "quest-active");
        assert_eq!(
            citizens[0].current_work[0].execution_id.as_deref(),
            Some("exec-1")
        );
    }

    #[test]
    fn citizens_mark_waiting_work_as_blocked() {
        let quests = vec![task(json!({
            "id": "quest-waiting",
            "title": "Await owner input",
            "description": "",
            "status": "waiting_for_user",
            "agent_id": "agent-a",
            "ui_thread_id": "general",
            "active_root_execution_id": "exec-waiting",
            "latest_root_execution_id": "exec-waiting",
            "last_completed_root_execution_id": null,
            "completion_summary": null,
            "completion_outcome": null,
            "created_at": "2026-07-10T08:00:00Z",
            "updated_at": "2026-07-10T09:00:00Z"
        }))];

        let citizens = project_citizens(vec![definition_record("agent-a", "Alpha")], &quests);

        assert_eq!(citizens[0].current_work.len(), 1);
        assert!(citizens[0].current_work[0].is_blocked);
    }

    #[test]
    fn attention_actions_require_an_authoritative_type() {
        let item = FeedItem {
            id: "attention-1".to_string(),
            principal: "alice".to_string(),
            workspace: "main".to_string(),
            item_type: FeedItemType::Approval,
            task_id: Some("quest-1".to_string()),
            ui_thread_id: None,
            agent_id: Some("agent-a".to_string()),
            title: "Approve plan".to_string(),
            summary: None,
            status: FeedItemStatus::NeedsAction,
            created_at: 1,
            updated_at: 2,
            actions: vec![
                FeedAction {
                    id: "approve".to_string(),
                    label: "Approve".to_string(),
                    action_type: Some("approve".to_string()),
                    payload: json!({"approval_id": "approval-1"}),
                },
                FeedAction {
                    id: "legacy".to_string(),
                    label: "Legacy action".to_string(),
                    action_type: None,
                    payload: Value::Null,
                },
            ],
            metadata: Value::Null,
        };

        let (items, omitted) = project_attention_items(vec![item]);
        assert_eq!(omitted, 1);
        assert_eq!(items[0].actions.len(), 1);
        assert_eq!(items[0].actions[0].action_type, "approve");
        assert_eq!(items[0].untyped_action_count, 1);
    }

    #[test]
    fn handoff_tree_reads_require_an_active_root_execution() {
        let completed = task(json!({
            "id": "quest-completed",
            "title": "Completed quest",
            "description": "",
            "status": "completed",
            "agent_id": "agent-a",
            "ui_thread_id": "general",
            "active_root_execution_id": null,
            "latest_root_execution_id": "exec-latest",
            "last_completed_root_execution_id": "exec-latest",
            "completion_summary": "done",
            "completion_outcome": "success",
            "created_at": "2026-07-10T08:00:00Z",
            "updated_at": "2026-07-10T09:00:00Z"
        }));
        let active = task(json!({
            "id": "quest-active",
            "title": "Active quest",
            "description": "",
            "status": "running",
            "agent_id": "agent-a",
            "ui_thread_id": "general",
            "active_root_execution_id": "exec-active",
            "latest_root_execution_id": "exec-active",
            "last_completed_root_execution_id": null,
            "completion_summary": null,
            "completion_outcome": null,
            "created_at": "2026-07-10T08:00:00Z",
            "updated_at": "2026-07-10T09:00:00Z"
        }));

        assert!(!quest_has_active_execution(&completed));
        assert!(quest_has_active_execution(&active));
    }

    fn delegation_summary(
        execution_id: &str,
        task_id: &str,
        delegations: Vec<DelegationReadinessEntry>,
    ) -> DelegationReadinessRecord {
        DelegationReadinessRecord {
            execution_id: execution_id.to_string(),
            task_id: task_id.to_string(),
            execution_status: "running".to_string(),
            waiting_for_children: !delegations.is_empty(),
            active_child_execution_ids: vec!["exec-child".to_string()],
            ready_child_output_ids: Vec::new(),
            result_ready_count: 0,
            waiting_count: delegations.len(),
            blocked_count: 0,
            terminal_without_output_count: 0,
            delegations,
        }
    }

    #[test]
    fn handoffs_resolve_child_identity_from_the_execution_tree() {
        let delegation = DelegationReadinessEntry {
            parent_step_id: "step-1".to_string(),
            sub_goal: "Research the API".to_string(),
            child_execution_id: Some("exec-child".to_string()),
            child_agent_id: None,
            status: "running".to_string(),
            readiness: "waiting".to_string(),
            outcome_type: None,
            output_id: None,
            output: None,
            budget_iterations: None,
            depth: Some(1),
            iterations_used: None,
            duration_ms: None,
            requested_at: "2026-07-10T08:00:00Z".to_string(),
            updated_at: "2026-07-10T08:01:00Z".to_string(),
        };
        let parent = ExecutionTreeNode {
            execution_id: "exec-parent".to_string(),
            parent_execution_id: None,
            root_execution_id: Some("exec-parent".to_string()),
            agent_id: "agent-a".to_string(),
            relationship_type: "root".to_string(),
            status: "running".to_string(),
            plan_id: None,
            primary_execution_output_id: None,
            active_child_execution_ids: vec!["exec-child".to_string()],
            child_execution_ids: vec!["exec-child".to_string()],
            ready_child_output_ids: Vec::new(),
            waiting_for_children: true,
            delegation_summary: delegation_summary("exec-parent", "quest-1", vec![delegation]),
            started_at: "2026-07-10T08:00:00Z".to_string(),
            completed_at: None,
            updated_at: "2026-07-10T08:01:00Z".to_string(),
        };
        let child = ExecutionTreeNode {
            execution_id: "exec-child".to_string(),
            parent_execution_id: Some("exec-parent".to_string()),
            root_execution_id: Some("exec-parent".to_string()),
            agent_id: "agent-b".to_string(),
            relationship_type: "delegation".to_string(),
            status: "running".to_string(),
            plan_id: None,
            primary_execution_output_id: None,
            active_child_execution_ids: Vec::new(),
            child_execution_ids: Vec::new(),
            ready_child_output_ids: Vec::new(),
            waiting_for_children: false,
            delegation_summary: delegation_summary("exec-child", "quest-1", Vec::new()),
            started_at: "2026-07-10T08:00:30Z".to_string(),
            completed_at: None,
            updated_at: "2026-07-10T08:01:00Z".to_string(),
        };
        let tree = ExecutionTreeRecord {
            task_id: "quest-1".to_string(),
            task_status: "running".to_string(),
            active_root_execution_id: Some("exec-parent".to_string()),
            latest_root_execution_id: Some("exec-parent".to_string()),
            last_completed_root_execution_id: None,
            root_execution_id: Some("exec-parent".to_string()),
            nodes: vec![parent, child],
        };
        let names = HashMap::from([
            ("agent-a".to_string(), "Alpha".to_string()),
            ("agent-b".to_string(), "Beta".to_string()),
        ]);

        let (handoffs, unresolved) = project_handoffs(&[tree], &names);
        assert_eq!(unresolved, 0);
        assert_eq!(handoffs.len(), 1);
        assert_eq!(handoffs[0].from.citizen_id, "agent-a");
        assert_eq!(handoffs[0].to.citizen_id, "agent-b");
        assert_eq!(handoffs[0].to.display_name.as_deref(), Some("Beta"));
        assert!(handoffs[0].active);
    }

    #[actix_web::test]
    async fn endpoint_requires_and_echoes_explicit_scope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let api = Arc::new(FleetStateApi::new(ArtifactV2Workspace::new(temp.path())));
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .service(web::scope("/api/magician/v2").configure(configure_routes)),
        )
        .await;

        let missing = actix_test::TestRequest::get()
            .uri("/api/magician/v2/fleet-state")
            .to_request();
        let missing_response = actix_test::call_service(&app, missing).await;
        assert_eq!(missing_response.status(), StatusCode::BAD_REQUEST);

        let scoped = actix_test::TestRequest::get()
            .uri("/api/magician/v2/fleet-state?workspace=main")
            .insert_header(("X-Principal", "alice"))
            .insert_header(("X-Workspace", "main"))
            .to_request();
        let scoped_response: Value = actix_test::call_and_read_body_json(&app, scoped).await;
        assert_eq!(
            scoped_response["schema_version"],
            FLEET_STATE_SCHEMA_VERSION
        );
        assert_eq!(scoped_response["scope"]["principal"], "alice");
        assert_eq!(scoped_response["scope"]["workspace"], "main");
        assert!(scoped_response["generated_at"].as_str().is_some());
        assert_eq!(
            scoped_response["availability"]["quests"]["status"],
            "unavailable"
        );
        assert_eq!(
            scoped_response["availability"]["guilds"]["status"],
            "available"
        );
    }
}
