//! Dormant process-local ownership for complete projected MCP catalog snapshots.
//!
//! This owner joins already projected per-skill catalogs. It performs no discovery,
//! transport, authentication, authorization, model routing, tool invocation, telemetry,
//! persistence, or product registration. A complete bounded candidate is prepared before
//! the write lock is taken, then published with one optimistic-revision swap or not at all.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    sync::{Arc, RwLock},
};

use serde::Serialize;

use crate::{
    McpModelToolDefinition, McpProjectedCatalog, McpProjectedTool, MCP_PROJECTED_CATALOG_V1,
};

pub const MCP_PRODUCT_CATALOG_SNAPSHOT_V1: &str = "magician-mcp.product-catalog-snapshot.v1";
pub const HARD_MAX_PUBLISHED_MCP_SKILLS: usize = 128;
pub const HARD_MAX_PUBLISHED_MCP_TOOLS: usize = 16_384;
pub const HARD_MAX_PUBLISHED_MCP_ACCOUNTED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpProductCatalogLimits {
    max_skills: usize,
    max_tools: usize,
    max_accounted_bytes: usize,
}

impl McpProductCatalogLimits {
    pub fn new(
        max_skills: usize,
        max_tools: usize,
        max_accounted_bytes: usize,
    ) -> Result<Self, McpCatalogOwnerError> {
        if max_skills == 0
            || max_skills > HARD_MAX_PUBLISHED_MCP_SKILLS
            || max_tools == 0
            || max_tools > HARD_MAX_PUBLISHED_MCP_TOOLS
            || max_accounted_bytes == 0
            || max_accounted_bytes > HARD_MAX_PUBLISHED_MCP_ACCOUNTED_BYTES
        {
            return Err(McpCatalogOwnerError::new(
                McpCatalogOwnerErrorCode::InvalidLimits,
                "limits",
                "the MCP product catalog limits must be nonzero and within immutable ceilings",
            ));
        }
        Ok(Self {
            max_skills,
            max_tools,
            max_accounted_bytes,
        })
    }

    pub fn max_skills(self) -> usize {
        self.max_skills
    }

    pub fn max_tools(self) -> usize {
        self.max_tools
    }

    pub fn max_accounted_bytes(self) -> usize {
        self.max_accounted_bytes
    }
}

impl Default for McpProductCatalogLimits {
    fn default() -> Self {
        Self {
            max_skills: HARD_MAX_PUBLISHED_MCP_SKILLS,
            max_tools: HARD_MAX_PUBLISHED_MCP_TOOLS,
            max_accounted_bytes: HARD_MAX_PUBLISHED_MCP_ACCOUNTED_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpCatalogOwnerErrorCode {
    InvalidLimits,
    TooManySkills,
    TooManyTools,
    CatalogTooLarge,
    DuplicateNamespace,
    ToolNameCollision,
    InvalidProjectedCatalog,
    StaleRevision,
    RevisionExhausted,
    StateUnavailable,
}

/// Stable value-free publication failure. Skill namespaces, tool names, schemas, and
/// remote metadata never enter this diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct McpCatalogOwnerError {
    pub code: McpCatalogOwnerErrorCode,
    pub field: &'static str,
    pub message: &'static str,
}

impl McpCatalogOwnerError {
    const fn new(
        code: McpCatalogOwnerErrorCode,
        field: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            code,
            field,
            message,
        }
    }
}

impl fmt::Display for McpCatalogOwnerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for McpCatalogOwnerError {}

/// Immutable process-local view. It deliberately has no serialization contract because
/// execution ids, output schemas, remote hints, and effective policy are retained beside
/// each tool. Model serialization remains possible only per tool through
/// `McpProjectedTool::model_definition`.
pub struct McpProductCatalogSnapshot {
    schema_version: &'static str,
    revision: u64,
    catalogs: Vec<McpProjectedCatalog>,
    tool_index: BTreeMap<String, (usize, usize)>,
    tool_count: usize,
    accounted_bytes: usize,
}

impl McpProductCatalogSnapshot {
    fn empty() -> Self {
        Self {
            schema_version: MCP_PRODUCT_CATALOG_SNAPSHOT_V1,
            revision: 0,
            catalogs: Vec::new(),
            tool_index: BTreeMap::new(),
            tool_count: 0,
            accounted_bytes: 0,
        }
    }

    pub fn schema_version(&self) -> &'static str {
        self.schema_version
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn catalog_count(&self) -> usize {
        self.catalogs.len()
    }

    pub fn tool_count(&self) -> usize {
        self.tool_count
    }

    pub fn accounted_bytes(&self) -> usize {
        self.accounted_bytes
    }

    pub fn catalogs(&self) -> &[McpProjectedCatalog] {
        &self.catalogs
    }

    pub fn catalog(&self, namespace: &str) -> Option<&McpProjectedCatalog> {
        self.catalogs
            .binary_search_by(|catalog| catalog.namespace().cmp(namespace))
            .ok()
            .map(|index| &self.catalogs[index])
    }

    pub fn tool(&self, local_name: &str) -> Option<&McpProjectedTool> {
        let &(catalog_index, tool_index) = self.tool_index.get(local_name)?;
        self.catalogs
            .get(catalog_index)
            .and_then(|catalog| catalog.tools().get(tool_index))
    }

    pub fn model_definitions(&self) -> impl Iterator<Item = McpModelToolDefinition<'_>> + '_ {
        self.catalogs
            .iter()
            .flat_map(|catalog| catalog.tools().iter())
            .map(McpProjectedTool::model_definition)
    }

    fn content_eq(&self, other: &PreparedSnapshot) -> bool {
        self.catalogs == other.catalogs
    }
}

impl fmt::Debug for McpProductCatalogSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpProductCatalogSnapshot")
            .field("schema_version", &self.schema_version)
            .field("revision", &self.revision)
            .field("catalog_count", &self.catalogs.len())
            .field("tool_count", &self.tool_count)
            .field("accounted_bytes", &self.accounted_bytes)
            .finish()
    }
}

pub struct McpCatalogPublication {
    changed: bool,
    snapshot: Arc<McpProductCatalogSnapshot>,
}

impl McpCatalogPublication {
    pub fn changed(&self) -> bool {
        self.changed
    }

    pub fn revision(&self) -> u64 {
        self.snapshot.revision()
    }

    pub fn snapshot(&self) -> &Arc<McpProductCatalogSnapshot> {
        &self.snapshot
    }

    pub fn into_snapshot(self) -> Arc<McpProductCatalogSnapshot> {
        self.snapshot
    }
}

impl fmt::Debug for McpCatalogPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpCatalogPublication")
            .field("changed", &self.changed)
            .field("revision", &self.snapshot.revision())
            .field("catalog_count", &self.snapshot.catalog_count())
            .field("tool_count", &self.snapshot.tool_count())
            .finish()
    }
}

/// Single process-local owner. Share it through `Arc<McpProductCatalogOwner>` rather
/// than cloning independent owners with divergent revision histories.
pub struct McpProductCatalogOwner {
    limits: McpProductCatalogLimits,
    state: RwLock<Arc<McpProductCatalogSnapshot>>,
}

impl McpProductCatalogOwner {
    pub fn new() -> Self {
        Self::with_limits(McpProductCatalogLimits::default())
    }

    pub fn with_limits(limits: McpProductCatalogLimits) -> Self {
        Self {
            limits,
            state: RwLock::new(Arc::new(McpProductCatalogSnapshot::empty())),
        }
    }

    pub fn limits(&self) -> McpProductCatalogLimits {
        self.limits
    }

    pub fn snapshot(&self) -> Result<Arc<McpProductCatalogSnapshot>, McpCatalogOwnerError> {
        self.state
            .read()
            .map(|state| Arc::clone(&state))
            .map_err(|_| {
                McpCatalogOwnerError::new(
                    McpCatalogOwnerErrorCode::StateUnavailable,
                    "catalog_state",
                    "the MCP product catalog state is unavailable",
                )
            })
    }

    /// Atomically replace the complete cross-skill snapshot if `expected_revision` is
    /// current. Candidate construction, sorting, bounds, and collision checks happen
    /// before the writer lock. A stale writer changes nothing, and an identical complete
    /// candidate does not consume a revision.
    pub fn replace_all(
        &self,
        expected_revision: u64,
        catalogs: Vec<McpProjectedCatalog>,
    ) -> Result<McpCatalogPublication, McpCatalogOwnerError> {
        let observed = self.snapshot()?;
        if observed.revision() != expected_revision {
            return Err(stale_revision());
        }
        let candidate = PreparedSnapshot::new(catalogs, self.limits)?;
        let unchanged = observed.content_eq(&candidate);

        let mut state = self.state.write().map_err(|_| {
            McpCatalogOwnerError::new(
                McpCatalogOwnerErrorCode::StateUnavailable,
                "catalog_state",
                "the MCP product catalog state is unavailable",
            )
        })?;
        if state.revision() != expected_revision {
            return Err(stale_revision());
        }
        if unchanged {
            return Ok(McpCatalogPublication {
                changed: false,
                snapshot: Arc::clone(&state),
            });
        }

        let revision = expected_revision.checked_add(1).ok_or_else(|| {
            McpCatalogOwnerError::new(
                McpCatalogOwnerErrorCode::RevisionExhausted,
                "revision",
                "the MCP product catalog revision is exhausted",
            )
        })?;
        let published = Arc::new(candidate.publish(revision));
        let previous = std::mem::replace(&mut *state, Arc::clone(&published));
        drop(state);
        drop(previous);
        Ok(McpCatalogPublication {
            changed: true,
            snapshot: published,
        })
    }
}

impl Default for McpProductCatalogOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for McpProductCatalogOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("McpProductCatalogOwner");
        debug.field("limits", &self.limits);
        match self.state.read() {
            Ok(state) => {
                debug
                    .field("revision", &state.revision())
                    .field("catalog_count", &state.catalog_count())
                    .field("tool_count", &state.tool_count());
            },
            Err(_) => {
                debug.field("state", &"unavailable");
            },
        }
        debug.finish()
    }
}

struct PreparedSnapshot {
    catalogs: Vec<McpProjectedCatalog>,
    tool_index: BTreeMap<String, (usize, usize)>,
    tool_count: usize,
    accounted_bytes: usize,
}

impl PreparedSnapshot {
    fn new(
        mut catalogs: Vec<McpProjectedCatalog>,
        limits: McpProductCatalogLimits,
    ) -> Result<Self, McpCatalogOwnerError> {
        if catalogs.len() > limits.max_skills {
            return Err(McpCatalogOwnerError::new(
                McpCatalogOwnerErrorCode::TooManySkills,
                "catalogs",
                "the MCP product catalog exceeds its skill limit",
            ));
        }
        catalogs.sort_by(|left, right| left.namespace().cmp(right.namespace()));

        let mut namespaces = BTreeSet::new();
        let mut tool_index = BTreeMap::new();
        let mut tool_count = 0usize;
        let mut accounted_bytes = 0usize;
        for (catalog_index, catalog) in catalogs.iter().enumerate() {
            if catalog.schema_version() != MCP_PROJECTED_CATALOG_V1 {
                return Err(invalid_projected_catalog());
            }
            if !namespaces.insert(catalog.namespace()) {
                return Err(McpCatalogOwnerError::new(
                    McpCatalogOwnerErrorCode::DuplicateNamespace,
                    "catalogs",
                    "the MCP product catalog contains a duplicate skill namespace",
                ));
            }
            add_accounted_bytes(
                &mut accounted_bytes,
                catalog.accounted_bytes(),
                limits.max_accounted_bytes,
            )?;

            for (catalog_tool_index, tool) in catalog.tools().iter().enumerate() {
                tool_count = tool_count.checked_add(1).ok_or_else(too_many_tools)?;
                if tool_count > limits.max_tools {
                    return Err(too_many_tools());
                }
                if tool_index.contains_key(tool.local_name()) {
                    return Err(McpCatalogOwnerError::new(
                        McpCatalogOwnerErrorCode::ToolNameCollision,
                        "tools",
                        "projected MCP tools collide across skill catalogs",
                    ));
                }
                if !tool_name_is_bound_to_catalog(catalog.namespace(), tool) {
                    return Err(invalid_projected_catalog());
                }
                add_accounted_bytes(
                    &mut accounted_bytes,
                    tool.local_name().len(),
                    limits.max_accounted_bytes,
                )?;
                tool_index.insert(
                    tool.local_name().to_owned(),
                    (catalog_index, catalog_tool_index),
                );
            }
        }

        Ok(Self {
            catalogs,
            tool_index,
            tool_count,
            accounted_bytes,
        })
    }

    fn publish(self, revision: u64) -> McpProductCatalogSnapshot {
        McpProductCatalogSnapshot {
            schema_version: MCP_PRODUCT_CATALOG_SNAPSHOT_V1,
            revision,
            catalogs: self.catalogs,
            tool_index: self.tool_index,
            tool_count: self.tool_count,
            accounted_bytes: self.accounted_bytes,
        }
    }
}

fn tool_name_is_bound_to_catalog(namespace: &str, tool: &McpProjectedTool) -> bool {
    let remote_name = tool.remote_id().remote_name();
    let local_name = tool.local_name();
    local_name.len()
        == namespace
            .len()
            .checked_add(1)
            .and_then(|length| length.checked_add(remote_name.len()))
            .unwrap_or(usize::MAX)
        && local_name.starts_with(namespace)
        && local_name.as_bytes().get(namespace.len()) == Some(&b'.')
        && local_name
            .get(namespace.len() + 1..)
            .is_some_and(|suffix| suffix == remote_name)
}

fn add_accounted_bytes(
    total: &mut usize,
    addition: usize,
    maximum: usize,
) -> Result<(), McpCatalogOwnerError> {
    *total = total.checked_add(addition).ok_or_else(catalog_too_large)?;
    if *total > maximum {
        return Err(catalog_too_large());
    }
    Ok(())
}

fn too_many_tools() -> McpCatalogOwnerError {
    McpCatalogOwnerError::new(
        McpCatalogOwnerErrorCode::TooManyTools,
        "tools",
        "the MCP product catalog exceeds its tool limit",
    )
}

fn catalog_too_large() -> McpCatalogOwnerError {
    McpCatalogOwnerError::new(
        McpCatalogOwnerErrorCode::CatalogTooLarge,
        "catalog",
        "the MCP product catalog exceeds its aggregate accounted-byte limit",
    )
}

fn invalid_projected_catalog() -> McpCatalogOwnerError {
    McpCatalogOwnerError::new(
        McpCatalogOwnerErrorCode::InvalidProjectedCatalog,
        "catalog",
        "an MCP catalog is not a valid projected snapshot",
    )
}

fn stale_revision() -> McpCatalogOwnerError {
    McpCatalogOwnerError::new(
        McpCatalogOwnerErrorCode::StaleRevision,
        "revision",
        "the MCP product catalog revision is stale",
    )
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use serde_json::json;
    use static_assertions::assert_not_impl_any;
    use tool_runtime_core::{
        manifest::{
            AuthContract, McpDiscoveryPolicy, McpTransport, PolicyFloor, RuntimeLimits,
            RuntimeProtocol, RuntimeRequirements, SkillRuntimeContract,
            SkillRuntimeContractVersion,
        },
        manifest_validation::validate_skill_runtime_contract,
        mcp_catalog_policy::McpCatalogPolicyContract,
    };

    use super::*;
    use crate::{project_mcp_catalog, McpToolDescriptor, McpToolHints, McpToolId};

    fn catalog(namespace: &str, names: &[&str]) -> McpProjectedCatalog {
        catalog_with_identity(namespace, names, 1, 1)
    }

    fn catalog_with_identity(
        namespace: &str,
        names: &[&str],
        client_instance_id: u64,
        discovery_generation: u64,
    ) -> McpProjectedCatalog {
        let contract = SkillRuntimeContract {
            schema_version: SkillRuntimeContractVersion::v1(),
            requires: RuntimeRequirements::default(),
            runtime: RuntimeProtocol::Mcp {
                transport: McpTransport::StreamableHttp {
                    endpoint: "https://provider.example/mcp".to_owned(),
                },
                discovery: McpDiscoveryPolicy::default(),
                limits: RuntimeLimits::default(),
            },
            auth: AuthContract::default(),
            policy_floor: PolicyFloor::default(),
        };
        let validated = validate_skill_runtime_contract(&contract).unwrap();
        let policy = McpCatalogPolicyContract::compile(namespace, validated).unwrap();
        let descriptors = names
            .iter()
            .map(|name| McpToolDescriptor {
                id: McpToolId::new((*name).to_owned(), client_instance_id, discovery_generation),
                title: Some("REMOTE TITLE CANARY".to_owned()),
                description: Some("REMOTE DESCRIPTION CANARY".to_owned()),
                input_schema: json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "additionalProperties": false
                })
                .as_object()
                .unwrap()
                .clone(),
                output_schema: None,
                hints: McpToolHints::default(),
            })
            .collect();
        project_mcp_catalog(descriptors, &policy).unwrap()
    }

    #[test]
    fn initial_snapshot_is_empty_revision_zero_and_metadata_only_in_debug() {
        let owner = McpProductCatalogOwner::new();
        let snapshot = owner.snapshot().unwrap();
        assert_eq!(snapshot.schema_version(), MCP_PRODUCT_CATALOG_SNAPSHOT_V1);
        assert_eq!(snapshot.revision(), 0);
        assert_eq!(snapshot.catalog_count(), 0);
        assert_eq!(snapshot.tool_count(), 0);
        let debug = format!("{snapshot:?}");
        assert!(!debug.contains("REMOTE"));
        assert_not_impl_any!(McpProductCatalogSnapshot: Serialize, Clone);
        assert_not_impl_any!(McpCatalogPublication: Serialize, Clone);
        assert_not_impl_any!(McpProductCatalogOwner: Serialize, Clone);
    }

    #[test]
    fn replacement_sorts_catalogs_builds_exact_lookup_and_advances_once() {
        let owner = McpProductCatalogOwner::new();
        let publication = owner
            .replace_all(
                0,
                vec![catalog("zeta", &["read"]), catalog("alpha", &["write"])],
            )
            .unwrap();
        assert!(publication.changed());
        assert_eq!(publication.revision(), 1);
        let snapshot = publication.snapshot();
        assert_eq!(snapshot.catalogs()[0].namespace(), "alpha");
        assert_eq!(snapshot.catalogs()[1].namespace(), "zeta");
        assert_eq!(
            snapshot.tool("alpha.write").unwrap().local_name(),
            "alpha.write"
        );
        assert!(snapshot.tool("zeta.write").is_none());
        assert_eq!(snapshot.model_definitions().count(), 2);
    }

    #[test]
    fn identical_reordered_candidate_reuses_snapshot_without_revision_churn() {
        let owner = McpProductCatalogOwner::new();
        let first = owner
            .replace_all(0, vec![catalog("alpha", &["a"]), catalog("beta", &["b"])])
            .unwrap()
            .into_snapshot();
        let second = owner
            .replace_all(1, vec![catalog("beta", &["b"]), catalog("alpha", &["a"])])
            .unwrap();
        assert!(!second.changed());
        assert_eq!(second.revision(), 1);
        assert!(Arc::ptr_eq(&first, second.snapshot()));
    }

    #[test]
    fn duplicate_namespace_and_cross_catalog_tool_collision_fail_atomically() {
        let owner = McpProductCatalogOwner::new();
        let first = owner
            .replace_all(0, vec![catalog("stable", &["read"])])
            .unwrap()
            .into_snapshot();

        let error = owner
            .replace_all(
                1,
                vec![
                    catalog("duplicate", &["one"]),
                    catalog("duplicate", &["two"]),
                ],
            )
            .unwrap_err();
        assert_eq!(error.code, McpCatalogOwnerErrorCode::DuplicateNamespace);

        let mut second = catalog("second", &["read"]);
        second.rewrite_local_name_for_test(0, "first.read".to_owned());
        let error = owner
            .replace_all(1, vec![catalog("first", &["read"]), second])
            .unwrap_err();
        assert_eq!(error.code, McpCatalogOwnerErrorCode::ToolNameCollision);
        let current = owner.snapshot().unwrap();
        assert!(Arc::ptr_eq(&first, &current));
    }

    #[test]
    fn fresh_discovery_authority_advances_revision_even_when_schema_is_unchanged() {
        let owner = McpProductCatalogOwner::new();
        let first = owner
            .replace_all(0, vec![catalog_with_identity("provider", &["read"], 7, 1)])
            .unwrap();
        let second = owner
            .replace_all(1, vec![catalog_with_identity("provider", &["read"], 7, 2)])
            .unwrap();
        assert!(first.changed());
        assert!(second.changed());
        assert_eq!(second.revision(), 2);
        assert!(!Arc::ptr_eq(first.snapshot(), second.snapshot()));
    }

    #[test]
    fn lower_limits_reject_skill_tool_and_accounted_byte_overflow_without_echoes() {
        assert_eq!(
            McpProductCatalogLimits::new(0, 1, 1).unwrap_err().code,
            McpCatalogOwnerErrorCode::InvalidLimits
        );
        let skill_owner = McpProductCatalogOwner::with_limits(
            McpProductCatalogLimits::new(1, 4, 1_000_000).unwrap(),
        );
        assert_eq!(
            skill_owner
                .replace_all(0, vec![catalog("a", &["x"]), catalog("b", &["y"])])
                .unwrap_err()
                .code,
            McpCatalogOwnerErrorCode::TooManySkills
        );

        let tool_owner = McpProductCatalogOwner::with_limits(
            McpProductCatalogLimits::new(2, 1, 1_000_000).unwrap(),
        );
        assert_eq!(
            tool_owner
                .replace_all(0, vec![catalog("a", &["x", "y"])])
                .unwrap_err()
                .code,
            McpCatalogOwnerErrorCode::TooManyTools
        );

        let bytes_owner =
            McpProductCatalogOwner::with_limits(McpProductCatalogLimits::new(2, 4, 1).unwrap());
        let error = bytes_owner
            .replace_all(
                0,
                vec![catalog("secret-namespace-canary", &["secret-tool-canary"])],
            )
            .unwrap_err();
        assert_eq!(error.code, McpCatalogOwnerErrorCode::CatalogTooLarge);
        let diagnostic = serde_json::to_string(&error).unwrap();
        assert!(!diagnostic.contains("secret"));
    }

    #[test]
    fn stale_and_exhausted_revisions_preserve_the_previous_snapshot() {
        let owner = McpProductCatalogOwner::new();
        let first = owner
            .replace_all(0, vec![catalog("stable", &["read"])])
            .unwrap()
            .into_snapshot();
        assert_eq!(
            owner
                .replace_all(0, vec![catalog("replacement", &["write"])])
                .unwrap_err()
                .code,
            McpCatalogOwnerErrorCode::StaleRevision
        );
        assert!(Arc::ptr_eq(&first, &owner.snapshot().unwrap()));

        let exhausted = Arc::new(
            PreparedSnapshot::new(vec![catalog("stable", &["read"])], owner.limits())
                .unwrap()
                .publish(u64::MAX),
        );
        *owner.state.write().unwrap() = Arc::clone(&exhausted);
        assert_eq!(
            owner
                .replace_all(u64::MAX, vec![catalog("new", &["write"])])
                .unwrap_err()
                .code,
            McpCatalogOwnerErrorCode::RevisionExhausted
        );
        assert!(Arc::ptr_eq(&exhausted, &owner.snapshot().unwrap()));
    }

    #[test]
    fn concurrent_same_revision_writers_have_exactly_one_winner() {
        let owner = Arc::new(McpProductCatalogOwner::new());
        let barrier = Arc::new(Barrier::new(9));
        let mut workers = Vec::new();
        for index in 0..8 {
            let owner = Arc::clone(&owner);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                let namespace = format!("worker-{index}");
                let candidate = catalog(&namespace, &["read"]);
                barrier.wait();
                owner.replace_all(0, vec![candidate])
            }));
        }
        barrier.wait();
        let outcomes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert!(outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .all(|error| error.code == McpCatalogOwnerErrorCode::StaleRevision));
        assert_eq!(owner.snapshot().unwrap().revision(), 1);
    }

    #[test]
    fn maximum_owner_work_is_iterative_on_a_small_stack() {
        let catalogs = (0..HARD_MAX_PUBLISHED_MCP_SKILLS)
            .map(|index| catalog(&format!("skill-{index:03}"), &["a", "b", "c", "d"]))
            .collect();
        thread::Builder::new()
            .stack_size(96 * 1024)
            .spawn(move || {
                let owner = McpProductCatalogOwner::new();
                let snapshot = owner.replace_all(0, catalogs).unwrap().into_snapshot();
                assert_eq!(snapshot.catalog_count(), HARD_MAX_PUBLISHED_MCP_SKILLS);
                assert_eq!(snapshot.tool_count(), HARD_MAX_PUBLISHED_MCP_SKILLS * 4);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn poisoned_state_fails_closed_with_fixed_diagnostics() {
        let owner = Arc::new(McpProductCatalogOwner::new());
        let poison_owner = Arc::clone(&owner);
        let _ = thread::spawn(move || {
            let _guard = poison_owner.state.write().unwrap();
            panic!("poison catalog owner for test");
        })
        .join();
        let error = owner.snapshot().unwrap_err();
        assert_eq!(error.code, McpCatalogOwnerErrorCode::StateUnavailable);
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"code":"state_unavailable","field":"catalog_state","message":"the MCP product catalog state is unavailable"}"#
        );
    }
}
