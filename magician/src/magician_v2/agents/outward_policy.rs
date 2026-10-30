//! Optional capture overrides by trusted task origin. This changes dispatch
//! posture only: tool grants, approvals, containment and suppression still apply.

use std::{
    future::Future,
    sync::{OnceLock, RwLock},
};

use serde::{Deserialize, Serialize};

use crate::{
    config::{OutwardActionsConfig, OutwardContextMode},
    magician_v2::{
        analytics::llm_trace_content::scoped_content_fingerprint,
        artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error},
    },
};

use super::{types::GoalSource, AgentInvocationContext, InvocationSourceKind, InvocationSurface};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutwardOrigin {
    #[default]
    Unclassified,
    OwnerChat,
    OwnerTask,
    Opc,
    /// Envoy, public/shared-channel bots and agents declared public-facing.
    PublicAgent,
}

impl OutwardOrigin {
    /// Only server-resolved invocation context, never tool arguments or tags.
    pub fn for_invocation(invocation: Option<&AgentInvocationContext>) -> Self {
        match invocation {
            Some(context)
                if matches!(
                    context.surface,
                    InvocationSurface::Chat | InvocationSurface::RealtimeVoice
                ) && matches!(
                    context.source_kind,
                    InvocationSourceKind::Direct | InvocationSourceKind::ChatInline
                ) =>
            {
                Self::OwnerChat
            },
            Some(context) if context.source_kind == InvocationSourceKind::Public => {
                Self::PublicAgent
            },
            _ => Self::Unclassified,
        }
    }
}

/// A host-side locator, not an assertion extracted from a model-written goal.
/// A delegate on the same task and a restarted run consult the same sealed row.
#[derive(Debug, Clone)]
pub enum OutwardSource {
    Root {
        principal: String,
        workspace: String,
        origin: OutwardOrigin,
    },
    Task {
        principal: String,
        workspace: String,
        task_id: String,
    },
}

tokio::task_local! {
    static SOURCE: Option<OutwardSource>;
}

pub fn current_source() -> Option<OutwardSource> {
    SOURCE.try_with(Clone::clone).ok().flatten()
}

/// A new programme/public cycle establishes its own origin even when launched
/// from inside an owner chat. Ordinary chat delegation retains its source.
/// Public-originated work, and the cycles of an agent whose definition
/// declares itself public-facing (`declared_public`), count as public agents
/// rather than OPC.
pub(crate) fn source_for_goal(
    source: &GoalSource,
    invocation: Option<&AgentInvocationContext>,
    declared_public: bool,
    principal: &str,
    workspace: &str,
) -> Option<OutwardSource> {
    let public =
        invocation.is_some_and(|context| context.source_kind == InvocationSourceKind::Public);
    if public || (declared_public && *source != GoalSource::ChatInline) {
        Some(OutwardSource::root(
            principal,
            workspace,
            OutwardOrigin::PublicAgent,
        ))
    } else if *source != GoalSource::ChatInline {
        Some(OutwardSource::root(
            principal,
            workspace,
            OutwardOrigin::Opc,
        ))
    } else {
        current_source()
    }
}

pub fn with_source<F: Future>(
    source: Option<OutwardSource>,
    future: F,
) -> impl Future<Output = F::Output> {
    SOURCE.scope(source, future)
}

impl OutwardSource {
    pub fn root(principal: &str, workspace: &str, origin: OutwardOrigin) -> Self {
        Self::Root {
            principal: principal.into(),
            workspace: workspace.into(),
            origin,
        }
    }

    pub fn task(principal: &str, workspace: &str, task_id: &str) -> Self {
        Self::Task {
            principal: principal.into(),
            workspace: workspace.into(),
            task_id: task_id.into(),
        }
    }

    pub async fn resolve(
        &self,
        layout: &ArtifactV2Workspace,
        principal: &str,
        workspace: &str,
    ) -> Result<OutwardOrigin, String> {
        match self {
            Self::Root {
                principal: p,
                workspace: w,
                origin,
            } if p == principal && w == workspace => Ok(*origin),
            Self::Task {
                principal: p,
                workspace: w,
                task_id,
            } if p == principal && w == workspace => {
                load_task_origin(layout, principal, workspace, task_id).await
            },
            _ => Err("outward_origin_scope_mismatch".into()),
        }
    }
}

const DOMAIN: &str = "magician.outward-task-origin.v1";
// Serialize the write-once check and publication. Scoped storage leases fence
// other processes; this gate prevents two local creators replacing each other.
static ORIGIN_WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OriginRecord {
    origin: OutwardOrigin,
    hmac_sha256: String,
}

fn path(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    task_id: &str,
) -> std::path::PathBuf {
    layout
        .scope_root(principal, workspace)
        .join("restricted")
        .join("outward_origins")
        .join(format!(
            "{}.json",
            blake3::hash(task_id.as_bytes()).to_hex()
        ))
}

fn fingerprint(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    task_id: &str,
    origin: OutwardOrigin,
) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(DOMAIN, principal, workspace, task_id, origin))
        .map_err(|e| e.to_string())?;
    scoped_content_fingerprint(
        layout,
        &magicllm::LlmScope::new(principal, workspace),
        &bytes,
    )
    .map_err(|e| e.to_string())
}

/// Called only during task creation, before any execution can be dispatched.
/// There is no API/model parameter for replacing this immutable provenance.
pub(crate) async fn persist_task_origin(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    task_id: &str,
    origin: OutwardOrigin,
) -> Result<(), String> {
    let _write = ORIGIN_WRITES.lock().await;
    let record_path = path(layout, principal, workspace, task_id);
    match layout
        .read_json_bounded_stream_path::<OriginRecord, _>(&record_path, 4096, 8, 32)
        .await
    {
        Ok(existing) => {
            let loaded = load_task_origin(layout, principal, workspace, task_id).await?;
            return if loaded == origin && existing.origin == origin {
                Ok(())
            } else {
                Err("outward_origin_already_recorded".into())
            };
        },
        Err(ArtifactV2Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(e.to_string()),
    }
    let record = OriginRecord {
        origin,
        hmac_sha256: fingerprint(layout, principal, workspace, task_id, origin)?,
    };
    layout
        .create_dir_all_path(record_path.parent().expect("origin parent"))
        .await
        .map_err(|e| e.to_string())?;
    layout
        .write_json_atomic_path(&record_path, &record)
        .await
        .map_err(|e| e.to_string())
}

pub async fn load_task_origin(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    task_id: &str,
) -> Result<OutwardOrigin, String> {
    let record = match layout
        .read_json_bounded_stream_path::<OriginRecord, _>(
            &path(layout, principal, workspace, task_id),
            4096,
            8,
            32,
        )
        .await
    {
        Ok(record) => record,
        Err(ArtifactV2Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OutwardOrigin::Unclassified)
        },
        Err(e) => return Err(e.to_string()),
    };
    let expected = fingerprint(layout, principal, workspace, task_id, record.origin)?;
    let left = expected.as_bytes();
    let right = record.hmac_sha256.as_bytes();
    if left.len() != right.len()
        || left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            != 0
    {
        return Err("outward_origin_integrity_mismatch".into());
    }
    Ok(record.origin)
}

#[derive(Default)]
struct PolicyState {
    settings: Option<OutwardActionsConfig>,
    generation: u64,
}

#[derive(Default)]
struct LivePolicy(RwLock<PolicyState>);

impl LivePolicy {
    fn snapshot(&self) -> Option<OutwardActionsConfig> {
        match self.0.read() {
            Ok(state) => state.settings.clone(),
            Err(_) => Some(OutwardActionsConfig {
                force_capture_only: true,
                browser_submission_guard: true,
                ..Default::default()
            }),
        }
    }

    fn generation(&self) -> u64 {
        self.0
            .read()
            .map(|state| state.generation)
            .unwrap_or(u64::MAX)
    }

    fn publish(&self, settings: OutwardActionsConfig, expected: Option<u64>) -> bool {
        let mut state = self
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if expected.is_some_and(|generation| generation != state.generation) {
            return false;
        }
        state.settings = Some(settings);
        state.generation = state.generation.saturating_add(1);
        self.0.clear_poison();
        true
    }
}

static POSTURE: OnceLock<LivePolicy> = OnceLock::new();

fn policy() -> &'static LivePolicy {
    POSTURE.get_or_init(LivePolicy::default)
}

/// Compatibility installation: do not overwrite an already accepted config.
pub fn install(config: OutwardActionsConfig) -> bool {
    let mut state = policy()
        .0
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.settings.is_some() {
        return false;
    }
    state.settings = Some(config);
    state.generation = state.generation.saturating_add(1);
    policy().0.clear_poison();
    true
}

pub fn settings() -> Option<OutwardActionsConfig> {
    policy().snapshot()
}
pub fn generation() -> u64 {
    policy().generation()
}

/// Called only after an owner settings write or accepted configuration load.
pub fn configure(config: OutwardActionsConfig) {
    policy().publish(config, None);
}

/// An overlapping old reload must not revert a newer owner settings write.
pub fn configure_if_unchanged(config: OutwardActionsConfig, expected: u64) -> bool {
    policy().publish(config, Some(expected))
}

pub fn set_browser_submission_guard(enabled: bool) -> bool {
    let mut state = policy()
        .0
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let settings = state
        .settings
        .get_or_insert_with(OutwardActionsConfig::default);
    settings.browser_submission_guard = enabled;
    state.generation = state.generation.saturating_add(1);
    policy().0.clear_poison();
    true
}

pub fn capture_for(config: &OutwardActionsConfig, origin: OutwardOrigin) -> bool {
    if config.force_capture_only {
        return true;
    }
    let mode = match origin {
        OutwardOrigin::OwnerChat => config.contexts.owner_chat,
        OutwardOrigin::OwnerTask => config.contexts.owner_tasks,
        OutwardOrigin::Opc => config.contexts.opc,
        OutwardOrigin::PublicAgent => config.contexts.public_agents,
        OutwardOrigin::Unclassified => {
            // Once origin-specific controls are used, losing provenance must
            // never bypass a context configured capture under a live baseline.
            return config.capture_only
                || [
                    config.contexts.owner_chat,
                    config.contexts.owner_tasks,
                    config.contexts.opc,
                    config.contexts.public_agents,
                ]
                .iter()
                .any(|mode| *mode != OutwardContextMode::Inherit);
        },
    };
    match mode {
        OutwardContextMode::Inherit => config.capture_only,
        OutwardContextMode::Capture => true,
        OutwardContextMode::Live => false,
    }
}

pub fn unclassified_capture() -> bool {
    settings().is_none_or(|config| capture_for(&config, OutwardOrigin::Unclassified))
}

/// Missing metadata captures when context controls are used. The policy is
/// read AFTER provenance I/O, so a live change cannot be lost during lookup.
pub async fn capture_for_task(
    layout: &ArtifactV2Workspace,
    principal: Option<&str>,
    workspace: Option<&str>,
    task_id: Option<&str>,
) -> Result<bool, String> {
    capture_for_task_at(policy(), layout, principal, workspace, task_id).await
}

async fn capture_after_origin<F>(policy: &LivePolicy, origin: F) -> Result<bool, String>
where
    F: Future<Output = Result<OutwardOrigin, String>>,
{
    let origin = origin.await?;
    Ok(policy
        .snapshot()
        .is_none_or(|config| capture_for(&config, origin)))
}

async fn capture_for_task_at(
    policy: &LivePolicy,
    layout: &ArtifactV2Workspace,
    principal: Option<&str>,
    workspace: Option<&str>,
    task_id: Option<&str>,
) -> Result<bool, String> {
    if policy
        .snapshot()
        .is_none_or(|config| config.force_capture_only)
    {
        return Ok(true);
    }
    capture_after_origin(policy, async {
        match (principal, workspace, task_id) {
            (Some(p), Some(w), Some(task)) => load_task_origin(layout, p, w, task).await,
            (Some(p), Some(w), None) => match current_source() {
                Some(source) => source.resolve(layout, p, w).await,
                None => Ok(OutwardOrigin::Unclassified),
            },
            _ => Ok(OutwardOrigin::Unclassified),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OutwardContextsConfig;

    #[tokio::test]
    async fn an_uninstalled_posture_captures_rather_than_sends() {
        // A fresh registry, independent of config-loading tests in this process.
        let policy = LivePolicy::default();
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        assert!(
            capture_for_task_at(&policy, &layout, Some("p"), Some("w"), Some("task"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn taskless_dispatch_uses_host_source_without_promoting_an_unknown_task() {
        let policy = LivePolicy::default();
        let mut config = OutwardActionsConfig::default();
        config.contexts.owner_chat = OutwardContextMode::Live;
        policy.publish(config, None);
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        with_source(
            Some(OutwardSource::root("p", "w", OutwardOrigin::OwnerChat)),
            async {
                assert!(
                    !capture_for_task_at(&policy, &layout, Some("p"), Some("w"), None)
                        .await
                        .unwrap()
                );
                assert!(capture_for_task_at(
                    &policy,
                    &layout,
                    Some("p"),
                    Some("w"),
                    Some("legacy")
                )
                .await
                .unwrap());
                assert!(
                    capture_for_task_at(&policy, &layout, Some("p"), Some("other"), None)
                        .await
                        .is_err()
                );
            },
        )
        .await;
        assert!(
            capture_for_task_at(&policy, &layout, Some("p"), Some("w"), None)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn conflicting_creators_cannot_replace_an_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        let (chat, opc) = tokio::join!(
            persist_task_origin(&layout, "p", "w", "task", OutwardOrigin::OwnerChat),
            persist_task_origin(&layout, "p", "w", "task", OutwardOrigin::Opc),
        );
        assert_ne!(chat.is_ok(), opc.is_ok());
        let expected = if chat.is_ok() {
            OutwardOrigin::OwnerChat
        } else {
            OutwardOrigin::Opc
        };
        assert_eq!(
            load_task_origin(&layout, "p", "w", "task").await.unwrap(),
            expected
        );
    }

    #[test]
    fn only_direct_private_invocation_can_establish_owner_chat_origin() {
        let mut context = AgentInvocationContext {
            principal: "p".into(),
            workspace: "w".into(),
            source_agent_id: None,
            target_agent_id: "assistant".into(),
            surface: InvocationSurface::Chat,
            source_kind: InvocationSourceKind::Direct,
            feature_mode: super::super::FeatureMode::None,
            chat_session_id: Some("session".into()),
            chat_turn_id: Some("turn".into()),
        };
        assert_eq!(
            OutwardOrigin::for_invocation(Some(&context)),
            OutwardOrigin::OwnerChat
        );
        context.source_kind = InvocationSourceKind::Delegated;
        assert_eq!(
            OutwardOrigin::for_invocation(Some(&context)),
            OutwardOrigin::Unclassified
        );
        context.source_kind = InvocationSourceKind::Public;
        context.surface = InvocationSurface::PublicEnvoy;
        assert_eq!(
            OutwardOrigin::for_invocation(Some(&context)),
            OutwardOrigin::PublicAgent
        );
        assert!(matches!(
            source_for_goal(&GoalSource::ChatInline, Some(&context), false, "p", "w"),
            Some(OutwardSource::Root {
                origin: OutwardOrigin::PublicAgent,
                ..
            })
        ));
        // Meeting rooms are public invocations too (chat service stamps them
        // `Public`), so they are Envoy-and-public work, never OPC.
        context.surface = InvocationSurface::Meeting;
        assert_eq!(
            OutwardOrigin::for_invocation(Some(&context)),
            OutwardOrigin::PublicAgent
        );
        assert_eq!(
            OutwardOrigin::for_invocation(None),
            OutwardOrigin::Unclassified
        );
    }

    #[test]
    fn complete_posture_matrix() {
        use OutwardContextMode::{Capture, Inherit, Live};
        for baseline in [false, true] {
            for force in [false, true] {
                for chat in [Inherit, Capture, Live] {
                    for tasks in [Inherit, Capture, Live] {
                        for opc in [Inherit, Capture, Live] {
                            let config = OutwardActionsConfig {
                                capture_only: baseline,
                                force_capture_only: force,
                                contexts: OutwardContextsConfig {
                                    owner_chat: chat,
                                    owner_tasks: tasks,
                                    opc,
                                    public_agents: OutwardContextMode::Inherit,
                                },
                                ..Default::default()
                            };
                            for (origin, mode) in [
                                (OutwardOrigin::OwnerChat, chat),
                                (OutwardOrigin::OwnerTask, tasks),
                                (OutwardOrigin::Opc, opc),
                                (OutwardOrigin::Unclassified, Inherit),
                            ] {
                                let expected = force
                                    || (origin == OutwardOrigin::Unclassified
                                        && [chat, tasks, opc].iter().any(|mode| *mode != Inherit))
                                    || match mode {
                                        Inherit => baseline,
                                        Capture => true,
                                        Live => false,
                                    };
                                assert_eq!(
                                    capture_for(&config, origin),
                                    expected,
                                    "{origin:?} {mode:?} baseline={baseline} force={force}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn provenance_survives_reload_and_rejects_tampering_and_copying() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path().to_path_buf());
        persist_task_origin(&layout, "p", "w", "task-a", OutwardOrigin::OwnerChat)
            .await
            .unwrap();
        let reloaded = ArtifactV2Workspace::new(tmp.path().to_path_buf());
        assert_eq!(
            load_task_origin(&reloaded, "p", "w", "task-a")
                .await
                .unwrap(),
            OutwardOrigin::OwnerChat
        );
        assert!(
            persist_task_origin(&layout, "p", "w", "task-a", OutwardOrigin::Opc)
                .await
                .is_err()
        );
        let record_path = path(&layout, "p", "w", "task-a");
        let mut record: OriginRecord = layout
            .read_json_bounded_stream_path(&record_path, 4096, 8, 32)
            .await
            .unwrap();
        layout
            .write_json_atomic_path(&path(&layout, "p", "w", "task-b"), &record)
            .await
            .unwrap();
        assert!(load_task_origin(&layout, "p", "w", "task-b").await.is_err());
        record.origin = OutwardOrigin::Opc;
        layout
            .write_json_atomic_path(&record_path, &record)
            .await
            .unwrap();
        assert!(load_task_origin(&layout, "p", "w", "task-a").await.is_err());
        assert_eq!(
            load_task_origin(&layout, "p", "w", "legacy").await.unwrap(),
            OutwardOrigin::Unclassified
        );
    }

    #[tokio::test]
    async fn missing_or_invalid_origin_never_escapes_context_capture_via_live_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        let config = OutwardActionsConfig {
            capture_only: false,
            contexts: OutwardContextsConfig {
                opc: OutwardContextMode::Capture,
                ..Default::default()
            },
            ..Default::default()
        };
        let policy = LivePolicy::default();
        policy.publish(config, None);
        assert!(
            capture_for_task_at(&policy, &layout, Some("p"), Some("w"), Some("legacy"))
                .await
                .unwrap()
        );
        persist_task_origin(&layout, "p", "w", "task", OutwardOrigin::Opc)
            .await
            .unwrap();
        assert!(
            capture_for_task_at(&policy, &layout, Some("p"), Some("w"), Some("task"))
                .await
                .unwrap()
        );
        let record_path = path(&layout, "p", "w", "task");
        let mut record: OriginRecord = layout
            .read_json_bounded_stream_path(&record_path, 4096, 8, 32)
            .await
            .unwrap();
        record.origin = OutwardOrigin::OwnerTask;
        layout
            .write_json_atomic_path(&record_path, &record)
            .await
            .unwrap();
        assert!(
            capture_for_task_at(&policy, &layout, Some("p"), Some("w"), Some("task"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn task_source_is_scoped_and_delegation_reads_original_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path().to_path_buf());
        persist_task_origin(&layout, "p", "w", "parent", OutwardOrigin::Opc)
            .await
            .unwrap();
        let source = OutwardSource::task("p", "w", "parent");
        assert_eq!(
            source.resolve(&layout, "p", "w").await.unwrap(),
            OutwardOrigin::Opc
        );
        assert!(source.resolve(&layout, "p", "other").await.is_err());
        with_source(Some(source), async {
            let captured = current_source().unwrap();
            assert_eq!(
                captured.resolve(&layout, "p", "w").await.unwrap(),
                OutwardOrigin::Opc
            );
        })
        .await;
        assert!(current_source().is_none());
    }

    #[tokio::test]
    async fn public_agents_are_their_own_context_apart_from_autonomous_work() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        // A declared public-facing agent's scheduled cycle (the ambassador).
        let declared = source_for_goal(&GoalSource::Schedule, None, true, "p", "w").unwrap();
        assert_eq!(
            declared.resolve(&layout, "p", "w").await.unwrap(),
            OutwardOrigin::PublicAgent
        );
        // Ordinary autonomous work stays OPC.
        let cycle = source_for_goal(&GoalSource::Schedule, None, false, "p", "w").unwrap();
        assert_eq!(
            cycle.resolve(&layout, "p", "w").await.unwrap(),
            OutwardOrigin::Opc
        );
        // Owner chat delegating to a declared-public agent is still the owner's work.
        with_source(
            Some(OutwardSource::root("p", "w", OutwardOrigin::OwnerChat)),
            async {
                let delegated =
                    source_for_goal(&GoalSource::ChatInline, None, true, "p", "w").unwrap();
                assert_eq!(
                    delegated.resolve(&layout, "p", "w").await.unwrap(),
                    OutwardOrigin::OwnerChat
                );
            },
        )
        .await;

        // OPC may send while public agents stay captured, and the reverse.
        let mut config = OutwardActionsConfig {
            capture_only: true,
            ..Default::default()
        };
        config.contexts.opc = OutwardContextMode::Live;
        assert!(!capture_for(&config, OutwardOrigin::Opc));
        assert!(capture_for(&config, OutwardOrigin::PublicAgent));
        config.contexts.public_agents = OutwardContextMode::Live;
        config.contexts.opc = OutwardContextMode::Capture;
        assert!(capture_for(&config, OutwardOrigin::Opc));
        assert!(!capture_for(&config, OutwardOrigin::PublicAgent));
        // A public-agents override alone still captures unclassified work.
        let mut only_public = OutwardActionsConfig {
            capture_only: false,
            ..Default::default()
        };
        only_public.contexts.public_agents = OutwardContextMode::Capture;
        assert!(capture_for(&only_public, OutwardOrigin::Unclassified));
    }

    #[tokio::test]
    async fn programme_cycles_do_not_inherit_owner_chat_privileges() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        with_source(
            Some(OutwardSource::root("p", "w", OutwardOrigin::OwnerChat)),
            async {
                let scheduled =
                    source_for_goal(&GoalSource::Schedule, None, false, "p", "w").unwrap();
                assert_eq!(
                    scheduled.resolve(&layout, "p", "w").await.unwrap(),
                    OutwardOrigin::Opc
                );
                let delegated =
                    source_for_goal(&GoalSource::ChatInline, None, false, "p", "w").unwrap();
                assert_eq!(
                    delegated.resolve(&layout, "p", "w").await.unwrap(),
                    OutwardOrigin::OwnerChat
                );
            },
        )
        .await;
        // An OPC cycle delegating through the chat-inline runtime stays OPC.
        with_source(
            Some(OutwardSource::root("p", "w", OutwardOrigin::Opc)),
            async {
                let delegated =
                    source_for_goal(&GoalSource::ChatInline, None, false, "p", "w").unwrap();
                assert_eq!(
                    delegated.resolve(&layout, "p", "w").await.unwrap(),
                    OutwardOrigin::Opc
                );
            },
        )
        .await;
    }

    #[test]
    fn config_defaults_preserve_capture_and_invalid_modes_are_rejected() {
        let config: OutwardActionsConfig = serde_yaml::from_str("capture_only: true").unwrap();
        for origin in [
            OutwardOrigin::OwnerChat,
            OutwardOrigin::OwnerTask,
            OutwardOrigin::Opc,
            OutwardOrigin::Unclassified,
        ] {
            assert!(capture_for(&config, origin));
        }
        assert!(
            serde_yaml::from_str::<OutwardActionsConfig>("contexts: {owner_chat: liv}").is_err()
        );
        assert!(
            serde_yaml::from_str::<OutwardActionsConfig>("contexts: {owner_chats: live}").is_err()
        );
    }

    #[tokio::test]
    async fn live_changes_apply_after_lookup_and_stale_reload_cannot_revert_them() {
        let policy = LivePolicy::default();
        policy.publish(
            OutwardActionsConfig {
                capture_only: false,
                ..Default::default()
            },
            None,
        );
        let old_generation = policy.generation();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (decision, ()) = tokio::join!(
            capture_after_origin(&policy, async {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok(OutwardOrigin::Opc)
            }),
            async {
                started_rx.await.unwrap();
                policy.publish(
                    OutwardActionsConfig {
                        force_capture_only: true,
                        ..Default::default()
                    },
                    None,
                );
                release_tx.send(()).unwrap();
            }
        );
        assert!(
            decision.unwrap(),
            "a restriction saved during lookup applies to this decision"
        );
        assert!(!policy.publish(
            OutwardActionsConfig {
                capture_only: false,
                ..Default::default()
            },
            Some(old_generation)
        ));
        assert!(policy.snapshot().unwrap().force_capture_only);
        policy.publish(
            OutwardActionsConfig {
                contexts: OutwardContextsConfig {
                    owner_chat: OutwardContextMode::Live,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        assert!(
            !capture_after_origin(&policy, async { Ok(OutwardOrigin::OwnerChat) })
                .await
                .unwrap()
        );
        assert!(
            capture_after_origin(&policy, async { Ok(OutwardOrigin::Opc) })
                .await
                .unwrap()
        );
    }
}
