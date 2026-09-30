//! Apps adapter for reviewed Universal Skill Runtime CLI actions.
//!
//! The adapter admits exact skill bytes and their compiled typed-action argv
//! schema, then attests only physical identity to the common app effect kernel.
//! It does not mint app authority, resource permits or disclosure permits. The
//! actual child is still owned by tool-runtime-core's authorization-before-auth
//! coordinator and strict process jail.

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{mpsc, Arc, OnceLock},
    time::{Duration as StdDuration, Instant},
};

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tool_runtime_core::{
    action_overrides::{
        compile_typed_action_overrides, lower_typed_action_invocation, CompiledActionCatalog,
        CompiledTypedAction, EffectiveActionPolicy, TypedActionParameter, TypedActionRoute,
        TypedArgumentMapping, WorkspacePathAccess,
    },
    credential_injection::{
        ChildEnvironmentBaseline, ChildEnvironmentVariable, CredentialCallId,
        CredentialInjectionPlan,
    },
    credential_materialization::ChildEnvironmentValues,
    credential_preparation::{
        CredentialMaterialBindingName, CredentialMaterialKind, CredentialMaterialResolver,
        CredentialMaterialSink, CredentialPreparationBinding, CredentialPreparationError,
        CredentialPreparationPlan,
    },
    credential_profiles::{CredentialProfileBinding, CredentialScope},
    governed_batch_process::GovernedBatchCancellation,
    governed_execution::{
        GovernedExecutionContract, GovernedExecutionDispatch, GovernedExecutionPolicy,
        GovernedExecutionRequest, GovernedExecutionTerminal, GovernedExecutionTerminalState,
    },
    governed_execution_authority::{
        GovernedExpectedExecutableDigest, GovernedWorkingDirectoryRoot,
    },
    governed_execution_coordinator::{
        GovernedAuthorizationDecision, GovernedAuthorizationEvidence, GovernedAuthorizationRequest,
        GovernedExecutionAuditError, GovernedExecutionAuditReceipt, GovernedExecutionAuditSink,
        GovernedExecutionAuthorizer, GovernedExecutionCallContext,
        GovernedExecutionCoordinatorError, GovernedExecutionInvocation,
    },
    governed_process_jail::{
        GovernedEgressBrokerEndpoint, GovernedJailInterpreter, GovernedProcessJail,
        GovernedProcessJailError, GovernedProcessJailLimits, GOVERNED_JAIL_EXEC_ROOTS_INPUT_DIRECTORY,
        GOVERNED_JAIL_EXEC_ROOTS_V1, GOVERNED_PROCESS_JAIL_BROKERED_EGRESS_V1,
        GOVERNED_PROCESS_JAIL_V1,
    },
    manifest::{
        AuthContract, AuthKind, CliInteraction, InjectionSource, PolicyFloor, RuntimeProtocol,
        SkillRuntimeContract, WorkingDirectoryMode,
    },
    manifest_parser::{parse_skill_frontmatter, parse_skill_runtime_package, SkillRuntimePackage},
    manifest_validation::validate_skill_runtime_contract,
    profile_selection::{select_credential_profile, CredentialProfileSelectionRequest},
};

use super::{
    effect_kernel::{
        AppEffectInFlight, AppEffectKernelError, AppEffectPhysicalOwner, AppEffectPhysicalTarget,
        AppEffectProviderIoAuthorization, AppEffectSettlement, AppEffectStage,
        MAX_APP_EFFECT_RESULT_BYTES,
    },
    models::{AppDigest, AppReference, AppRevision},
    os_jail_egress::{
        parse_app_egress_declaration, AppOsJailEgressAdmission, AppOsJailEgressBroker,
        AppOsJailEgressDeclaration, AppOsJailEgressReceipt, AppOsJailNetworkGrant,
        APP_OS_JAIL_EGRESS_LIMITS, APP_OS_JAIL_EGRESS_PORT, APP_OS_JAIL_EGRESS_PROFILE_V1,
    },
    os_jail_in_place::{
        declared_companions, derive_in_place_skill, read_entry_head, skill_package_root,
        verify_in_place_skill, AppInPlaceHost, AppInPlaceLaunch, AppInPlaceSkill,
        AppInPlaceSkillError, AppPrivateCredentialRoot, APP_OS_JAIL_IN_PLACE_PROFILE_V1,
        APP_OS_JAIL_IN_PLACE_REF_PREFIX,
    },
    package_lock::{AppLockedPrimitiveActionBinding, AppLockedPrimitiveBinding},
    policy::{AppEndpointClass, AttestedAppEndpoint},
    tool_disclosure::AttestedAppToolTarget,
    tool_eligibility::{assess_app_tool_eligibility, AppToolAdmissionSource},
};
use crate::magician_v2::{execution::actions::ActionResult, json_traversal::canonical_json_bytes};

pub(crate) const APP_OS_JAIL_PROFILE_V1: &str = "magician.app-os-jail.v1";
/// Qualified implementation witness covered by package-lock plan identities.
/// Any change to lowering, executable admission, environment construction or
/// the strict jail recipe must mint a new value before it can dispatch an old
/// reviewed lock.
pub(crate) const APP_OS_JAIL_IMPLEMENTATION_REVISION: &str =
    "magician.app-os-jail-implementation.2026-08-22.1";
pub(crate) const MAX_APP_OS_JAIL_INPUT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_APP_OS_JAIL_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_APP_OS_JAIL_JSON_DEPTH: usize = 32;
pub(crate) const MAX_APP_OS_JAIL_JSON_NODES: usize = 32 * 1024;
pub(crate) const MAX_APP_OS_JAIL_PRIVATE_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
/// Exact raw completion is later projected through the bounded labeled-result
/// lane. Larger results require a separate sealed blob owner; they must not be
/// admitted merely because the process jail can hold them in memory.
pub(crate) const MAX_APP_OS_JAIL_DURABLE_RESULT_BYTES: u64 = 56 * 1024;
pub(crate) const MAX_APP_OS_JAIL_AUDIT_BYTES: usize = 8 * 1024;
const MAX_APP_OS_JAIL_BLOCKING_WORKERS: usize = 4;
const APP_OS_JAIL_BLOCKING_RESERVATION_TIMEOUT: StdDuration = StdDuration::from_secs(30);
const APP_OS_JAIL_BLOCKING_SLOT_LIFETIME: StdDuration = StdDuration::from_secs(30);
/// Worst-case JSON string expansion plus the fixed typed-result envelope. The
/// reviewed transport ceiling is derived from the authored stdout/stderr
/// limits; this allowance only converts those already-finite streams into the
/// canonical `ActionResult` representation and is not an implicit output
/// policy.
const APP_OS_JAIL_RESULT_JSON_EXPANSION: u64 = 6;
const APP_OS_JAIL_RESULT_ENVELOPE_BYTES: u64 = 4 * 1024;
/// App-run stream budget. Many skills author multi-megabyte limits for agent
/// use; an app result must fit the durable ceiling, so their app runs are
/// budgeted instead of refused. A larger answer fails as invalid output rather
/// than being cut short. Skills already inside the budget are unchanged.
const APP_OS_JAIL_STDOUT_BUDGET: u64 = 7 * 1024;
const APP_OS_JAIL_STDERR_BUDGET: u64 = 1024;
/// How long an egress endpoint attestation stays admissible after the action
/// is prepared. It covers disclosure, resource admission and dispatch.
const APP_OS_JAIL_EGRESS_ATTESTATION_SECONDS: i64 = 15 * 60;

#[derive(Debug, Error)]
pub enum AppOsJailError {
    #[error("reviewed app skill bytes are invalid or not app-exposed")]
    InvalidSource,
    #[error("app OS-jail execution requires a typed non-interactive CLI action")]
    UnsupportedRuntime,
    #[error("the exact locked primitive/action does not match the reviewed skill bytes")]
    IdentityMismatch,
    #[error("app OS-jail input is invalid or exceeds its structural bounds")]
    InvalidInput,
    #[error("app OS-jail actions cannot accept caller-selected paths or runtime controls")]
    AmbientAuthorityRequested,
    #[error("app OS-jail output is invalid or exceeds its structural bounds")]
    InvalidOutput,
    #[error("the reviewed private executable artifact is unavailable or changed")]
    PhysicalArtifactUnavailable,
    #[error("the bounded app OS-jail blocking executor is unavailable")]
    BlockingWorkerUnavailable,
    #[error("the absolute app resource I/O deadline elapsed")]
    ResourceDeadlineExceeded,
    #[error("the app egress broker could not be started")]
    EgressUnavailable,
    #[error("no trusted host interpreter is available for this script skill")]
    InterpreterUnavailable,
    #[error("the owner has not granted this app tool the secret it requires")]
    SecretNotGranted,
    #[error("the skill changed since the app was approved; re-approve the app to use it")]
    SkillChangedSinceApproval,
    #[error("this tool cannot run in place: {0}")]
    InPlaceUnavailable(String),
    #[error(transparent)]
    Jail(#[from] GovernedProcessJailError),
    #[error(transparent)]
    Execution(#[from] GovernedExecutionCoordinatorError),
}

#[derive(Deserialize)]
struct AppOsJailSkillHeader {
    name: String,
}

struct AppOsJailActionPlan {
    action_ref: Option<AppReference>,
    input_schema_digest: AppDigest,
    /// `None` for an action the jail refuses (it needs authority): the skill's
    /// other actions stay usable, and this one can never be lowered.
    execution_plan_digest: Option<AppDigest>,
    transport_result_byte_ceiling: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppOsJailPhysicalArtifactIdentity {
    revision_ref: AppReference,
    digest: AppDigest,
}

impl AppOsJailPhysicalArtifactIdentity {
    pub(crate) fn revision_ref(&self) -> &AppReference {
        &self.revision_ref
    }

    pub(crate) fn digest(&self) -> &AppDigest {
        &self.digest
    }
}

/// Move-only reopened artifact capability. The path is host-private and is
/// produced only by resolving a locked content-addressed revision beneath the
/// app artifact store; callers cannot provide it in an invocation.
pub(crate) struct AppOsJailVerifiedArtifact {
    identity: AppOsJailPhysicalArtifactIdentity,
    executable_directory: PathBuf,
    kind: AppOsJailArtifactKind,
    /// The validated in-place derivation of an `InPlaceSkill` artifact.
    in_place: Option<Box<AppInPlaceSkill>>,
}

/// What a reviewed private artifact is. It is a pure function of the
/// artifact's digest-pinned leading bytes, so the lock that pins the digest
/// already pins the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppOsJailArtifactKind {
    /// A Mach-O or ELF executable, exec'd directly.
    Native,
    /// A single-file Python 3 script with exactly `#!/usr/bin/env python3` or
    /// `#!/usr/bin/python3` as its first line. It runs under the host's
    /// pinned, trusted interpreter (`GovernedJailInterpreter`); the jail may
    /// exec only that interpreter, never the script.
    Python3Script,
    /// A root-owned native tool in `/usr/bin` or `/bin` (on macOS the sealed
    /// system volume, run in place; on Linux root-owned all the way up,
    /// snapshotted by MagicRun), pinned by digest. An OS update that changes
    /// it invalidates the lock until the app is re-reviewed.
    SystemTool,
    /// Any other skill (`app_in_place_skill_v1`): run in place from its
    /// package in MagicRun's exec-roots jail, reading and executing only the
    /// roots derived from the package (the skill less `config/`, the shared
    /// `node_modules`, its runtimes' install prefixes). Never copied; the lock
    /// pins the exec-roots profile identity and a fingerprint of the package.
    InPlaceSkill,
}

/// Directories a skill without its own `bin/` may take a trusted system tool
/// from. Only files on the sealed, read-only system volume qualify.
const APP_OS_JAIL_SYSTEM_TOOL_DIRECTORIES: [&str; 2] = ["/usr/bin", "/bin"];
const APP_OS_JAIL_SYSTEM_TOOL_REF_PREFIX: &str = "artifact:app-os-jail-system:v1:";

/// Longest leading byte run any kind needs to be recognised.
const APP_OS_JAIL_ARTIFACT_HEAD_BYTES: usize = 32;
const PYTHON3_SHEBANGS: [&[u8]; 2] = [b"#!/usr/bin/env python3\n", b"#!/usr/bin/python3\n"];

fn artifact_kind(head: &[u8]) -> Option<AppOsJailArtifactKind> {
    if head
        .get(..4)
        .and_then(|magic| <[u8; 4]>::try_from(magic).ok())
        .is_some_and(is_native_executable_magic)
    {
        return Some(AppOsJailArtifactKind::Native);
    }
    PYTHON3_SHEBANGS
        .iter()
        .any(|shebang| head.starts_with(shebang))
        .then_some(AppOsJailArtifactKind::Python3Script)
}

/// Read up to [`APP_OS_JAIL_ARTIFACT_HEAD_BYTES`], stopping early only at EOF.
fn read_artifact_head(file: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut head = vec![0_u8; APP_OS_JAIL_ARTIFACT_HEAD_BYTES];
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    head.truncate(filled);
    Ok(head)
}

/// Broker-owned scope store. Construction derives the only admitted
/// artifact location from the trusted workspace apps root; app input never
/// supplies a host path or store override.
#[derive(Clone, PartialEq, Eq)]
pub struct AppOsJailArtifactStore {
    root: PathBuf,
}

/// Bounded, canonical control-plane evidence for one governed execution audit.
/// The receipt remains value-free, but unlike the transient runtime carrier it
/// can be sealed beside the workflow completion/uncertainty correlation. A lost
/// worker may legitimately have no receipt; that absence is explicit and is
/// still bound to the post-start dispatch classification.
pub(crate) struct AppOsJailAuditEvidence {
    dispatch: GovernedExecutionDispatch,
    canonical_receipt: Option<Vec<u8>>,
    receipt_digest: Option<AppDigest>,
}

impl AppOsJailAuditEvidence {
    fn from_runtime(
        dispatch: GovernedExecutionDispatch,
        receipt: Option<&GovernedExecutionAuditReceipt>,
    ) -> Result<Self, AppOsJailError> {
        let (canonical_receipt, receipt_digest) = match receipt {
            Some(receipt) => {
                if receipt.terminal.dispatch() != dispatch {
                    return Err(AppOsJailError::IdentityMismatch);
                }
                let value =
                    serde_json::to_value(receipt).map_err(|_| AppOsJailError::InvalidOutput)?;
                let bytes =
                    canonical_json_bytes(&value).map_err(|_| AppOsJailError::InvalidOutput)?;
                if bytes.is_empty() || bytes.len() > MAX_APP_OS_JAIL_AUDIT_BYTES {
                    return Err(AppOsJailError::InvalidOutput);
                }
                let digest = AppDigest::blake3(&bytes);
                (Some(bytes), Some(digest))
            },
            None => (None, None),
        };
        Ok(Self {
            dispatch,
            canonical_receipt,
            receipt_digest,
        })
    }

    /// Preserve the dispatch correlation when a post-start worker cannot
    /// supply a bounded canonical receipt. This is admissible only for an
    /// uncertain workflow settlement; successful completion always requires
    /// the exact receipt bytes and digest.
    pub(crate) fn receipt_unavailable(dispatch: GovernedExecutionDispatch) -> Self {
        Self {
            dispatch,
            canonical_receipt: None,
            receipt_digest: None,
        }
    }

    pub(crate) fn dispatch_name(&self) -> &'static str {
        match self.dispatch {
            GovernedExecutionDispatch::NotDispatched => "not_dispatched",
            GovernedExecutionDispatch::Dispatched => "dispatched",
            GovernedExecutionDispatch::UnknownAfterDispatch => "unknown_after_dispatch",
        }
    }

    pub(crate) fn canonical_receipt(&self) -> Option<&[u8]> {
        self.canonical_receipt.as_deref()
    }

    pub(crate) fn receipt_digest(&self) -> Option<&AppDigest> {
        self.receipt_digest.as_ref()
    }
}

impl AppOsJailArtifactStore {
    pub fn open_or_create(apps_root: &Path) -> Result<Self, AppOsJailError> {
        let root = open_artifact_store(apps_root)?;
        Ok(Self { root })
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

impl AppOsJailVerifiedArtifact {
    fn identity(&self) -> &AppOsJailPhysicalArtifactIdentity {
        &self.identity
    }

}

/// Move-only exact action plan produced from reviewed source plus one locked
/// action. The executable name and effective policy stay inseparable from the
/// opaque governed request, so a workflow cannot accidentally compile the base
/// package entrypoint while dispatching action-specific argv.
pub(crate) struct AppOsJailPreparedAction {
    skill_name: String,
    source_digest: AppDigest,
    primitive_ref: AppReference,
    action_name: String,
    action_ref: AppReference,
    input_schema_digest: AppDigest,
    execution_plan_digest: AppDigest,
    transport_result_byte_ceiling: u64,
    physical_artifact_revision_ref: AppReference,
    physical_artifact_digest: AppDigest,
    executable_directory: PathBuf,
    jail_limits: GovernedProcessJailLimits,
    effective_contract: SkillRuntimeContract,
    request: GovernedExecutionRequest,
    spend_gate: Option<crate::magician_v2::resource_authority::spend_gate::SpendGate>,
    egress: Option<AppOsJailEgressDeclaration>,
    prepared_at: chrono::DateTime<chrono::Utc>,
    artifact_kind: AppOsJailArtifactKind,
    secret_authority: Option<AppOsJailSecretAuthority>,
    /// File contents the app supplied for read-only file inputs, staged into
    /// the jail's private workdir under these names before launch.
    staged_inputs: Vec<(String, Vec<u8>)>,
    /// The validated in-place derivation of an `InPlaceSkill` artifact.
    in_place: Option<Box<AppInPlaceSkill>>,
    /// The owner's live network grant for this app, resolved by the workflow
    /// for a network-capable tool. `None` keeps the declared-host behaviour.
    network_grant: Option<AppOsJailNetworkGrant>,
}

/// The owner's secret authority for one app jail call: the scope whose vault
/// holds the secrets and the secret references the reviewed grant allows for
/// this exact tool. Built by the workflow from the installation's grant; the
/// vault's own per-secret policy still applies when the value is issued.
pub(crate) struct AppOsJailSecretAuthority {
    pub(crate) store_resolver: Arc<crate::magician_v2::secrets::SecretStoreResolver>,
    pub(crate) principal: String,
    pub(crate) workspace: String,
    pub(crate) granted: BTreeSet<String>,
    /// For an any-host or undeclared tool: each granted key's owner-chosen
    /// scope (`secret_ref` → picked hosts or "any site"). Such a key is used
    /// only with a scope; while it is injected the broker admits only its
    /// scope.
    pub(crate) key_scopes: BTreeMap<String, AppOsJailKeyScope>,
}

/// Where one key of an undeclared or any-host tool may go, as the owner
/// chose at install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppOsJailKeyScope {
    Hosts(BTreeSet<String>),
    AnySite,
}

impl AppOsJailKeyScope {
    /// From a grant's `hosts` / `any_site`; `None` when it chose neither.
    pub(crate) fn from_grant(hosts: &[String], any_site: bool) -> Option<Self> {
        if any_site {
            Some(Self::AnySite)
        } else if hosts.is_empty() {
            None
        } else {
            Some(Self::Hosts(hosts.iter().cloned().collect()))
        }
    }
}

/// The domains one call's keys may reach, as the vault request should carry
/// them: exactly the hosts the call's broker admits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppOsJailVaultDomains {
    /// No network: the key reaches no host.
    None,
    One(String),
    /// Several hosts, sorted and unique.
    Set(Vec<String>),
    /// Any public host (a key only with the owner's explicit "any site").
    Any,
}

pub(crate) fn vault_domains(admission: Option<&AppOsJailEgressAdmission>) -> AppOsJailVaultDomains {
    match admission {
        None => AppOsJailVaultDomains::None,
        Some(AppOsJailEgressAdmission::AnyPublicHost) => AppOsJailVaultDomains::Any,
        Some(AppOsJailEgressAdmission::Hosts { hosts }) => match hosts.as_slice() {
            [host] => AppOsJailVaultDomains::One(host.clone()),
            _ => {
                let mut hosts = hosts.clone();
                hosts.sort();
                hosts.dedup();
                AppOsJailVaultDomains::Set(hosts)
            },
        },
    }
}

/// The one switch point to the vault's domain scope: the vault request
/// carries exactly the domains the call's broker admits — none, one host, a
/// set, or every site (`Any`, allowed by the vault only for a key whose
/// policy is unrestricted or lists `*`). The vault does no public/private
/// filtering for a set or `Any`; the call's broker refuses every
/// non-public address, for every admitted host.
pub(crate) fn credential_route_domains(
    domains: &AppOsJailVaultDomains,
) -> Result<crate::magician_v2::secrets::policy::RequestedDomains, AppOsJailError> {
    use crate::magician_v2::secrets::policy::RequestedDomains;
    Ok(match domains {
        AppOsJailVaultDomains::None => RequestedDomains::None,
        AppOsJailVaultDomains::One(host) => RequestedDomains::One(host.clone()),
        AppOsJailVaultDomains::Set(hosts) => {
            RequestedDomains::hosts(hosts).map_err(|_| AppOsJailError::SecretNotGranted)?
        },
        AppOsJailVaultDomains::Any => RequestedDomains::Any,
    })
}

/// Largest secret value an app jail call accepts, matching the governed
/// runtime's static-secret bound.
const MAX_APP_OS_JAIL_SECRET_BYTES: usize = 16 * 1024;

impl AppOsJailPreparedAction {
    pub(crate) fn with_secret_authority(
        mut self,
        secret_authority: Option<AppOsJailSecretAuthority>,
    ) -> Self {
        self.secret_authority = secret_authority;
        self
    }

    /// Whether the reviewed tool uses any secret, so the workflow knows to
    /// resolve the owner's grant.
    pub(crate) fn uses_secrets(&self) -> bool {
        self.effective_contract.auth.kind != AuthKind::None
    }

    /// Whether the tool can reach the network at all (it declares hosts, or
    /// runs in place and may be granted them), so the workflow knows to
    /// resolve the owner's live network grant.
    pub(crate) fn network_capable(&self) -> bool {
        self.egress.is_some() || self.in_place.is_some()
    }

    pub(crate) fn with_network_grant(mut self, grant: Option<AppOsJailNetworkGrant>) -> Self {
        self.network_grant = grant;
        self
    }

    /// Whether this tool's keys need an owner-chosen scope: it declares no
    /// host (in place), or declares arbitrary sites (`*`).
    fn keys_need_scope(&self) -> bool {
        match &self.egress {
            None => self.in_place.is_some(),
            Some(declared) => declared.any_host(),
        }
    }

    /// The scope the keys this call will receive narrow its network to:
    /// the intersection of every picked host set (and the app's granted
    /// hosts), or any public host when every key may go to any site (which
    /// still needs the app's effective "any public host" grant). `None` when
    /// no scoped key is used.
    fn key_scope_admission(&self) -> Result<Option<AppOsJailEgressAdmission>, AppOsJailError> {
        let auth = &self.effective_contract.auth;
        let Some(authority) = self.secret_authority.as_ref() else {
            return Ok(None);
        };
        if !self.keys_need_scope() || auth.kind == AuthKind::None {
            return Ok(None);
        }
        let scopes = auth
            .secret_bindings
            .iter()
            .filter(|binding| authority.granted.contains(&binding.secret_ref))
            .filter_map(|binding| authority.key_scopes.get(&binding.secret_ref))
            .collect::<Vec<_>>();
        if scopes.is_empty() {
            return Ok(None);
        }
        let grant = self
            .network_grant
            .as_ref()
            .ok_or(AppOsJailError::SecretNotGranted)?;
        let mut hosts: Option<BTreeSet<String>> = None;
        for scope in scopes {
            if let AppOsJailKeyScope::Hosts(picked) = scope {
                let allowed = picked
                    .iter()
                    .filter(|host| grant.hosts.contains(*host))
                    .cloned()
                    .collect::<BTreeSet<_>>();
                hosts = Some(match hosts {
                    None => allowed,
                    Some(current) => current.intersection(&allowed).cloned().collect(),
                });
            }
        }
        match hosts {
            Some(hosts) if hosts.is_empty() => Err(AppOsJailError::SecretNotGranted),
            Some(hosts) => Ok(Some(AppOsJailEgressAdmission::Hosts {
                hosts: hosts.into_iter().collect(),
            })),
            None if grant.any_public_host => Ok(Some(AppOsJailEgressAdmission::AnyPublicHost)),
            None => Err(AppOsJailError::SecretNotGranted),
        }
    }

    /// The hosts this call's broker admits (none: no network). A key scoped
    /// by the owner narrows the call to its scope.
    fn egress_admission(&self) -> Result<Option<AppOsJailEgressAdmission>, AppOsJailError> {
        if let Some(scoped) = self.key_scope_admission()? {
            return Ok(Some(scoped));
        }
        Ok(AppOsJailEgressAdmission::for_call(
            self.egress.as_ref(),
            self.network_grant.as_ref(),
            self.in_place.is_some(),
        ))
    }
    pub(crate) fn with_spend_gate(
        mut self,
        spend_gate: Option<crate::magician_v2::resource_authority::spend_gate::SpendGate>,
    ) -> Self {
        self.spend_gate = spend_gate;
        self
    }

    pub(crate) fn skill_name(&self) -> &str {
        &self.skill_name
    }

    pub(crate) fn take_spend_gate(
        &mut self,
    ) -> Option<crate::magician_v2::resource_authority::spend_gate::SpendGate> {
        self.spend_gate.take()
    }

    fn jail_profile(
        &self,
        broker: Option<GovernedEgressBrokerEndpoint>,
        private_root: Option<&Path>,
    ) -> Result<(GovernedProcessJail, GovernedWorkingDirectoryRoot), AppOsJailError> {
        let mut jail = match broker {
            Some(endpoint) => {
                GovernedProcessJail::strict_app_with_brokered_egress(self.jail_limits, endpoint)?
            },
            None => GovernedProcessJail::strict_app(self.jail_limits)?,
        };
        let pinned_python = self.artifact_kind == AppOsJailArtifactKind::Python3Script
            || self
                .in_place
                .as_ref()
                .is_some_and(|in_place| in_place.launch() == AppInPlaceLaunch::PinnedPython3);
        if pinned_python {
            // Discovered per call: the interpreter is re-trusted and its
            // digest re-checked here and again immediately before launch.
            let interpreter = GovernedJailInterpreter::python3_for_host()
                .map_err(|_| AppOsJailError::InterpreterUnavailable)?;
            jail = jail
                .with_interpreter(interpreter)
                .map_err(|_| AppOsJailError::InterpreterUnavailable)?;
        }
        if let Some(in_place) = &self.in_place {
            // The roots are revalidated here and again at launch; a runtime
            // that stopped passing the trust checks refuses the call.
            let roots = in_place
                .exec_roots(private_root)
                .map_err(|error| AppOsJailError::InPlaceUnavailable(error.to_string()))?;
            jail = jail.with_exec_roots(roots)?;
        }
        let mode = match &self.effective_contract.runtime {
            RuntimeProtocol::Cli {
                working_directory, ..
            } => working_directory.mode,
            RuntimeProtocol::Mcp { .. } => return Err(AppOsJailError::UnsupportedRuntime),
        };
        let workdir = jail.working_directory_root(mode)?;
        Ok((jail, workdir))
    }

    /// The sole physical execution implementation. The eventual common effect
    /// owner wrapper supplies only its already-started call context; governed
    /// authorization and audit capture remain sealed here with every
    /// process-bearing detail. Keeping this method private prevents an app
    /// workflow from bypassing durable start or substituting an authorizer.
    fn execute_governed(
        mut self,
        context: GovernedExecutionCallContext,
        cancellation: &GovernedBatchCancellation,
    ) -> Result<AppOsJailExecution, AppOsJailExecutionFailure> {
        // The hosts this call may reach: the declared ones the app is
        // granted, every granted host for an in-place skill that declares
        // none, or any public host under the owner's explicit grant.
        let admission = self.egress_admission()?;
        // A secret's vault grant is scoped to the one reachable host when
        // there is exactly one; otherwise it carries no domain, so a key
        // whose vault policy is domain-restricted is refused.
        let key_domains = credential_route_domains(&vault_domains(admission.as_ref()))?;
        let key_needs_scope = self.keys_need_scope();
        self.egress = None;
        // The call's own broker; dropping it on any early return stops every
        // tunnel.
        let broker = admission
            .as_ref()
            .map(|admission| {
                AppOsJailEgressBroker::start(admission)
                    .map_err(|_| AppOsJailError::EgressUnavailable)
            })
            .transpose()?;
        // Keep only the secrets the owner granted this tool. A required one
        // that is not granted refuses the call before anything launches.
        narrow_auth_to_granted_secrets(
            &mut self.effective_contract.auth,
            self.secret_authority.as_ref(),
            key_needs_scope,
        )?;
        // A key delivered as a config file lives in a fresh 0700 directory
        // granted to this call's jail read-only, removed with the call.
        let credential_root = if self.in_place.is_some()
            && self.secret_authority.is_some()
            && self
                .effective_contract
                .auth
                .injections
                .iter()
                .any(|injection| {
                    matches!(
                        injection.target,
                        tool_runtime_core::manifest::InjectionTarget::ConfigDirectory { .. }
                    )
                }) {
            Some(
                AppPrivateCredentialRoot::create()
                    .map_err(|error| AppOsJailError::InPlaceUnavailable(error.to_string()))?,
            )
        } else {
            None
        };
        let (jail, workdir) = self.jail_profile(
            broker.as_ref().map(AppOsJailEgressBroker::endpoint),
            credential_root.as_ref().map(AppPrivateCredentialRoot::path),
        )?;
        for (name, bytes) in std::mem::take(&mut self.staged_inputs) {
            let staged = jail
                .stage_input_file(&name, &bytes)
                .map_err(|_| AppOsJailError::InvalidInput)?;
            // Lowering already named the file where this jail stages it.
            let expected = if self.in_place.is_some() {
                format!("{GOVERNED_JAIL_EXEC_ROOTS_INPUT_DIRECTORY}/{name}")
            } else {
                name
            };
            if staged != expected {
                return Err(AppOsJailError::InvalidInput.into());
            }
        }
        let RuntimeProtocol::Cli { limits, .. } = &self.effective_contract.runtime else {
            return Err(AppOsJailError::UnsupportedRuntime.into());
        };
        let timeout = limits
            .timeout_secs
            .ok_or(AppOsJailError::IdentityMismatch)?;
        // A skill without stdin has no allowance in its contract, which is
        // what denies stdin; the governed policy still needs a non-zero
        // ceiling, and it can admit nothing the contract refuses.
        let stdin = limits.stdin_bytes.unwrap_or(1);
        let stdout = limits
            .stdout_bytes
            .ok_or(AppOsJailError::IdentityMismatch)?;
        let stderr = limits
            .stderr_bytes
            .ok_or(AppOsJailError::IdentityMismatch)?;
        let validated = validate_skill_runtime_contract(&self.effective_contract)
            .map_err(|_| AppOsJailError::IdentityMismatch)?;
        let contract = GovernedExecutionContract::compile(
            validated,
            GovernedExecutionPolicy::new(timeout, timeout, stdin, stdout, stderr)
                .map_err(|_| AppOsJailError::IdentityMismatch)?,
        )
        .map_err(|_| AppOsJailError::IdentityMismatch)?;
        let intent = contract
            .admit(self.request)
            .map_err(|_| AppOsJailError::IdentityMismatch)?;

        let secrets = self
            .secret_authority
            .as_ref()
            .filter(|_| self.effective_contract.auth.kind != AuthKind::None);
        let scope = match secrets {
            Some(authority) => CredentialScope::new(&authority.principal, &authority.workspace),
            None => CredentialScope::new("app-runtime", "os-jail"),
        }
        .map_err(|_| AppOsJailError::IdentityMismatch)?;
        let preparation = match secrets {
            Some(_) => secret_preparation_plan(validated, scope.clone())?,
            None => CredentialPreparationPlan::unauthenticated(scope.clone()),
        };
        let injection = CredentialInjectionPlan::compile(
            validated,
            &preparation,
            ChildEnvironmentBaseline::path_only(),
        )
        .map_err(|_| AppOsJailError::IdentityMismatch)?;
        let mut environment = ChildEnvironmentValues::new(injection.baseline());
        let credential_scratch = match (&credential_root, secrets) {
            (Some(root), Some(authority)) => Some(private_credential_scratch(
                root.path(),
                &authority.principal,
                &authority.workspace,
                &scope,
            )?),
            _ => None,
        };
        let path = self
            .executable_directory
            .to_str()
            .ok_or(AppOsJailError::PhysicalArtifactUnavailable)?;
        environment
            .provide(ChildEnvironmentVariable::Path, path.as_bytes().to_vec())
            .map_err(|_| AppOsJailError::IdentityMismatch)?;
        // The contract's fixed public environment, as the governed runtime
        // provides it.
        for (name, value) in &self.effective_contract.requires.environment {
            environment
                .provide_fixed(name.clone(), value.as_bytes().to_vec())
                .map_err(|_| AppOsJailError::IdentityMismatch)?;
        }
        let expected = GovernedExpectedExecutableDigest::from_blake3(app_digest_blake3_bytes(
            &self.physical_artifact_digest,
        )?);
        let invocation = GovernedExecutionInvocation::new(
            context,
            validated,
            intent,
            &preparation,
            &injection,
            environment,
            Some(workdir),
            None,
            credential_scratch.as_ref().map(|scratch| {
                tool_runtime_core::governed_execution_coordinator::GovernedCredentialFilesystemRequest::new(
                    scratch, None,
                )
            }),
        )?
        .with_expected_executable_digest(expected);
        let mut no_credentials = AppOsJailNoCredentialResolver;
        let mut vault;
        let resolver: &mut dyn CredentialMaterialResolver = match secrets {
            // The existing sealed one-shot adapter: it issues and redeems a
            // short-lived vault grant bound to this tool, action and egress
            // host, so the vault's own per-secret policy applies too.
            Some(authority) => {
                let route = crate::magician_v2::secrets::credential_material_adapter::CredentialGrantRoute::new_scoped(
                    self.skill_name.clone(),
                    self.action_name.clone(),
                    key_domains.clone(),
                )
                .map_err(|_| AppOsJailError::IdentityMismatch)?;
                vault = crate::magician_v2::secrets::credential_material_adapter::ScopedCredentialMaterialAdapter::for_secret_references(
                    Arc::clone(&authority.store_resolver),
                    validated,
                    &preparation,
                    route,
                )
                .map_err(|_| AppOsJailError::SecretNotGranted)?;
                &mut vault
            },
            None => &mut no_credentials,
        };
        let mut authorizer = AppOsJailGovernedAuthorizer;
        let mut audit = AppOsJailAuditSink::default();
        let settlement = match invocation.execute_batch_in_jail(
            jail,
            &mut authorizer,
            resolver,
            &mut audit,
            cancellation,
        ) {
            Ok(settlement) => settlement,
            Err(error) => {
                return Err(AppOsJailExecutionFailure::governed(
                    error,
                    audit.recorded.take(),
                ));
            },
        };
        let (result, audit_receipt) = settlement.into_parts();
        let egress = broker.map(AppOsJailEgressBroker::finish);
        let dispatch = result.terminal().dispatch();
        if audit.recorded.as_ref() != Some(&audit_receipt) {
            return Err(AppOsJailExecutionFailure::after_dispatch(
                AppOsJailError::IdentityMismatch,
                dispatch,
                None,
            ));
        }
        // Test builds only: show why a live jailed run's output was refused
        // (MagicRun has already redacted every injected value from it).
        #[cfg(test)]
        if result.stdout_metadata().truncated
            || result.stderr_metadata().truncated
            || !result.artifacts().is_empty()
        {
            eprintln!(
                "[app-os-jail test] truncated={}/{} artifacts={} stdout({} bytes)={:?} stderr({} bytes)={:?}",
                result.stdout_metadata().truncated,
                result.stderr_metadata().truncated,
                result.artifacts().len(),
                result.stdout().len(),
                String::from_utf8_lossy(&result.stdout()[..result.stdout().len().min(1500)]),
                result.stderr().len(),
                String::from_utf8_lossy(&result.stderr()[..result.stderr().len().min(1500)]),
            );
        }
        if result.stdout_metadata().truncated
            || result.stderr_metadata().truncated
            || result
                .stdout()
                .len()
                .checked_add(result.stderr().len())
                .is_none_or(|bytes| bytes > MAX_APP_OS_JAIL_OUTPUT_BYTES)
            || !result.artifacts().is_empty()
        {
            return Err(AppOsJailExecutionFailure::after_dispatch(
                AppOsJailError::InvalidOutput,
                dispatch,
                Some(audit_receipt),
            ));
        }
        let stdout = AppOsJailTypedOutput::parse(result.stdout()).map_err(|error| {
            AppOsJailExecutionFailure::after_dispatch(error, dispatch, Some(audit_receipt.clone()))
        })?;
        // Diagnostics are text: a log line such as `[TOOL] started` is not a
        // malformed JSON document.
        let stderr = AppOsJailTypedOutput::parse_diagnostics(result.stderr()).map_err(|error| {
            AppOsJailExecutionFailure::after_dispatch(error, dispatch, Some(audit_receipt.clone()))
        })?;
        Ok(AppOsJailExecution {
            terminal: result.terminal(),
            exit_code: result.exit_code(),
            stdout,
            stderr,
            audit: audit_receipt,
            transport_result_byte_ceiling: self.transport_result_byte_ceiling,
            egress,
        })
    }

    /// Consume the common post-start provider token inside the already-running
    /// blocking worker. Identity is checked before constructing the governed
    /// call context, and the token is dropped immediately before the sole
    /// physical execution method is entered.
    fn execute_authorized(
        self,
        authorization: AppEffectProviderIoAuthorization,
        cancellation: &GovernedBatchCancellation,
    ) -> Result<AppOsJailExecution, AppOsJailExecutionFailure> {
        let binding = authorization.binding();
        let expected_target = runtime_target_ref(
            &self.execution_plan_digest,
            self.transport_result_byte_ceiling,
            &self.physical_artifact_revision_ref,
            &self.physical_artifact_digest,
        )?;
        if binding.primitive_ref() != &self.primitive_ref
            || binding.action_ref() != &self.action_ref
            || binding.physical_target_ref() != &expected_target
            || binding.result_byte_ceiling() != self.transport_result_byte_ceiling
        {
            return Err(AppOsJailError::IdentityMismatch.into());
        }
        let binding_digest = digest_hex(binding.binding_digest())?;
        let call_id = CredentialCallId::new(format!("app-{binding_digest}"))
            .map_err(|_| AppOsJailError::IdentityMismatch)?;
        let context = GovernedExecutionCallContext::new(
            call_id,
            self.skill_name.clone(),
            self.action_name.clone(),
        )?;
        drop(authorization);
        self.execute_governed(context, cancellation)
    }
}

/// The first app OS-jail vertical admits only the ordinary, authority-free
/// governed policy. This local authorizer cannot widen it: reviewed source and
/// action compilation reject every additive policy axis before a prepared
/// action can exist.
struct AppOsJailGovernedAuthorizer;

impl GovernedExecutionAuthorizer for AppOsJailGovernedAuthorizer {
    fn authorize(
        &mut self,
        request: &GovernedAuthorizationRequest<'_>,
    ) -> GovernedAuthorizationDecision {
        if request.policy_floor() != &PolicyFloor::default() {
            return GovernedAuthorizationDecision::Denied;
        }
        GovernedAuthorizationEvidence::new(
            request.request_digest(),
            APP_OS_JAIL_PROFILE_V1,
            tool_runtime_core::manifest::ApprovalClass::Ordinary,
            None,
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeSet::new(),
        )
        .map_or(
            GovernedAuthorizationDecision::Unavailable,
            GovernedAuthorizationDecision::Approved,
        )
    }
}

#[derive(Default)]
struct AppOsJailAuditSink {
    recorded: Option<GovernedExecutionAuditReceipt>,
}

impl GovernedExecutionAuditSink for AppOsJailAuditSink {
    fn record(
        &mut self,
        receipt: &GovernedExecutionAuditReceipt,
    ) -> Result<(), GovernedExecutionAuditError> {
        if self.recorded.replace(receipt.clone()).is_some() {
            return Err(GovernedExecutionAuditError::unavailable());
        }
        Ok(())
    }
}

struct AppOsJailNoCredentialResolver;

impl CredentialMaterialResolver for AppOsJailNoCredentialResolver {
    fn resolve_once(
        &mut self,
        plan: &CredentialPreparationPlan,
        _sink: &mut CredentialMaterialSink<'_>,
    ) -> Result<(), CredentialPreparationError> {
        if plan.auth_kind() != tool_runtime_core::manifest::AuthKind::None
            || !plan.bindings().is_empty()
        {
            return Err(CredentialPreparationError::resolution(
                tool_runtime_core::credential_preparation::CredentialResolutionFailure::Unavailable,
            ));
        }
        Ok(())
    }
}

/// Typed, bounded terminal projection. Raw process bytes cannot escape this
/// owner; common effect settlement later labels a canonical typed projection.
struct AppOsJailExecution {
    terminal: GovernedExecutionTerminalState,
    exit_code: Option<i32>,
    stdout: AppOsJailTypedOutput,
    stderr: AppOsJailTypedOutput,
    audit: GovernedExecutionAuditReceipt,
    transport_result_byte_ceiling: u64,
    egress: Option<AppOsJailEgressReceipt>,
}

/// One fully observed jailed completion. The common effect owner consumes the
/// action result into its raw completion intent and keeps the governed audit
/// beside that settlement; callers never receive child bytes separately.
struct AppOsJailObservedResult {
    action_result: ActionResult,
    canonical_result_bytes: Vec<u8>,
    successful: bool,
    audit: GovernedExecutionAuditReceipt,
}

/// Failure observed by the already-running blocking owner. Dispatch state is
/// retained independently of the error class, and a coordinator audit that
/// was durably emitted before failure is never discarded. The common effect
/// wrapper uses this sealed outcome to distinguish abort-before-I/O from
/// post-dispatch uncertainty.
struct AppOsJailExecutionFailure {
    error: AppOsJailError,
    dispatch: GovernedExecutionDispatch,
    audit: Option<Box<GovernedExecutionAuditReceipt>>,
}

impl AppOsJailExecutionFailure {
    fn governed(
        error: GovernedExecutionCoordinatorError,
        audit: Option<GovernedExecutionAuditReceipt>,
    ) -> Self {
        let dispatch = error.dispatch();
        Self::after_dispatch(AppOsJailError::Execution(error), dispatch, audit)
    }

    fn after_dispatch(
        error: AppOsJailError,
        dispatch: GovernedExecutionDispatch,
        audit: Option<GovernedExecutionAuditReceipt>,
    ) -> Self {
        Self {
            error,
            dispatch,
            audit: audit.map(Box::new),
        }
    }

    fn worker_lost_after_handoff() -> Self {
        Self::after_dispatch(
            AppOsJailError::BlockingWorkerUnavailable,
            GovernedExecutionDispatch::UnknownAfterDispatch,
            None,
        )
    }
}

impl From<AppOsJailError> for AppOsJailExecutionFailure {
    fn from(error: AppOsJailError) -> Self {
        Self::after_dispatch(error, GovernedExecutionDispatch::NotDispatched, None)
    }
}

impl From<GovernedExecutionCoordinatorError> for AppOsJailExecutionFailure {
    fn from(error: GovernedExecutionCoordinatorError) -> Self {
        Self::governed(error, None)
    }
}

struct AppOsJailBlockingLaunch {
    action: AppOsJailPreparedAction,
    authorization: AppEffectProviderIoAuthorization,
    cancellation: GovernedBatchCancellation,
}

/// Opaque pre-start capacity. Acquiring it starts one bounded blocking worker
/// and waits for that worker to reach its private receive point. The token has
/// no public launch method: only the eventual common-effect wrapper in this
/// module may hand it an already-started proof and physical action.
pub(crate) struct AppOsJailBlockingSlot {
    launch: Option<mpsc::SyncSender<AppOsJailBlockingLaunch>>,
    result:
        tokio::sync::oneshot::Receiver<Result<AppOsJailObservedResult, AppOsJailExecutionFailure>>,
    expires_at: Instant,
}

struct AppOsJailBlockingInFlight {
    result:
        tokio::sync::oneshot::Receiver<Result<AppOsJailObservedResult, AppOsJailExecutionFailure>>,
}

impl AppOsJailBlockingSlot {
    /// Final pre-start liveness fence. This exposes no launch authority; the
    /// workflow calls it immediately before durable dispatch-start, while the
    /// private launch path repeats the check after start and maps a race to
    /// uncertainty.
    pub(crate) fn is_live_for_start(&self) -> bool {
        self.launch.is_some() && Instant::now() < self.expires_at
    }

    fn launch(
        mut self,
        action: AppOsJailPreparedAction,
        authorization: AppEffectProviderIoAuthorization,
        cancellation: GovernedBatchCancellation,
    ) -> Result<AppOsJailBlockingInFlight, AppOsJailError> {
        if !self.is_live_for_start() {
            return Err(AppOsJailError::BlockingWorkerUnavailable);
        }
        self.launch
            .take()
            .ok_or(AppOsJailError::BlockingWorkerUnavailable)?
            .send(AppOsJailBlockingLaunch {
                action,
                authorization,
                cancellation,
            })
            .map_err(|_| AppOsJailError::BlockingWorkerUnavailable)?;
        Ok(AppOsJailBlockingInFlight {
            result: self.result,
        })
    }
}

impl AppOsJailBlockingInFlight {
    async fn settle(self) -> Result<AppOsJailObservedResult, AppOsJailExecutionFailure> {
        self.result
            .await
            .map_err(|_| AppOsJailExecutionFailure::worker_lost_after_handoff())?
    }
}

fn os_jail_blocking_capacity() -> &'static Arc<tokio::sync::Semaphore> {
    static CAPACITY: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    CAPACITY.get_or_init(|| {
        Arc::new(tokio::sync::Semaphore::new(
            MAX_APP_OS_JAIL_BLOCKING_WORKERS,
        ))
    })
}

async fn run_os_jail_bounded_blocking<T, F>(operation: F) -> Result<T, AppOsJailError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, AppOsJailError> + Send + 'static,
{
    let capacity = tokio::time::timeout(
        APP_OS_JAIL_BLOCKING_RESERVATION_TIMEOUT,
        Arc::clone(os_jail_blocking_capacity()).acquire_owned(),
    )
    .await
    .map_err(|_| AppOsJailError::BlockingWorkerUnavailable)?
    .map_err(|_| AppOsJailError::BlockingWorkerUnavailable)?;
    tokio::task::spawn_blocking(move || {
        let _capacity = capacity;
        operation()
    })
    .await
    .map_err(|_| AppOsJailError::BlockingWorkerUnavailable)?
}

/// Build the exact selected owner on bounded blocking capacity before workflow
/// task/resource guards are acquired. The governed executor later opens and
/// hashes the selected executable again from its own descriptor immediately
/// before snapshot/launch, so this removes async lock-held disk I/O without
/// weakening the final byte equality fence.
pub(crate) async fn prepare_locked_os_jail_action(
    source_bytes: Vec<u8>,
    primitive: AppLockedPrimitiveBinding,
    locked: AppLockedPrimitiveActionBinding,
    artifact_store: AppOsJailArtifactStore,
    source_directory: Option<PathBuf>,
    canonical_input: Vec<u8>,
) -> Result<AppOsJailPreparedAction, AppOsJailError> {
    run_os_jail_bounded_blocking(move || {
        let mut adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(&source_bytes)?;
        adapter.bind_locked_actions(&primitive)?;
        let artifact = adapter.revalidate_locked_artifact(
            &primitive,
            &locked,
            &artifact_store,
            source_directory.as_deref(),
        )?;
        adapter.lower_locked_action(&primitive, &locked, artifact, &canonical_input)
    })
    .await
}

/// Bounded selected-artifact review used by installation and recovery. It
/// returns no path or launch capability and cannot become a second executor.
pub async fn revalidate_locked_os_jail_artifact(
    source_bytes: Vec<u8>,
    primitive: AppLockedPrimitiveBinding,
    locked: AppLockedPrimitiveActionBinding,
    artifact_store: AppOsJailArtifactStore,
    source_directory: Option<PathBuf>,
) -> Result<(), AppOsJailError> {
    run_os_jail_bounded_blocking(move || {
        let mut adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(&source_bytes)?;
        adapter.bind_locked_actions(&primitive)?;
        adapter
            .revalidate_locked_artifact(
                &primitive,
                &locked,
                &artifact_store,
                source_directory.as_deref(),
            )
            .map(|_| ())
    })
    .await
}

/// Reserve and prove a running blocking worker before taking the final task /
/// resource-root guards. The later consuming wrapper requires this opaque slot
/// as an input, so it never allocates blocking capacity after dispatch-start.
pub(crate) async fn reserve_os_jail_blocking_slot() -> Result<AppOsJailBlockingSlot, AppOsJailError>
{
    let capacity = tokio::time::timeout(
        APP_OS_JAIL_BLOCKING_RESERVATION_TIMEOUT,
        Arc::clone(os_jail_blocking_capacity()).acquire_owned(),
    )
    .await
    .map_err(|_| AppOsJailError::BlockingWorkerUnavailable)?
    .map_err(|_| AppOsJailError::BlockingWorkerUnavailable)?;
    let (launch_tx, launch_rx) = mpsc::sync_channel::<AppOsJailBlockingLaunch>(1);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    drop(tokio::task::spawn_blocking(move || {
        let _capacity = capacity;
        let expires_at = Instant::now() + APP_OS_JAIL_BLOCKING_SLOT_LIFETIME;
        if ready_tx.send(expires_at).is_err() {
            return;
        }
        let result = launch_rx
            .recv_timeout(APP_OS_JAIL_BLOCKING_SLOT_LIFETIME)
            .map_err(|_| AppOsJailExecutionFailure::from(AppOsJailError::BlockingWorkerUnavailable))
            .and_then(|launch| {
                launch
                    .action
                    .execute_authorized(launch.authorization, &launch.cancellation)
                    .and_then(AppOsJailExecution::into_observed_result)
            });
        let _ = result_tx.send(result);
    }));
    let expires_at = tokio::time::timeout(APP_OS_JAIL_BLOCKING_RESERVATION_TIMEOUT, ready_rx)
        .await
        .map_err(|_| AppOsJailError::BlockingWorkerUnavailable)?
        .map_err(|_| AppOsJailError::BlockingWorkerUnavailable)?;
    Ok(AppOsJailBlockingSlot {
        launch: Some(launch_tx),
        result: result_rx,
        expires_at,
    })
}

/// A fully observed provider result that still owns the in-flight common
/// effect. Callers may inspect the exact typed result and canonical bytes to
/// persist completion intent, but cannot separate either from settlement
/// authority. They then consume this object into commit or uncertainty.
pub(crate) struct AppOsJailObservedEffect<R> {
    observed: AppOsJailObservedResult,
    effect: AppEffectInFlight<R>,
}

impl<R> AppOsJailObservedEffect<R> {
    pub(crate) fn action_result(&self) -> &ActionResult {
        &self.observed.action_result
    }

    pub(crate) fn canonical_result_bytes(&self) -> &[u8] {
        &self.observed.canonical_result_bytes
    }

    pub(crate) fn successful(&self) -> bool {
        self.observed.successful
    }

    pub(crate) fn audit_evidence(&self) -> Result<AppOsJailAuditEvidence, AppOsJailError> {
        AppOsJailAuditEvidence::from_runtime(
            self.observed.audit.terminal.dispatch(),
            Some(&self.observed.audit),
        )
    }

    /// Recheck the complete typed projection immediately before durable
    /// completion-intent publication. This keeps the terminal bit, governed
    /// audit and exact canonical bytes one invariant even if the structured
    /// carrier evolves later.
    pub(crate) fn validates_completion_projection(&self) -> bool {
        let ActionResult::Browser { data } = &self.observed.action_result else {
            return false;
        };
        let audit_digest = serde_json::to_value(&self.observed.audit)
            .ok()
            .and_then(|audit| AppDigest::blake3_canonical_json(&audit).ok());
        data.get("kind").and_then(Value::as_str) == Some("app_os_jail")
            && data.get("success").and_then(Value::as_bool) == Some(self.observed.successful)
            && data.get("audit_digest").and_then(Value::as_str)
                == audit_digest.as_ref().map(AppDigest::as_str)
            && serde_json::to_value(&self.observed.action_result)
                .ok()
                .and_then(|value| canonical_json_bytes(&value).ok())
                .as_deref()
                == Some(self.observed.canonical_result_bytes.as_slice())
    }

    pub(crate) fn commit(self) -> Result<AppOsJailCommittedResult<R>, AppOsJailUncertainResult<R>> {
        match self
            .effect
            .commit_result(&self.observed.canonical_result_bytes)
        {
            Ok(settlement) => Ok(AppOsJailCommittedResult {
                observed: self.observed,
                settlement,
            }),
            Err(settlement) => {
                let dispatch = self.observed.audit.terminal.dispatch();
                Err(AppOsJailUncertainResult {
                    failure: AppOsJailExecutionFailure::after_dispatch(
                        AppOsJailError::InvalidOutput,
                        dispatch,
                        Some(self.observed.audit),
                    ),
                    settlement,
                })
            },
        }
    }

    pub(crate) fn outcome_uncertain(
        self,
        stage: AppEffectStage,
        error: AppOsJailError,
    ) -> AppOsJailUncertainResult<R> {
        let dispatch = self.observed.audit.terminal.dispatch();
        AppOsJailUncertainResult {
            failure: AppOsJailExecutionFailure::after_dispatch(
                error,
                dispatch,
                Some(self.observed.audit),
            ),
            settlement: self.effect.outcome_uncertain(stage),
        }
    }
}

/// A committed result paired with the common effect settlement that still
/// owns resource/disclosure settlement. Construction is possible only by
/// consuming an observed effect after completion intent persistence.
pub(crate) struct AppOsJailCommittedResult<R> {
    observed: AppOsJailObservedResult,
    settlement: AppEffectSettlement<R>,
}

impl<R> AppOsJailCommittedResult<R> {
    pub(crate) fn into_parts(
        self,
    ) -> (
        ActionResult,
        Vec<u8>,
        bool,
        GovernedExecutionAuditReceipt,
        AppEffectSettlement<R>,
    ) {
        (
            self.observed.action_result,
            self.observed.canonical_result_bytes,
            self.observed.successful,
            self.observed.audit,
            self.settlement,
        )
    }
}

/// A post-start failure is inseparable from an uncertain common settlement.
/// Even a provider-reported NotDispatched value cannot release resources after
/// `begin_io` without a common opaque abort proof.
pub(crate) struct AppOsJailUncertainResult<R> {
    failure: AppOsJailExecutionFailure,
    settlement: AppEffectSettlement<R>,
}

impl<R> AppOsJailUncertainResult<R> {
    pub(crate) fn audit_evidence(&self) -> AppOsJailAuditEvidence {
        AppOsJailAuditEvidence::from_runtime(self.failure.dispatch, self.failure.audit.as_deref())
            .unwrap_or_else(|_| AppOsJailAuditEvidence::receipt_unavailable(self.failure.dispatch))
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        AppOsJailError,
        GovernedExecutionDispatch,
        Option<GovernedExecutionAuditReceipt>,
        AppEffectSettlement<R>,
    ) {
        (
            self.failure.error,
            self.failure.dispatch,
            self.failure.audit.map(|audit| *audit),
            self.settlement,
        )
    }
}

pub(crate) enum AppOsJailEffectOutcome<R> {
    Observed(AppOsJailObservedEffect<R>),
    Uncertain(AppOsJailUncertainResult<R>),
}

/// The non-bypassable post-start bridge. It takes the one common provider-I/O
/// authorization from the outer effect owner, moves it into the pre-reserved
/// worker with the exact prepared action, and returns only a common committed
/// or uncertain settlement. No caller can obtain a launch handle or settle a
/// post-start NotDispatched observation as proven-unspent.
pub(crate) async fn execute_started_os_jail<R>(
    slot: AppOsJailBlockingSlot,
    action: AppOsJailPreparedAction,
    mut effect: AppEffectInFlight<R>,
    cancellation: GovernedBatchCancellation,
    physical_timeout: StdDuration,
) -> AppOsJailEffectOutcome<R> {
    let Some(authorization) = effect.take_provider_io_authorization() else {
        return AppOsJailEffectOutcome::Uncertain(AppOsJailUncertainResult {
            failure: AppOsJailExecutionFailure::after_dispatch(
                AppOsJailError::IdentityMismatch,
                GovernedExecutionDispatch::NotDispatched,
                None,
            ),
            settlement: effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
        });
    };
    let deadline_cancellation = cancellation.clone();
    let execution = match slot.launch(action, authorization, cancellation) {
        Ok(execution) => execution,
        Err(error) => {
            return AppOsJailEffectOutcome::Uncertain(AppOsJailUncertainResult {
                failure: AppOsJailExecutionFailure::after_dispatch(
                    error,
                    GovernedExecutionDispatch::UnknownAfterDispatch,
                    None,
                ),
                settlement: effect.outcome_uncertain(AppEffectStage::ProviderIo),
            });
        },
    };
    let settlement = execution.settle();
    tokio::pin!(settlement);
    match tokio::time::timeout(physical_timeout, &mut settlement).await {
        Ok(Ok(observed)) => {
            AppOsJailEffectOutcome::Observed(AppOsJailObservedEffect { observed, effect })
        },
        Ok(Err(failure)) => AppOsJailEffectOutcome::Uncertain(AppOsJailUncertainResult {
            failure,
            settlement: effect.outcome_uncertain(AppEffectStage::ProviderIo),
        }),
        Err(_) => {
            // The blocking worker owns the process group. Sticky cancellation
            // makes it terminate/reap that group after this receiver is
            // dropped; the common effect is immediately settled uncertain
            // because dispatch already crossed its durable start edge.
            deadline_cancellation.cancel();
            AppOsJailEffectOutcome::Uncertain(AppOsJailUncertainResult {
                failure: AppOsJailExecutionFailure::after_dispatch(
                    AppOsJailError::ResourceDeadlineExceeded,
                    GovernedExecutionDispatch::UnknownAfterDispatch,
                    None,
                ),
                settlement: effect.outcome_uncertain(AppEffectStage::ProviderIo),
            })
        },
    }
}

impl AppOsJailExecution {
    fn into_observed_result(self) -> Result<AppOsJailObservedResult, AppOsJailExecutionFailure> {
        let dispatch = self.terminal.dispatch();
        let audit_value = serde_json::to_value(&self.audit).map_err(|_| {
            AppOsJailExecutionFailure::after_dispatch(
                AppOsJailError::InvalidOutput,
                dispatch,
                Some(self.audit.clone()),
            )
        })?;
        let audit_digest = AppDigest::blake3_canonical_json(&audit_value).map_err(|_| {
            AppOsJailExecutionFailure::after_dispatch(
                AppOsJailError::InvalidOutput,
                dispatch,
                Some(self.audit.clone()),
            )
        })?;
        let successful = self.terminal.terminal() == GovernedExecutionTerminal::Success
            && self.exit_code == Some(0);
        let terminal = serde_json::to_value(self.terminal).map_err(|_| {
            AppOsJailExecutionFailure::after_dispatch(
                AppOsJailError::InvalidOutput,
                dispatch,
                Some(self.audit.clone()),
            )
        })?;
        // `Browser` is the existing generic structured ActionResult carrier;
        // the explicit kind prevents this value from matching browser or
        // primitive-terminal projections. Child strings enter exactly one JSON
        // encoding pass, preserving the reviewed 6x expansion proof.
        let mut data = serde_json::json!({
            "kind": "app_os_jail",
            "schema": APP_OS_JAIL_PROFILE_V1,
            "success": successful,
            "terminal": terminal,
            "exit_code": self.exit_code,
            "audit_digest": audit_digest,
            "stdout": {
                "kind": output_kind_name(self.stdout.kind),
                "value": self.stdout.value,
                "digest": self.stdout.digest,
                "bytes": self.stdout.byte_count,
            },
            "stderr": {
                "kind": output_kind_name(self.stderr.kind),
                "value": self.stderr.value,
                "digest": self.stderr.digest,
                "bytes": self.stderr.byte_count,
            },
        });
        // The broker receipt is bounded (fixed counters, a destination name
        // and at most eight static refusal reasons) and fits the envelope.
        if let (Some(egress), Some(object)) = (&self.egress, data.as_object_mut()) {
            let receipt = serde_json::to_value(egress).map_err(|_| {
                AppOsJailExecutionFailure::after_dispatch(
                    AppOsJailError::InvalidOutput,
                    dispatch,
                    Some(self.audit.clone()),
                )
            })?;
            object.insert("egress".to_owned(), receipt);
        }
        let action_result = ActionResult::Browser { data };
        let bytes = canonical_json_bytes(&serde_json::to_value(&action_result).map_err(|_| {
            AppOsJailExecutionFailure::after_dispatch(
                AppOsJailError::InvalidOutput,
                dispatch,
                Some(self.audit.clone()),
            )
        })?)
        .map_err(|_| {
            AppOsJailExecutionFailure::after_dispatch(
                AppOsJailError::InvalidOutput,
                dispatch,
                Some(self.audit.clone()),
            )
        })?;
        if u64::try_from(bytes.len())
            .ok()
            .map_or(true, |bytes| bytes > self.transport_result_byte_ceiling)
        {
            return Err(AppOsJailExecutionFailure::after_dispatch(
                AppOsJailError::InvalidOutput,
                dispatch,
                Some(self.audit),
            ));
        }
        Ok(AppOsJailObservedResult {
            successful,
            action_result,
            canonical_result_bytes: bytes,
            audit: self.audit,
        })
    }
}

/// Exact finite serialized-result ceiling for the first OS-jail vertical.
/// Both stream limits must be authored explicitly: accepting the USR runtime's
/// broad fallback would silently turn the Apps hard maximum into app policy.
/// The returned value is the worst-case canonical JSON expansion of both
/// streams plus a fixed envelope. Oversized contracts are rejected rather than
/// clipped because a smaller declared ceiling does not make every
/// contract-valid provider result persistable.
///
/// The ceiling no longer depends on egress (every jailed skill runs on the
/// same budget); the parameter stays so callers keep one signature.
pub(crate) fn reviewed_transport_result_byte_ceiling(
    contract: &SkillRuntimeContract,
    _egress: Option<&AppOsJailEgressDeclaration>,
) -> Option<u64> {
    let budgeted = app_run_contract(contract);
    let contract = &budgeted;
    validate_skill_runtime_contract(contract).ok()?;
    let RuntimeProtocol::Cli { limits, .. } = &contract.runtime else {
        return None;
    };
    let stdout = limits.stdout_bytes?;
    let stderr = limits.stderr_bytes?;
    let encoded = stdout
        .checked_add(stderr)?
        .checked_mul(APP_OS_JAIL_RESULT_JSON_EXPANSION)?
        .checked_add(APP_OS_JAIL_RESULT_ENVELOPE_BYTES)?;
    let supported_ceiling = u64::try_from(MAX_APP_EFFECT_RESULT_BYTES)
        .ok()?
        .min(MAX_APP_OS_JAIL_DURABLE_RESULT_BYTES);
    (encoded <= supported_ceiling).then_some(encoded)
}

/// Canonical reviewed physical-plan identity shared by descriptor projection
/// and the runtime owner. Keeping this recipe in one place prevents a compiler
/// or jail-profile change from silently reusing an older package lock.
pub(crate) fn implementation_plan_digest(
    source_digest: &AppDigest,
    contract: &SkillRuntimeContract,
    action_name: &str,
    action: &CompiledTypedAction,
    egress: Option<&AppOsJailEgressDeclaration>,
) -> Option<AppDigest> {
    if !first_vertical_contract_supported(contract)
        || !in_place_auth_supported(&contract.auth)
        || action_requests_ambient_authority(action)
        || action_policy_requires_authority(&action.effective_policy)
    {
        return None;
    }
    let runtime_implementation_digest = os_jail_runtime_implementation_digest()?;
    let transport_result_byte_ceiling = reviewed_transport_result_byte_ceiling(contract, egress)?;
    // A contract only an in-place run can carry (a `provider` label, a
    // config-file key, keys without a declared host) locks its own recipe:
    // the exec-roots jail, the declared hosts (or none: the owner's grant
    // decides), the broker ceilings and the exact auth contract. Every
    // contract the single-file kinds already carried keeps its recipe below.
    if contract_requires_in_place(contract, egress) {
        return AppDigest::blake3_canonical_json(&serde_json::json!({
            "profile": APP_OS_JAIL_PROFILE_V1,
            "implementation_revision": APP_OS_JAIL_IMPLEMENTATION_REVISION,
            "runtime_implementation_digest": runtime_implementation_digest,
            "core_profile": GOVERNED_JAIL_EXEC_ROOTS_V1,
            "in_place": APP_OS_JAIL_IN_PLACE_PROFILE_V1,
            "egress": {
                "profile": APP_OS_JAIL_EGRESS_PROFILE_V1,
                "declared": egress.map(AppOsJailEgressDeclaration::destinations),
                "port": APP_OS_JAIL_EGRESS_PORT,
                "limits": APP_OS_JAIL_EGRESS_LIMITS,
                "stdout_budget": APP_OS_JAIL_STDOUT_BUDGET,
                "stderr_budget": APP_OS_JAIL_STDERR_BUDGET,
            },
            "auth": &contract.auth,
            "source_digest": source_digest,
            "transport_result_byte_ceiling": transport_result_byte_ceiling,
            "action": action_name,
            "definition_schema": &action.definition.input_schema,
            "invocation": &action.invocation,
            "effective_policy": &action.effective_policy,
        }))
        .ok();
    }
    // A skill with egress runs a different jail profile against a named
    // destination under fixed broker ceilings; all of it is lock identity.
    // Skills without egress keep their exact previous recipe.
    if let Some(egress) = egress {
        let mut recipe = serde_json::json!({
            "profile": APP_OS_JAIL_PROFILE_V1,
            "implementation_revision": APP_OS_JAIL_IMPLEMENTATION_REVISION,
            "runtime_implementation_digest": runtime_implementation_digest,
            "core_profile": GOVERNED_PROCESS_JAIL_BROKERED_EGRESS_V1,
            "egress": {
                "profile": APP_OS_JAIL_EGRESS_PROFILE_V1,
                "destination": egress.destination(),
                "port": APP_OS_JAIL_EGRESS_PORT,
                "limits": APP_OS_JAIL_EGRESS_LIMITS,
                "stdout_budget": APP_OS_JAIL_STDOUT_BUDGET,
                "stderr_budget": APP_OS_JAIL_STDERR_BUDGET,
            },
            "source_digest": source_digest,
            "transport_result_byte_ceiling": transport_result_byte_ceiling,
            "action": action_name,
            "definition_schema": &action.definition.input_schema,
            "invocation": &action.invocation,
            "effective_policy": &action.effective_policy,
        });
        // A secret-using skill also locks its exact auth contract: which
        // secrets, required or not, and where each is injected.
        if contract.auth != AuthContract::default() {
            recipe.as_object_mut()?.insert(
                "auth".to_owned(),
                serde_json::to_value(&contract.auth).ok()?,
            );
        }
        // A declared host list locks every host; the single `destination`
        // form keeps its exact reviewed recipe.
        if egress.destinations().len() > 1 {
            let egress_recipe = recipe.get_mut("egress")?.as_object_mut()?;
            egress_recipe.remove("destination");
            egress_recipe.insert(
                "destinations".to_owned(),
                serde_json::to_value(egress.destinations()).ok()?,
            );
        }
        return AppDigest::blake3_canonical_json(&recipe).ok();
    }
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "profile": APP_OS_JAIL_PROFILE_V1,
        "implementation_revision": APP_OS_JAIL_IMPLEMENTATION_REVISION,
        "runtime_implementation_digest": runtime_implementation_digest,
        "core_profile": GOVERNED_PROCESS_JAIL_V1,
        "source_digest": source_digest,
        "transport_result_byte_ceiling": transport_result_byte_ceiling,
        "action": action_name,
        "definition_schema": &action.definition.input_schema,
        "invocation": &action.invocation,
        "effective_policy": &action.effective_policy,
    }))
    .ok()
}

/// The contract an app run actually executes: [`app_jail_contract`], with
/// declared stream limits clamped to a budget fitting the app result ceiling.
fn app_run_contract(contract: &SkillRuntimeContract) -> SkillRuntimeContract {
    let mut budgeted = app_jail_contract(contract);
    if let RuntimeProtocol::Cli { limits, .. } = &mut budgeted.runtime {
        // Only declared limits are clamped: a skill that states no stream
        // limit still has no reviewable ceiling and stays refused. Skills
        // already inside the budget are unchanged.
        limits.stdout_bytes = limits
            .stdout_bytes
            .map(|declared| declared.min(APP_OS_JAIL_STDOUT_BUDGET));
        limits.stderr_bytes = limits
            .stderr_bytes
            .map(|declared| declared.min(APP_OS_JAIL_STDERR_BUDGET));
    }
    budgeted
}

/// Drop every secret binding (and its injections) the owner did not grant
/// this tool. Refuses when a required secret is not granted; an optional
/// skill left with no secrets runs unauthenticated.
fn narrow_auth_to_granted_secrets(
    auth: &mut AuthContract,
    authority: Option<&AppOsJailSecretAuthority>,
    needs_scope: bool,
) -> Result<(), AppOsJailError> {
    if auth.kind == AuthKind::None {
        return Ok(());
    }
    let required = super::secret_access::supported_in_place_secret_requirement(auth)
        .map_err(|_| AppOsJailError::UnsupportedRuntime)?;
    // An undeclared or any-host tool uses a key only with the owner's
    // chosen scope.
    let granted = |secret_ref: &str| {
        authority.is_some_and(|authority| {
            authority.granted.contains(secret_ref)
                && (!needs_scope || authority.key_scopes.contains_key(secret_ref))
        })
    };
    let kept = auth
        .secret_bindings
        .iter()
        .filter(|binding| granted(&binding.secret_ref))
        .map(|binding| binding.name.clone())
        .collect::<BTreeSet<_>>();
    if required && kept.len() != auth.secret_bindings.len() {
        return Err(AppOsJailError::SecretNotGranted);
    }
    if kept.is_empty() {
        *auth = AuthContract::default();
        return Ok(());
    }
    auth.secret_bindings
        .retain(|binding| kept.contains(&binding.name));
    auth.injections.retain(|injection| {
        matches!(&injection.source, InjectionSource::Secret { binding } if kept.contains(binding))
    });
    Ok(())
}

/// The static-secret preparation plan, built exactly as the governed
/// runtime's profile-free path builds it.
fn secret_preparation_plan(
    validated: tool_runtime_core::manifest_validation::ValidatedSkillRuntimeContract<'_>,
    scope: CredentialScope,
) -> Result<CredentialPreparationPlan, AppOsJailError> {
    let auth = &validated.contract().auth;
    let selection_request = CredentialProfileSelectionRequest::new(
        scope.clone(),
        auth.provider.as_deref(),
        CredentialProfileBinding::Provider,
        &auth.profile_selection,
        None,
    )
    .map_err(|_| AppOsJailError::IdentityMismatch)?;
    let selection = select_credential_profile(
        &crate::magician_v2::execution::primitive_dispatch::NoProfileRegistry,
        &selection_request,
    )
    .map_err(|_| AppOsJailError::IdentityMismatch)?;
    let required = super::secret_access::supported_in_place_secret_requirement(auth)
        .map_err(|_| AppOsJailError::UnsupportedRuntime)?;
    let bindings = auth
        .secret_bindings
        .iter()
        .map(|binding| {
            let name = CredentialMaterialBindingName::new(binding.name.clone())?;
            if required {
                CredentialPreparationBinding::new(
                    name,
                    CredentialMaterialKind::SecretBinding,
                    MAX_APP_OS_JAIL_SECRET_BYTES,
                )
            } else {
                CredentialPreparationBinding::optional(
                    name,
                    CredentialMaterialKind::SecretBinding,
                    MAX_APP_OS_JAIL_SECRET_BYTES,
                )
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AppOsJailError::IdentityMismatch)?;
    let minimum_present = if required { bindings.len() } else { 0 };
    CredentialPreparationPlan::new_with_minimum_present(
        scope,
        auth.kind,
        &selection,
        bindings,
        minimum_present,
    )
    .map_err(|_| AppOsJailError::IdentityMismatch)
}

/// The credential scratch area of one call inside its private credential
/// root: `<root>/<principal>/<workspace>/`, each 0700, opened through
/// MagicRun's scoped-path authority so the config file is written exactly as
/// the governed runtime writes it.
#[cfg(unix)]
fn private_credential_scratch(
    root: &Path,
    principal: &str,
    workspace: &str,
    scope: &CredentialScope,
) -> Result<tool_runtime_core::credential_filesystem::CredentialScratchAuthority, AppOsJailError> {
    use std::os::unix::fs::DirBuilderExt;

    let unavailable = || {
        AppOsJailError::InPlaceUnavailable(
            "the private credential directory could not be prepared".to_owned(),
        )
    };
    let plain = |component: &str| {
        !component.is_empty()
            && component != "."
            && component != ".."
            && !component.contains('/')
            && !component.contains('\\')
    };
    if !plain(principal) || !plain(workspace) {
        return Err(unavailable());
    }
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(root.join(principal).join(workspace))
        .map_err(|_| unavailable())?;
    let authority =
        tool_runtime_core::scoped_paths::ScopedPathAuthority::open(root).map_err(|_| unavailable())?;
    let scope_root = authority
        .resolve_scope_root(scope)
        .map_err(|_| unavailable())?;
    tool_runtime_core::credential_filesystem::CredentialScratchAuthority::open_or_create(&scope_root)
        .map_err(|_| unavailable())
}

#[cfg(not(unix))]
fn private_credential_scratch(
    _root: &Path,
    _principal: &str,
    _workspace: &str,
    _scope: &CredentialScope,
) -> Result<tool_runtime_core::credential_filesystem::CredentialScratchAuthority, AppOsJailError> {
    Err(AppOsJailError::UnsupportedRuntime)
}

fn first_vertical_contract_supported(contract: &SkillRuntimeContract) -> bool {
    matches!(
        &contract.runtime,
        RuntimeProtocol::Cli {
            interaction: CliInteraction::Batch,
            ..
        }
    ) && floor_without_jail_workdir(&contract.policy_floor) == PolicyFloor::default()
}

/// No auth, or the one supported secret shape (`app_secret_use_v1`): static
/// secrets injected only as environment variables into a skill that declares
/// its egress host, so a key can only reach that host.
fn app_auth_supported(auth: &AuthContract, egress: Option<&AppOsJailEgressDeclaration>) -> bool {
    *auth == AuthContract::default()
        || (egress.is_some() && super::secret_access::supported_secret_requirement(auth).is_ok())
}

/// The wider secret shape only an in-place skill may use: a `provider`
/// label, a `MMX_CONFIG_DIR` config file, or keys without a declared host
/// (they then reach only the hosts the owner grants the app).
fn in_place_auth_supported(auth: &AuthContract) -> bool {
    *auth == AuthContract::default()
        || super::secret_access::supported_in_place_secret_requirement(auth).is_ok()
}

/// Source-level view (no skill directory): whether a skill runs in place.
/// Its secret contract needs it, or it names companions it spawns beyond its
/// entry point and `python3`. The physical review also runs in place a skill
/// that ships Node packages or whose entry point is not a single native file
/// or exact `python3` script.
pub(crate) fn source_runs_in_place(
    contract: &SkillRuntimeContract,
    egress: Option<&AppOsJailEgressDeclaration>,
) -> bool {
    if !in_place_auth_supported(&contract.auth) {
        return false;
    }
    if contract_requires_in_place(contract, egress) {
        return true;
    }
    let entry = contract.requires.entrypoint.as_deref().or_else(|| {
        (contract.requires.bins.len() == 1)
            .then(|| contract.requires.bins.iter().next().map(String::as_str))
            .flatten()
    });
    entry.is_some_and(|entry| !declared_companions(&contract.requires.bins, entry).is_empty())
}

/// How a reviewed OS-jail skill runs, for the install review: the skill it
/// runs in place from (when any selected action runs in place), the hosts
/// it declares, and whether it reaches only the hosts the app is granted.
pub fn reviewed_os_jail_runtime(
    source: &str,
    source_directory: Option<&Path>,
) -> (Option<String>, Vec<String>, bool) {
    let Ok(adapter) = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source.as_bytes()) else {
        return (None, Vec::new(), false);
    };
    let declared = adapter
        .egress
        .as_ref()
        .map(|egress| egress.destinations().to_vec())
        .unwrap_or_default();
    let in_place = source_directory.is_some_and(|directory| {
        adapter
            .compiled
            .actions
            .values()
            .any(|action| adapter.runs_in_place(directory, &action.invocation.executable))
    });
    (
        in_place.then(|| adapter.skill_name.clone()),
        declared,
        in_place && adapter.egress.is_none(),
    )
}

/// A contract only an in-place run may carry. Such a skill is never copied
/// into a single-file artifact.
pub(crate) fn contract_requires_in_place(
    contract: &SkillRuntimeContract,
    egress: Option<&AppOsJailEgressDeclaration>,
) -> bool {
    !app_auth_supported(&contract.auth, egress) && in_place_auth_supported(&contract.auth)
}

/// The one resource scope the jail itself satisfies. Its working directory
/// is always a fresh, empty, private directory, so a skill that asks for a
/// workspace receives strictly less: never the owner's workspace or files.
/// Every other scope, grant, approval class or resource authority still
/// refuses the skill.
const JAIL_SATISFIED_RESOURCE_SCOPE: &str = "workspace";

/// The contract the jail compiles and locks. The model never chooses a
/// working directory in an app: a `workspace` working directory and scope are
/// dropped here, so no `working_dir` control reaches the model schema, and
/// lowering then runs the child in the jail's own private directory. Skills
/// already without them are returned unchanged, keeping their digests.
pub(crate) fn app_jail_contract(contract: &SkillRuntimeContract) -> SkillRuntimeContract {
    let mut jail = contract.clone();
    jail.policy_floor = floor_without_jail_workdir(&jail.policy_floor);
    if let RuntimeProtocol::Cli {
        working_directory, ..
    } = &mut jail.runtime
    {
        if working_directory.mode == WorkingDirectoryMode::Workspace {
            working_directory.mode = WorkingDirectoryMode::Denied;
        }
    }
    jail
}

fn floor_without_jail_workdir(floor: &PolicyFloor) -> PolicyFloor {
    let mut floor = floor.clone();
    floor.resource_scopes.remove(JAIL_SATISFIED_RESOURCE_SCOPE);
    floor
}

fn os_jail_runtime_implementation_digest() -> Option<&'static AppDigest> {
    static DIGEST: OnceLock<Option<AppDigest>> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"magician.app-os-jail-runtime-source.v1\0");
            hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
            for (name, source) in [
                ("apps/os-jail", include_bytes!("os_jail.rs").as_slice()),
                (
                    "apps/os-jail-in-place",
                    include_bytes!("os_jail_in_place.rs").as_slice(),
                ),
                (
                    "apps/os-jail-egress",
                    include_bytes!("os_jail_egress.rs").as_slice(),
                ),
                (
                    "core/process-jail",
                    tool_runtime_core::source_bytes::GOVERNED_PROCESS_JAIL,
                ),
                (
                    "core/batch-process",
                    tool_runtime_core::source_bytes::GOVERNED_BATCH_PROCESS,
                ),
                (
                    "core/execution-authority",
                    tool_runtime_core::source_bytes::GOVERNED_EXECUTION_AUTHORITY,
                ),
                (
                    "core/execution-coordinator",
                    tool_runtime_core::source_bytes::GOVERNED_EXECUTION_COORDINATOR,
                ),
                (
                    "core/execution-contract",
                    tool_runtime_core::source_bytes::GOVERNED_EXECUTION,
                ),
                (
                    "core/execution-result",
                    tool_runtime_core::source_bytes::GOVERNED_EXECUTION_RESULT,
                ),
                (
                    "core/action-overrides",
                    tool_runtime_core::source_bytes::ACTION_OVERRIDES,
                ),
                (
                    "core/credential-preparation",
                    tool_runtime_core::source_bytes::CREDENTIAL_PREPARATION,
                ),
                (
                    "core/credential-injection",
                    tool_runtime_core::source_bytes::CREDENTIAL_INJECTION,
                ),
                (
                    "core/credential-materialization",
                    tool_runtime_core::source_bytes::CREDENTIAL_MATERIALIZATION,
                ),
                (
                    "core/manifest-parser",
                    tool_runtime_core::source_bytes::MANIFEST_PARSER,
                ),
                (
                    "core/manifest-validation",
                    tool_runtime_core::source_bytes::MANIFEST_VALIDATION,
                ),
            ] {
                hasher.update(name.as_bytes());
                hasher.update(b"\0");
                hasher.update(source);
                hasher.update(b"\0");
            }
            AppDigest::parse(format!("blake3:{}", hasher.finalize().to_hex())).ok()
        })
        .as_ref()
}

/// Exact reviewed source/argv-schema owner. It is intentionally not Clone,
/// Debug, Serialize or Deserialize: compiled action mappings can carry private
/// authored implementation details and are not transport data.
pub(crate) struct AppOsJailPhysicalOwnerAdapter {
    skill_name: String,
    source_digest: AppDigest,
    package: SkillRuntimePackage,
    compiled: CompiledActionCatalog,
    actions: BTreeMap<String, AppOsJailActionPlan>,
    egress: Option<AppOsJailEgressDeclaration>,
    /// The contract can only run in place (see [`contract_requires_in_place`]).
    requires_in_place: bool,
}

impl AppOsJailPhysicalOwnerAdapter {
    pub(crate) fn from_reviewed_source(source_bytes: &[u8]) -> Result<Self, AppOsJailError> {
        if source_bytes.is_empty()
            || source_bytes.len() > tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES
        {
            return Err(AppOsJailError::InvalidSource);
        }
        let eligible =
            assess_app_tool_eligibility(source_bytes, AppToolAdmissionSource::ReviewedCatalog)
                .map_err(|_| AppOsJailError::InvalidSource)?;
        let source =
            std::str::from_utf8(source_bytes).map_err(|_| AppOsJailError::InvalidSource)?;
        let header: AppOsJailSkillHeader =
            parse_skill_frontmatter(source).map_err(|_| AppOsJailError::InvalidSource)?;
        if header.name != eligible.name.as_str() {
            return Err(AppOsJailError::InvalidSource);
        }
        let package = parse_skill_runtime_package(source)
            .map_err(|_| AppOsJailError::InvalidSource)?
            .ok_or(AppOsJailError::InvalidSource)?;
        let egress =
            parse_app_egress_declaration(source).map_err(|_| AppOsJailError::InvalidSource)?;
        // Compile and lock exactly what the catalog projects.
        let jail_contract = app_jail_contract(&package.contract);
        let RuntimeProtocol::Cli { interaction, .. } = &package.contract.runtime else {
            return Err(AppOsJailError::UnsupportedRuntime);
        };
        // The first physical vertical is intentionally unauthenticated. Secret,
        // profile, browser/native permission and delegated-credential contracts
        // remain blocked until the common app effect owner can carry their
        // existing broker permits into this exact jail without inventing a
        // second authority path.
        if *interaction != CliInteraction::Batch
            || !(app_auth_supported(&package.contract.auth, egress.as_ref())
                || in_place_auth_supported(&package.contract.auth))
        {
            return Err(AppOsJailError::UnsupportedRuntime);
        }
        let requires_in_place = contract_requires_in_place(&package.contract, egress.as_ref());
        let overrides = package
            .actions
            .as_ref()
            .ok_or(AppOsJailError::UnsupportedRuntime)?;
        let validated = validate_skill_runtime_contract(&jail_contract)
            .map_err(|_| AppOsJailError::InvalidSource)?;
        let compiled = compile_typed_action_overrides(eligible.name.as_str(), validated, overrides)
            .map_err(|_| AppOsJailError::InvalidSource)?;
        // Admission is per action: an action that needs authority (for
        // example one that creates files) is locked as non-dispatchable while
        // the skill's other actions stay usable. A skill with none left is
        // refused.
        if jail_contract.policy_floor != PolicyFloor::default()
            || compiled.actions.is_empty()
            || compiled.actions.values().all(|action| {
                action_requests_ambient_authority(action)
                    || action_policy_requires_authority(&action.effective_policy)
            })
        {
            return Err(AppOsJailError::AmbientAuthorityRequested);
        }

        let source_digest = AppDigest::blake3(source_bytes);
        let transport_result_byte_ceiling =
            reviewed_transport_result_byte_ceiling(&jail_contract, egress.as_ref())
                .ok_or(AppOsJailError::UnsupportedRuntime)?;
        let mut actions = BTreeMap::new();
        for (name, action) in &compiled.actions {
            let input_schema_digest =
                AppDigest::blake3_canonical_json(&app_facing_input_schema(action))
                    .map_err(|_| AppOsJailError::InvalidSource)?;
            let execution_plan_digest = implementation_plan_digest(
                &source_digest,
                &jail_contract,
                name,
                action,
                egress.as_ref(),
            );
            actions.insert(
                name.clone(),
                AppOsJailActionPlan {
                    action_ref: None,
                    input_schema_digest,
                    execution_plan_digest,
                    transport_result_byte_ceiling,
                },
            );
        }
        Ok(Self {
            skill_name: eligible.name.as_str().to_owned(),
            source_digest,
            // Lock identity above comes from the authored contract, exactly
            // as the catalog derives it; only the run uses the budget.
            package: SkillRuntimePackage {
                contract: app_run_contract(&package.contract),
                ..package
            },
            compiled,
            actions,
            egress,
            requires_in_place,
        })
    }

    /// Whether `bin/<executable>` of this skill runs in place rather than as
    /// a copied single-file artifact: its contract needs it, it names
    /// companions it spawns, it ships Node packages, or its entry point is
    /// neither a native file nor an exact `python3` script. `false` without
    /// its own `bin/<executable>` (a trusted system tool may then qualify).
    fn runs_in_place(&self, source_directory: &Path, executable: &str) -> bool {
        let Some(head) = read_entry_head(source_directory, executable) else {
            return false;
        };
        self.requires_in_place
            || !declared_companions(&self.package.contract.requires.bins, executable).is_empty()
            || skill_package_root(source_directory)
                .is_ok_and(|package| package.join("package.json").is_file())
            || artifact_kind(&head).is_none()
    }

    /// The in-place derivation of one action, or why it cannot run.
    fn derive_in_place(
        &self,
        source_directory: &Path,
        executable: &str,
    ) -> Result<AppInPlaceSkill, AppInPlaceSkillError> {
        derive_in_place_skill(
            source_directory,
            executable,
            &self.package.contract.requires.bins,
            &AppInPlaceHost::current(),
        )
    }


    /// Bind this exact reviewed owner to the lock identities once. A later
    /// invocation must present the same action reference; a same-name action
    /// from changed source bytes cannot reuse the owner.
    pub(crate) fn bind_locked_actions(
        &mut self,
        primitive: &AppLockedPrimitiveBinding,
    ) -> Result<(), AppOsJailError> {
        self.validate_locked_actions(primitive)?;
        for locked in primitive.actions() {
            let plan = self
                .actions
                .get_mut(locked.name())
                .ok_or(AppOsJailError::IdentityMismatch)?;
            plan.action_ref = Some(locked.action_ref().clone());
        }
        Ok(())
    }

    /// Attest the disclosure/effect target from the exact reviewed source and
    /// immutable lock before reopening the artifact for the final I/O fence.
    /// This is identity-only: it neither opens a process nor produces launch
    /// authority. The final prepared owner repeats the same recipe after the
    /// content-addressed artifact has been reopened and hashed.
    pub(crate) fn attest_locked_effect_target(
        &self,
        tool_ref: &AppReference,
        primitive: &AppLockedPrimitiveBinding,
        action: &AppLockedPrimitiveActionBinding,
    ) -> Result<AppEffectPhysicalTarget, AppEffectKernelError> {
        self.validate_locked_actions(primitive)
            .map_err(|_| AppEffectKernelError::IdentityMismatch)?;
        let plan = self
            .actions
            .get(action.name())
            .filter(|plan| plan.action_ref.as_ref() == Some(action.action_ref()))
            .ok_or(AppEffectKernelError::IdentityMismatch)?;
        let revision_ref = action
            .physical_artifact_revision_ref()
            .ok_or(AppEffectKernelError::IdentityMismatch)?;
        let artifact_digest = action
            .physical_artifact_digest()
            .ok_or(AppEffectKernelError::IdentityMismatch)?;
        attest_os_jail_effect_target(
            &self.skill_name,
            &self.source_digest,
            primitive.primitive_ref(),
            action.name(),
            action.action_ref(),
            &plan.input_schema_digest,
            plan.execution_plan_digest
                .as_ref()
                .ok_or(AppEffectKernelError::IdentityMismatch)?,
            plan.transport_result_byte_ceiling,
            revision_ref,
            artifact_digest,
            tool_ref,
            primitive,
            action,
            AppOsJailEgressAdmission::for_call(self.egress.as_ref(), None, false)
                .map(|admission| (admission, chrono::Utc::now())),
        )
    }

    /// Validate the exact canonical invocation against the reviewed selected
    /// action before disclosure/resource admission. Final lowering repeats
    /// this check while constructing the move-only prepared owner.
    pub(crate) fn validate_locked_action_input(
        &self,
        primitive: &AppLockedPrimitiveBinding,
        locked: &AppLockedPrimitiveActionBinding,
        canonical_input: &[u8],
    ) -> Result<(), AppOsJailError> {
        self.parse_locked_action_input(primitive, locked, canonical_input)
            .map(|_| ())
    }

    fn parse_locked_action_input(
        &self,
        primitive: &AppLockedPrimitiveBinding,
        locked: &AppLockedPrimitiveActionBinding,
        canonical_input: &[u8],
    ) -> Result<Value, AppOsJailError> {
        self.validate_locked_actions(primitive)?;
        if !primitive
            .actions()
            .iter()
            .any(|candidate| candidate == locked)
            || canonical_input.is_empty()
            || canonical_input.len() > MAX_APP_OS_JAIL_INPUT_BYTES
        {
            return Err(AppOsJailError::InvalidInput);
        }
        let input: Value =
            serde_json::from_slice(canonical_input).map_err(|_| AppOsJailError::InvalidInput)?;
        bounded_json(
            &input,
            MAX_APP_OS_JAIL_JSON_DEPTH,
            MAX_APP_OS_JAIL_JSON_NODES,
        )
        .map_err(|_| AppOsJailError::InvalidInput)?;
        if canonical_json_bytes(&input)
            .map_err(|_| AppOsJailError::InvalidInput)?
            .as_slice()
            != canonical_input
        {
            return Err(AppOsJailError::InvalidInput);
        }
        let action = self
            .compiled
            .actions
            .get(locked.name())
            .ok_or(AppOsJailError::IdentityMismatch)?;
        let (lowerable, _) = stage_app_inputs(action, &input, None)?;
        lower_typed_action_invocation(action, &lowerable)
            .map_err(|_| AppOsJailError::InvalidInput)?;
        Ok(input)
    }

    /// Validate an exact selected action subset without mutating the adapter.
    /// Install review uses this before touching any physical artifact, while a
    /// prepared runtime owner additionally binds the action references once.
    pub(crate) fn validate_locked_actions(
        &self,
        primitive: &AppLockedPrimitiveBinding,
    ) -> Result<(), AppOsJailError> {
        if primitive.source_content_digest() != &self.source_digest
            || primitive.actions().is_empty()
            || primitive.actions().len() > self.actions.len()
        {
            return Err(AppOsJailError::IdentityMismatch);
        }
        // Validate the complete set before mutating any plan. Rebinding an
        // already-bound owner to another action identity is refused, while an
        // idempotent bind to the same lock remains harmless.
        for locked in primitive.actions() {
            let plan = self
                .actions
                .get(locked.name())
                .ok_or(AppOsJailError::IdentityMismatch)?;
            if locked.input_schema_digest() != Some(&plan.input_schema_digest)
                || locked.implementation_plan_digest() != plan.execution_plan_digest.as_ref()
                || (locked.dispatchable()
                    && locked.transport_result_byte_ceiling()
                        != Some(plan.transport_result_byte_ceiling))
            {
                return Err(AppOsJailError::IdentityMismatch);
            }
            if plan
                .action_ref
                .as_ref()
                .is_some_and(|bound| bound != locked.action_ref())
            {
                return Err(AppOsJailError::IdentityMismatch);
            }
        }
        Ok(())
    }

    /// Review a skill-private `bin/<exact action executable>` (a native
    /// binary or an exact-shebang Python 3 script, following a link to the
    /// file it resolves to), or, for a skill with no executable of its own, a
    /// trusted tool on the sealed system volume. There is no PATH search: an
    /// action with neither remains non-dispatchable. Each admitted file is
    /// copied through an already-open descriptor into the broker-owned
    /// content-addressed store before its identity is returned.
    pub(crate) fn review_private_artifacts(
        &self,
        primitive: &AppLockedPrimitiveBinding,
        source_directory: &Path,
        artifact_store: &AppOsJailArtifactStore,
    ) -> Result<BTreeMap<String, AppOsJailPhysicalArtifactIdentity>, AppOsJailError> {
        self.validate_locked_actions(primitive)?;
        let mut reviewed = BTreeMap::new();
        for locked in primitive.actions() {
            let action = self
                .compiled
                .actions
                .get(locked.name())
                .ok_or(AppOsJailError::IdentityMismatch)?;
            let executable = &action.invocation.executable;
            if self.runs_in_place(source_directory, executable) {
                // Nothing is copied: the identity is the exec-roots profile,
                // the package fingerprint and the program's digest. A skill
                // that cannot run in place stays non-dispatchable, and the
                // install review says why.
                if let Ok(derived) = self.derive_in_place(source_directory, executable) {
                    if let Some(revision_ref) = derived.revision_ref() {
                        reviewed.insert(
                            locked.name().to_owned(),
                            AppOsJailPhysicalArtifactIdentity {
                                revision_ref,
                                digest: derived.program_digest().clone(),
                            },
                        );
                    }
                }
                continue;
            }
            if self.requires_in_place {
                continue;
            }
            if let Some(identity) =
                review_private_artifact(source_directory, artifact_store.root(), executable)?
            {
                reviewed.insert(locked.name().to_owned(), identity);
            }
        }
        Ok(reviewed)
    }

    /// Reopen and hash only the selected locked action. Runtime dispatch and
    /// installation re-review use this narrow form so an unrelated action set
    /// cannot multiply synchronous disk work for one invocation.
    pub(crate) fn revalidate_locked_artifact(
        &self,
        primitive: &AppLockedPrimitiveBinding,
        locked: &AppLockedPrimitiveActionBinding,
        artifact_store: &AppOsJailArtifactStore,
        source_directory: Option<&Path>,
    ) -> Result<AppOsJailVerifiedArtifact, AppOsJailError> {
        self.validate_locked_actions(primitive)?;
        if !primitive
            .actions()
            .iter()
            .any(|candidate| candidate == locked)
        {
            return Err(AppOsJailError::IdentityMismatch);
        }
        let action = self
            .compiled
            .actions
            .get(locked.name())
            .ok_or(AppOsJailError::IdentityMismatch)?;
        let executable = &action.invocation.executable;
        let Some(revision_ref) = locked.physical_artifact_revision_ref() else {
            // Nothing was locked: say why an in-place action could not be.
            if let Some(source_directory) = source_directory {
                if self.runs_in_place(source_directory, executable) {
                    if let Err(error) = self.derive_in_place(source_directory, executable) {
                        return Err(AppOsJailError::InPlaceUnavailable(error.to_string()));
                    }
                }
            }
            return Err(AppOsJailError::PhysicalArtifactUnavailable);
        };
        let digest = locked
            .physical_artifact_digest()
            .ok_or(AppOsJailError::PhysicalArtifactUnavailable)?;
        if revision_ref
            .as_str()
            .starts_with(APP_OS_JAIL_IN_PLACE_REF_PREFIX)
        {
            let source_directory = source_directory.ok_or(AppOsJailError::PhysicalArtifactUnavailable)?;
            let derived = verify_in_place_skill(
                source_directory,
                executable,
                &self.package.contract.requires.bins,
                revision_ref,
                digest,
                &AppInPlaceHost::current(),
            )
            .map_err(|error| match error {
                AppInPlaceSkillError::Changed => AppOsJailError::SkillChangedSinceApproval,
                AppInPlaceSkillError::Unavailable(reason) => {
                    AppOsJailError::InPlaceUnavailable(reason)
                },
            })?;
            return Ok(AppOsJailVerifiedArtifact {
                identity: AppOsJailPhysicalArtifactIdentity {
                    revision_ref: revision_ref.clone(),
                    digest: digest.clone(),
                },
                executable_directory: derived.program_directory().to_path_buf(),
                kind: AppOsJailArtifactKind::InPlaceSkill,
                in_place: Some(Box::new(derived)),
            });
        }
        if self.requires_in_place {
            return Err(AppOsJailError::PhysicalArtifactUnavailable);
        }
        reopen_physical_artifact(
            artifact_store.root(),
            &action.invocation.executable,
            revision_ref,
            digest,
        )
    }

    /// Schema-validate canonical input and seal the exact action executable,
    /// effective policy and lowered argv/stdin/timeout into one move-only plan.
    /// No caller executable, host path, environment or raw argv is accepted.
    pub(crate) fn lower_locked_action(
        &self,
        primitive: &AppLockedPrimitiveBinding,
        locked: &AppLockedPrimitiveActionBinding,
        artifact: AppOsJailVerifiedArtifact,
        canonical_input: &[u8],
    ) -> Result<AppOsJailPreparedAction, AppOsJailError> {
        let input = self.parse_locked_action_input(primitive, locked, canonical_input)?;
        let plan = self
            .actions
            .get(locked.name())
            .filter(|plan| plan.action_ref.as_ref() == Some(locked.action_ref()))
            .ok_or(AppOsJailError::IdentityMismatch)?;
        if locked.input_schema_digest() != Some(&plan.input_schema_digest)
            || plan.execution_plan_digest.is_none()
            || locked.implementation_plan_digest() != plan.execution_plan_digest.as_ref()
            || locked.transport_result_byte_ceiling() != Some(plan.transport_result_byte_ceiling)
            || locked.physical_artifact_revision_ref() != Some(artifact.identity().revision_ref())
            || locked.physical_artifact_digest() != Some(artifact.identity().digest())
        {
            return Err(AppOsJailError::IdentityMismatch);
        }
        self.lower_reviewed_action(
            locked.name(),
            primitive.primitive_ref().clone(),
            locked.action_ref().clone(),
            artifact,
            &input,
        )
    }

    /// The single lowering from a reviewed action plus admitted input to the
    /// move-only prepared owner. Callers have already bound the lock; this
    /// never consults it.
    fn lower_reviewed_action(
        &self,
        action_name: &str,
        primitive_ref: AppReference,
        action_ref: AppReference,
        artifact: AppOsJailVerifiedArtifact,
        input: &Value,
    ) -> Result<AppOsJailPreparedAction, AppOsJailError> {
        let plan = self
            .actions
            .get(action_name)
            .ok_or(AppOsJailError::IdentityMismatch)?;
        let action = self
            .compiled
            .actions
            .get(action_name)
            .ok_or(AppOsJailError::IdentityMismatch)?;
        // A contract only an in-place run can carry never runs as a copy.
        if self.requires_in_place && artifact.kind != AppOsJailArtifactKind::InPlaceSkill {
            return Err(AppOsJailError::IdentityMismatch);
        }
        // An exec-roots jail stages inputs in `in/`; the argv names them
        // there.
        let staged_directory = (artifact.kind == AppOsJailArtifactKind::InPlaceSkill)
            .then_some(GOVERNED_JAIL_EXEC_ROOTS_INPUT_DIRECTORY);
        let (lowerable, staged_inputs) = stage_app_inputs(action, input, staged_directory)?;
        let lowered = lower_typed_action_invocation(action, &lowerable)
            .map_err(|_| AppOsJailError::InvalidInput)?;
        if lowered.working_directory.is_some()
            || lowered.profile.is_some()
            || !lowered.runtime_controls.is_empty()
        {
            return Err(AppOsJailError::AmbientAuthorityRequested);
        }
        let mut effective_contract = self.package.contract.clone();
        effective_contract.requires.bins = BTreeSet::from([action.invocation.executable.clone()]);
        // In place, Python keeps (and looks for) bytecode under the private
        // workdir, never the skill's own `__pycache__`.
        if artifact.kind == AppOsJailArtifactKind::InPlaceSkill {
            effective_contract.requires.environment.insert(
                "PYTHONPYCACHEPREFIX".to_owned(),
                super::os_jail_in_place::IN_PLACE_PYCACHE_PREFIX.to_owned(),
            );
        }
        effective_contract.requires.entrypoint = Some(action.invocation.executable.clone());
        let RuntimeProtocol::Cli {
            working_directory,
            limits,
            stdin: stdin_contract,
            ..
        } = &mut effective_contract.runtime
        else {
            return Err(AppOsJailError::UnsupportedRuntime);
        };
        // The governed batch owner requires a cwd capability for a strict jail.
        // This is always the jail's fresh private workdir—not an app or host
        // workspace path—and lower_locked_action already rejects caller cwd.
        working_directory.mode = WorkingDirectoryMode::Workspace;
        let timeout_secs = lowered.timeout_secs.min(
            u32::try_from(tool_runtime_core::governed_process_jail::MAX_GOVERNED_JAIL_WALL_SECONDS)
                .unwrap_or(u32::MAX),
        );
        let timeout_secs = limits
            .timeout_secs
            .unwrap_or(timeout_secs)
            .min(timeout_secs);
        limits.timeout_secs = Some(timeout_secs);
        // A skill that takes no stdin must keep no stdin allowance: the
        // runtime contract rejects a byte limit on denied stdin.
        if stdin_contract.mode != tool_runtime_core::manifest::StdinMode::Denied {
            limits.stdin_bytes = Some(
                limits
                    .stdin_bytes
                    .unwrap_or(MAX_APP_OS_JAIL_INPUT_BYTES as u64)
                    .min(MAX_APP_OS_JAIL_INPUT_BYTES as u64),
            );
        }
        limits.stdout_bytes = Some(
            limits
                .stdout_bytes
                .unwrap_or(MAX_APP_OS_JAIL_OUTPUT_BYTES as u64)
                .min(MAX_APP_OS_JAIL_OUTPUT_BYTES as u64),
        );
        limits.stderr_bytes = Some(
            limits
                .stderr_bytes
                .unwrap_or(MAX_APP_OS_JAIL_OUTPUT_BYTES as u64)
                .min(MAX_APP_OS_JAIL_OUTPUT_BYTES as u64),
        );
        let mut jail_limits = GovernedProcessJailLimits::default();
        jail_limits.wall_seconds = u64::from(timeout_secs);
        jail_limits.cpu_seconds = u64::from(timeout_secs);
        // macOS has no per-jail task rlimit, so the ceiling is the watchdog's
        // thread count; give threaded tools the widest reviewed ceiling there.
        #[cfg(target_os = "macos")]
        {
            jail_limits.max_tasks =
                tool_runtime_core::governed_process_jail::MAX_GOVERNED_JAIL_TASKS;
        }
        limits.memory_bytes = Some(
            limits
                .memory_bytes
                .unwrap_or(jail_limits.max_memory_bytes)
                .min(jail_limits.max_memory_bytes),
        );
        apply_effective_policy(
            &mut effective_contract.policy_floor,
            &action.effective_policy,
        );
        validate_skill_runtime_contract(&effective_contract)
            .map_err(|_| AppOsJailError::IdentityMismatch)?;
        let request = GovernedExecutionRequest::new(
            lowered.arguments,
            lowered.stdin.map(String::into_bytes),
            lowered.working_directory,
            Some(timeout_secs),
        );
        let physical_artifact_revision_ref = artifact.identity().revision_ref().clone();
        let physical_artifact_digest = artifact.identity().digest().clone();
        let artifact_kind = artifact.kind;
        let in_place = artifact.in_place;
        let executable_directory = artifact.executable_directory;
        Ok(AppOsJailPreparedAction {
            skill_name: self.skill_name.clone(),
            source_digest: self.source_digest.clone(),
            primitive_ref,
            action_name: action_name.to_owned(),
            action_ref,
            input_schema_digest: plan.input_schema_digest.clone(),
            execution_plan_digest: plan
                .execution_plan_digest
                .clone()
                .ok_or(AppOsJailError::AmbientAuthorityRequested)?,
            transport_result_byte_ceiling: plan.transport_result_byte_ceiling,
            physical_artifact_revision_ref,
            physical_artifact_digest,
            executable_directory,
            jail_limits,
            effective_contract,
            request,
            spend_gate: None,
            egress: self.egress.clone(),
            prepared_at: chrono::Utc::now(),
            artifact_kind,
            secret_authority: None,
            staged_inputs,
            in_place,
            network_grant: None,
        })
    }
}

#[cfg(unix)]
struct AppOsJailTemporaryArtifact {
    file: File,
    path: PathBuf,
}

#[cfg(unix)]
impl AppOsJailTemporaryArtifact {
    fn path(&self) -> &Path {
        &self.path
    }

    fn as_file(&self) -> &File {
        &self.file
    }
}

#[cfg(unix)]
impl Write for AppOsJailTemporaryArtifact {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(unix)]
impl Drop for AppOsJailTemporaryArtifact {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn create_temporary_artifact(store: &Path) -> Result<AppOsJailTemporaryArtifact, AppOsJailError> {
    for _ in 0..16 {
        let path = store.join(format!(".reviewed-app-artifact-{}", uuid::Uuid::new_v4()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => return Ok(AppOsJailTemporaryArtifact { file, path }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(AppOsJailError::PhysicalArtifactUnavailable),
        }
    }
    Err(AppOsJailError::PhysicalArtifactUnavailable)
}

#[cfg(unix)]
fn review_private_artifact(
    source_directory: &Path,
    artifact_store_root: &Path,
    executable_name: &str,
) -> Result<Option<AppOsJailPhysicalArtifactIdentity>, AppOsJailError> {
    if !is_plain_executable_name(executable_name) {
        return Ok(None);
    }
    let canonical_source = fs::canonicalize(source_directory)
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if canonical_source != source_directory {
        return Ok(None);
    }
    let source_metadata = fs::symlink_metadata(&canonical_source)
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Ok(None);
    }
    let spelled = canonical_source.join("bin").join(executable_name);
    let spelled_metadata = match fs::symlink_metadata(&spelled) {
        Ok(metadata) => metadata,
        // No executable of its own: it may name a trusted system tool.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return review_system_tool(executable_name)
        },
        Err(_) => return Err(AppOsJailError::PhysicalArtifactUnavailable),
    };
    // A skill may link its executable to a build output (for example
    // `bin/tool -> target/release/tool`). Review the file it resolves to: its
    // bytes are copied into the private store and pinned by digest below, so
    // a later change to the link or its target never reaches a lock.
    let candidate = if spelled_metadata.file_type().is_symlink() {
        fs::canonicalize(&spelled).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?
    } else {
        spelled
    };
    let path_metadata = match fs::symlink_metadata(&candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(AppOsJailError::PhysicalArtifactUnavailable),
    };
    if path_metadata.file_type().is_symlink()
        || !path_metadata.is_file()
        || path_metadata.len() == 0
        || path_metadata.len() > MAX_APP_OS_JAIL_PRIVATE_ARTIFACT_BYTES
        || path_metadata.mode() & 0o111 == 0
        || fs::canonicalize(&candidate).ok().as_deref() != Some(candidate.as_path())
    {
        return Ok(None);
    }
    let mut source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&candidate)
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let opened = source
        .metadata()
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if opened.dev() != path_metadata.dev()
        || opened.ino() != path_metadata.ino()
        || opened.len() != path_metadata.len()
        || opened.mode() != path_metadata.mode()
        || opened.ctime() != path_metadata.ctime()
        || opened.ctime_nsec() != path_metadata.ctime_nsec()
    {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    let head =
        read_artifact_head(&mut source).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if artifact_kind(&head).is_none() {
        return Ok(None);
    }
    source
        .rewind()
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;

    let store = ensure_artifact_store_root(artifact_store_root)?;
    let mut temporary = create_temporary_artifact(&store)?;
    let mut hasher = blake3::Hasher::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(read as u64)
            .filter(|bytes| *bytes <= MAX_APP_OS_JAIL_PRIVATE_ARTIFACT_BYTES)
            .ok_or(AppOsJailError::PhysicalArtifactUnavailable)?;
        hasher.update(&buffer[..read]);
        temporary
            .write_all(&buffer[..read])
            .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    }
    if copied != opened.len() {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    let after = source
        .metadata()
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let current = fs::symlink_metadata(&candidate)
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if after.dev() != opened.dev()
        || after.ino() != opened.ino()
        || after.len() != opened.len()
        || after.mode() != opened.mode()
        || after.ctime() != opened.ctime()
        || after.ctime_nsec() != opened.ctime_nsec()
        || current.dev() != opened.dev()
        || current.ino() != opened.ino()
        || current.len() != opened.len()
        || current.mode() != opened.mode()
        || current.ctime() != opened.ctime()
        || current.ctime_nsec() != opened.ctime_nsec()
    {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    temporary
        .flush()
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o500))
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;

    let digest = AppDigest::parse(format!("blake3:{}", hasher.finalize().to_hex()))
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let _ = digest_hex(&digest)?;
    let artifact_key = physical_artifact_key(executable_name, &digest);
    let revision_ref = AppReference::parse(format!("artifact:app-os-jail:v1:{artifact_key}"))
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let directory = store.join(&artifact_key);
    match fs::create_dir(&directory) {
        Ok(()) => fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(_) => return Err(AppOsJailError::PhysicalArtifactUnavailable),
    }
    validate_private_directory(&directory)?;
    let destination = directory.join(executable_name);
    match fs::hard_link(temporary.path(), &destination) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(_) => return Err(AppOsJailError::PhysicalArtifactUnavailable),
    }
    verify_artifact_file(&destination, &digest)?;
    validate_single_artifact_directory(&directory, executable_name)?;
    sync_directory(&directory)?;
    sync_directory(&store)?;
    Ok(Some(AppOsJailPhysicalArtifactIdentity {
        revision_ref,
        digest,
    }))
}

#[cfg(not(unix))]
fn review_private_artifact(
    _source_directory: &Path,
    _artifact_store_root: &Path,
    _executable_name: &str,
) -> Result<Option<AppOsJailPhysicalArtifactIdentity>, AppOsJailError> {
    Ok(None)
}

#[cfg(unix)]
fn reopen_physical_artifact(
    artifact_store_root: &Path,
    executable_name: &str,
    expected_revision_ref: &AppReference,
    expected_digest: &AppDigest,
) -> Result<AppOsJailVerifiedArtifact, AppOsJailError> {
    if !is_plain_executable_name(executable_name) {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    let _ = digest_hex(expected_digest)?;
    if expected_revision_ref
        .as_str()
        .starts_with(APP_OS_JAIL_SYSTEM_TOOL_REF_PREFIX)
    {
        return reopen_system_tool(executable_name, expected_revision_ref, expected_digest);
    }
    let artifact_key = physical_artifact_key(executable_name, expected_digest);
    let expected_ref = format!("artifact:app-os-jail:v1:{artifact_key}");
    if expected_revision_ref.as_str() != expected_ref {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    let store = ensure_artifact_store_root(artifact_store_root)?;
    let directory = store.join(artifact_key);
    validate_private_directory(&directory)?;
    validate_single_artifact_directory(&directory, executable_name)?;
    let kind = verify_artifact_file(&directory.join(executable_name), expected_digest)?;
    Ok(AppOsJailVerifiedArtifact {
        identity: AppOsJailPhysicalArtifactIdentity {
            revision_ref: expected_revision_ref.clone(),
            digest: expected_digest.clone(),
        },
        executable_directory: directory,
        kind,
        in_place: None,
    })
}

#[cfg(not(unix))]
fn reopen_physical_artifact(
    _artifact_store_root: &Path,
    _executable_name: &str,
    _expected_revision_ref: &AppReference,
    _expected_digest: &AppDigest,
) -> Result<AppOsJailVerifiedArtifact, AppOsJailError> {
    Err(AppOsJailError::PhysicalArtifactUnavailable)
}

#[cfg(unix)]
fn open_artifact_store(apps_root: &Path) -> Result<PathBuf, AppOsJailError> {
    if !apps_root.is_absolute() {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    fs::create_dir_all(apps_root).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let canonical_apps =
        fs::canonicalize(apps_root).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let apps_metadata = fs::symlink_metadata(&canonical_apps)
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if apps_metadata.file_type().is_symlink()
        || !apps_metadata.is_dir()
        || apps_metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    let physical = create_private_store_component(&canonical_apps, "physical-artifacts")?;
    let version = create_private_store_component(&physical, "v1")?;
    sync_directory(&physical)?;
    sync_directory(&canonical_apps)?;
    Ok(version)
}

#[cfg(not(unix))]
fn open_artifact_store(_apps_root: &Path) -> Result<PathBuf, AppOsJailError> {
    Err(AppOsJailError::PhysicalArtifactUnavailable)
}

#[cfg(unix)]
fn create_private_store_component(parent: &Path, name: &str) -> Result<PathBuf, AppOsJailError> {
    let child = parent.join(name);
    match fs::create_dir(&child) {
        Ok(()) => fs::set_permissions(&child, fs::Permissions::from_mode(0o700))
            .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(_) => return Err(AppOsJailError::PhysicalArtifactUnavailable),
    }
    let canonical =
        fs::canonicalize(&child).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if canonical != child {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    validate_private_directory(&canonical)?;
    sync_directory(&canonical)?;
    Ok(canonical)
}

#[cfg(unix)]
fn ensure_artifact_store_root(root: &Path) -> Result<PathBuf, AppOsJailError> {
    if !root.is_absolute() {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    let canonical =
        fs::canonicalize(root).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if canonical != root {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    let metadata = fs::symlink_metadata(&canonical)
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    validate_private_directory(&canonical)?;
    Ok(canonical)
}

#[cfg(unix)]
fn validate_private_directory(path: &Path) -> Result<(), AppOsJailError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || fs::canonicalize(path).ok().as_deref() != Some(path)
    {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), AppOsJailError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)
}

#[cfg(unix)]
fn verify_artifact_file(
    path: &Path,
    expected: &AppDigest,
) -> Result<AppOsJailArtifactKind, AppOsJailError> {
    let path_metadata =
        fs::symlink_metadata(path).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.is_file()
        || path_metadata.len() == 0
        || path_metadata.len() > MAX_APP_OS_JAIL_PRIVATE_ARTIFACT_BYTES
        || path_metadata.uid() != unsafe { libc::geteuid() }
        || path_metadata.mode() & 0o111 == 0
        || path_metadata.mode() & 0o022 != 0
        || fs::canonicalize(path).ok().as_deref() != Some(path)
    {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let opened = file
        .metadata()
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if opened.dev() != path_metadata.dev()
        || opened.ino() != path_metadata.ino()
        || opened.len() != path_metadata.len()
        || opened.mode() != path_metadata.mode()
        || opened.ctime() != path_metadata.ctime()
        || opened.ctime_nsec() != path_metadata.ctime_nsec()
    {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    let head =
        read_artifact_head(&mut file).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let kind = artifact_kind(&head).ok_or(AppOsJailError::PhysicalArtifactUnavailable)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&head);
    let mut read_bytes = head.len() as u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
        if read == 0 {
            break;
        }
        read_bytes = read_bytes
            .checked_add(read as u64)
            .filter(|bytes| *bytes <= path_metadata.len())
            .ok_or(AppOsJailError::PhysicalArtifactUnavailable)?;
        hasher.update(&buffer[..read]);
    }
    let actual = AppDigest::parse(format!("blake3:{}", hasher.finalize().to_hex()))
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let after = file
        .metadata()
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let current =
        fs::symlink_metadata(path).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if read_bytes != path_metadata.len()
        || after.dev() != opened.dev()
        || after.ino() != opened.ino()
        || after.len() != opened.len()
        || after.mode() != opened.mode()
        || after.ctime() != opened.ctime()
        || after.ctime_nsec() != opened.ctime_nsec()
        || current.dev() != opened.dev()
        || current.ino() != opened.ino()
        || current.len() != opened.len()
        || current.mode() != opened.mode()
        || current.ctime() != opened.ctime()
        || current.ctime_nsec() != opened.ctime_nsec()
        || &actual != expected
    {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    Ok(kind)
}

/// Pin a trusted system tool for a skill that ships no executable of its
/// own. The tool must be a root-owned, not group/other-writable native
/// executable, spelled canonically, on the sealed read-only system volume.
#[cfg(unix)]
fn review_system_tool(
    executable_name: &str,
) -> Result<Option<AppOsJailPhysicalArtifactIdentity>, AppOsJailError> {
    for directory in APP_OS_JAIL_SYSTEM_TOOL_DIRECTORIES {
        let path = Path::new(directory).join(executable_name);
        let Some(digest) = trusted_system_tool_digest(&path)? else {
            continue;
        };
        let revision_ref = system_tool_revision_ref(directory, executable_name, &digest)?;
        return Ok(Some(AppOsJailPhysicalArtifactIdentity {
            revision_ref,
            digest,
        }));
    }
    Ok(None)
}

#[cfg(unix)]
fn reopen_system_tool(
    executable_name: &str,
    expected_revision_ref: &AppReference,
    expected_digest: &AppDigest,
) -> Result<AppOsJailVerifiedArtifact, AppOsJailError> {
    for directory in APP_OS_JAIL_SYSTEM_TOOL_DIRECTORIES {
        let path = Path::new(directory).join(executable_name);
        if system_tool_revision_ref(directory, executable_name, expected_digest)?
            != *expected_revision_ref
        {
            continue;
        }
        // Same checks as review, and the bytes must still be the pinned ones:
        // an OS update that replaced the tool requires re-review.
        if trusted_system_tool_digest(&path)?.as_ref() != Some(expected_digest) {
            return Err(AppOsJailError::PhysicalArtifactUnavailable);
        }
        return Ok(AppOsJailVerifiedArtifact {
            identity: AppOsJailPhysicalArtifactIdentity {
                revision_ref: expected_revision_ref.clone(),
                digest: expected_digest.clone(),
            },
            executable_directory: PathBuf::from(directory),
            kind: AppOsJailArtifactKind::SystemTool,
            in_place: None,
        });
    }
    Err(AppOsJailError::PhysicalArtifactUnavailable)
}

#[cfg(unix)]
fn system_tool_revision_ref(
    directory: &str,
    executable_name: &str,
    digest: &AppDigest,
) -> Result<AppReference, AppOsJailError> {
    let hex = digest_hex(digest)?;
    let place = directory.trim_start_matches('/').replace('/', "-");
    AppReference::parse(format!(
        "{APP_OS_JAIL_SYSTEM_TOOL_REF_PREFIX}{place}:{executable_name}:{hex}"
    ))
    .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)
}

/// `Some(digest)` for a trusted system tool, `None` when the path is absent
/// or does not qualify.
#[cfg(unix)]
fn trusted_system_tool_digest(path: &Path) -> Result<Option<AppDigest>, AppOsJailError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(AppOsJailError::PhysicalArtifactUnavailable),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_APP_OS_JAIL_PRIVATE_ARTIFACT_BYTES
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
        || fs::canonicalize(path).ok().as_deref() != Some(path)
        || !in_trusted_system_location(path)
    {
        return Ok(None);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let head =
        read_artifact_head(&mut file).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    if artifact_kind(&head) != Some(AppOsJailArtifactKind::Native) {
        return Ok(None);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(&head);
    std::io::copy(&mut file, &mut hasher)
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    AppDigest::parse(format!("blake3:{}", hasher.finalize().to_hex()))
        .map(Some)
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)
}

/// macOS: the sealed system volume, mounted read-only and the root
/// filesystem (`MNT_RDONLY | MNT_ROOTFS`), the same test MagicRun uses to run
/// such binaries in place. A mount property, never a path prefix.
#[cfg(target_os = "macos")]
fn in_trusted_system_location(path: &Path) -> bool {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    let flags = (libc::MNT_RDONLY as u32) | (libc::MNT_ROOTFS as u32);
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut mount = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is NUL-terminated and outlives the call; `mount` is a
    // correctly sized, writable `statfs` owned by this thread.
    if unsafe { libc::statfs(path.as_ptr(), mount.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: `statfs` returned success, so it initialized the structure.
    unsafe { mount.assume_init() }.f_flags & flags == flags
}

/// Linux has no sealed volume: every directory above the tool must be
/// root-owned and not group/other-writable, so only root could replace it.
/// (MagicRun snapshots the bytes before exec, and the lock pins the digest.)
#[cfg(all(unix, not(target_os = "macos")))]
fn in_trusted_system_location(path: &Path) -> bool {
    path.ancestors().skip(1).all(|directory| {
        fs::symlink_metadata(directory).is_ok_and(|metadata| {
            metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0
        })
    })
}

fn is_plain_executable_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.contains('/')
        && !value.contains('\\')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn is_native_executable_magic(magic: [u8; 4]) -> bool {
    magic == [0x7f, b'E', b'L', b'F']
        || matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
        )
}

fn digest_hex(digest: &AppDigest) -> Result<&str, AppOsJailError> {
    digest
        .as_str()
        .strip_prefix("blake3:")
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
        .ok_or(AppOsJailError::PhysicalArtifactUnavailable)
}

fn physical_artifact_key(executable_name: &str, digest: &AppDigest) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.app-os-jail-physical-artifact.v1\0");
    hasher.update(executable_name.as_bytes());
    hasher.update(b"\0");
    hasher.update(digest.as_str().as_bytes());
    hasher.finalize().to_hex().to_string()
}

#[cfg(unix)]
fn validate_single_artifact_directory(
    directory: &Path,
    executable_name: &str,
) -> Result<(), AppOsJailError> {
    let mut entries =
        fs::read_dir(directory).map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?;
    let entry = entries
        .next()
        .transpose()
        .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?
        .ok_or(AppOsJailError::PhysicalArtifactUnavailable)?;
    if entry.file_name() != std::ffi::OsStr::new(executable_name)
        || entries
            .next()
            .transpose()
            .map_err(|_| AppOsJailError::PhysicalArtifactUnavailable)?
            .is_some()
    {
        return Err(AppOsJailError::PhysicalArtifactUnavailable);
    }
    Ok(())
}

fn app_digest_blake3_bytes(digest: &AppDigest) -> Result<[u8; 32], AppOsJailError> {
    let hex = digest_hex(digest)?.as_bytes();
    let mut bytes = [0_u8; 32];
    for (index, output) in bytes.iter_mut().enumerate() {
        let high = decode_lower_hex(hex[index * 2])?;
        let low = decode_lower_hex(hex[index * 2 + 1])?;
        *output = (high << 4) | low;
    }
    Ok(bytes)
}

fn decode_lower_hex(value: u8) -> Result<u8, AppOsJailError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(AppOsJailError::PhysicalArtifactUnavailable),
    }
}

impl AppEffectPhysicalOwner for AppOsJailPreparedAction {
    fn attest_effect_target(
        &self,
        tool_ref: &AppReference,
        primitive: &AppLockedPrimitiveBinding,
        action: &AppLockedPrimitiveActionBinding,
    ) -> Result<AppEffectPhysicalTarget, AppEffectKernelError> {
        attest_os_jail_effect_target(
            &self.skill_name,
            &self.source_digest,
            &self.primitive_ref,
            &self.action_name,
            &self.action_ref,
            &self.input_schema_digest,
            &self.execution_plan_digest,
            self.transport_result_byte_ceiling,
            &self.physical_artifact_revision_ref,
            &self.physical_artifact_digest,
            tool_ref,
            primitive,
            action,
            self.egress_admission()
                .map_err(|_| AppEffectKernelError::IdentityMismatch)?
                .map(|admission| (admission, self.prepared_at)),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn attest_os_jail_effect_target(
    skill_name: &str,
    source_digest: &AppDigest,
    primitive_ref: &AppReference,
    action_name: &str,
    action_ref: &AppReference,
    input_schema_digest: &AppDigest,
    execution_plan_digest: &AppDigest,
    transport_result_byte_ceiling: u64,
    physical_artifact_revision_ref: &AppReference,
    physical_artifact_digest: &AppDigest,
    tool_ref: &AppReference,
    primitive: &AppLockedPrimitiveBinding,
    action: &AppLockedPrimitiveActionBinding,
    egress: Option<(AppOsJailEgressAdmission, chrono::DateTime<chrono::Utc>)>,
) -> Result<AppEffectPhysicalTarget, AppEffectKernelError> {
    let expected_tool_ref = format!("capability:{skill_name}");
    if tool_ref.as_str() != expected_tool_ref
        || primitive.primitive_ref() != primitive_ref
        || primitive.source_content_digest() != source_digest
        || !primitive
            .actions()
            .iter()
            .any(|candidate| candidate == action)
        || !action.dispatchable()
        || action.name() != action_name
        || action.action_ref() != action_ref
        || action.input_schema_digest() != Some(input_schema_digest)
        || action.implementation_plan_digest() != Some(execution_plan_digest)
        || action.transport_result_byte_ceiling() != Some(transport_result_byte_ceiling)
        || action.physical_artifact_revision_ref() != Some(physical_artifact_revision_ref)
        || action.physical_artifact_digest() != Some(physical_artifact_digest)
    {
        return Err(AppEffectKernelError::IdentityMismatch);
    }
    let target_ref = runtime_target_ref(
        execution_plan_digest,
        transport_result_byte_ceiling,
        physical_artifact_revision_ref,
        physical_artifact_digest,
    )
    .map_err(|_| AppEffectKernelError::IdentityMismatch)?;
    let target = match egress {
        // Egress makes this an external tool call to every admitted host,
        // so disclosure applies the run's granted destinations (or the
        // owner's "any public host" grant) and data handling policy before
        // anything launches.
        Some((admission, issued_at)) => {
            os_jail_egress_target(tool_ref, &target_ref, &admission, issued_at)
                .ok_or(AppEffectKernelError::IdentityMismatch)?
        },
        None => AttestedAppToolTarget::from_trusted_local_dispatcher(
            tool_ref.clone(),
            target_ref.clone(),
        ),
    };
    Ok(AppEffectPhysicalTarget::from_owner(
        target_ref,
        source_digest.clone(),
        target,
    ))
}

fn os_jail_egress_target(
    tool_ref: &AppReference,
    target_ref: &AppReference,
    admission: &AppOsJailEgressAdmission,
    issued_at: chrono::DateTime<chrono::Utc>,
) -> Option<AttestedAppToolTarget> {
    // One admitted host keeps the exact reviewed configuration; a host set
    // or "any public host" binds the whole admission instead.
    let mut configuration = serde_json::json!({
        "profile": APP_OS_JAIL_EGRESS_PROFILE_V1,
        "jail_profile": GOVERNED_PROCESS_JAIL_BROKERED_EGRESS_V1,
        "runtime_target_ref": target_ref,
        "port": APP_OS_JAIL_EGRESS_PORT,
        "limits": APP_OS_JAIL_EGRESS_LIMITS,
        "resolution": "broker_resolves_public_addresses_only",
    });
    let object = configuration.as_object_mut()?;
    match admission.single_host() {
        Some(host) => object.insert("destination".to_owned(), Value::String(host.to_owned())),
        None => object.insert("admission".to_owned(), serde_json::to_value(admission).ok()?),
    };
    let configuration_digest = AppDigest::blake3_canonical_json(&configuration).ok()?;
    let endpoint = AttestedAppEndpoint::from_trusted_resolver(
        AppReference::parse("endpoint:app-os-jail-egress-v1").ok()?,
        AppEndpointClass::External,
        false,
        AppRevision::new(1).ok()?,
        configuration_digest,
        issued_at,
        issued_at.checked_add_signed(chrono::Duration::seconds(
            APP_OS_JAIL_EGRESS_ATTESTATION_SECONDS,
        ))?,
    )
    .ok()?;
    let mut destinations = admission
        .destination_refs()
        .into_iter()
        .map(AppReference::parse)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if destinations.is_empty() {
        return None;
    }
    let destination = destinations.remove(0);
    Some(AttestedAppToolTarget::from_trusted_external_dispatcher_set(
        tool_ref.clone(),
        endpoint,
        destination,
        destinations,
    ))
}

/// Parameters an app fills with file *content* instead of a path: every
/// typed read-only workspace path. The jail stages the content as a private
/// input file and hands the tool that file's name, so no host path is ever
/// named by the model or the app.
fn staged_input_parameters(action: &CompiledTypedAction) -> BTreeSet<String> {
    action
        .parameters
        .iter()
        .filter(|(_, parameter)| {
            matches!(
                parameter,
                TypedActionParameter::WorkspacePath {
                    access: WorkspacePathAccess::ReadFile,
                    ..
                }
            )
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// The input schema an app (and its model) sees: identical to the compiled
/// schema except that each read-only file input takes the file's content.
/// Both the catalog projection and the lock use this exact schema.
pub(crate) fn app_facing_input_schema(action: &CompiledTypedAction) -> Value {
    let mut schema = action.definition.input_schema.clone();
    let staged = staged_input_parameters(action);
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for name in staged {
            let authored = properties
                .get(&name)
                .and_then(|property| property.get("description"))
                .and_then(Value::as_str)
                .unwrap_or("Input file.")
                .to_owned();
            properties.insert(
                name,
                serde_json::json!({
                    "description": format!(
                        "{authored} Give the file's content, not a path: a string for text, \
                         or {{\"name\": \"report.pdf\", \"base64\": \"...\"}} for a named \
                         or binary file."
                    ),
                    "oneOf": [
                        {"type": "string"},
                        {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string", "maxLength": 100},
                                "text": {"type": "string"},
                                "base64": {"type": "string"}
                            },
                            "additionalProperties": false
                        }
                    ]
                }),
            );
        }
    }
    schema
}

/// Replace each read-only file input's content with the name it will be
/// staged under, and return those files. Content is a string (text) or an
/// object with exactly one of `text` / `base64` and an optional `name` whose
/// extension is kept (tools detect formats by it).
fn stage_app_inputs(
    action: &CompiledTypedAction,
    input: &Value,
    staged_directory: Option<&str>,
) -> Result<(Value, Vec<(String, Vec<u8>)>), AppOsJailError> {
    use base64::Engine as _;

    let staged = staged_input_parameters(action);
    if staged.is_empty() {
        return Ok((input.clone(), Vec::new()));
    }
    let mut object = input
        .as_object()
        .cloned()
        .ok_or(AppOsJailError::InvalidInput)?;
    let mut files = Vec::with_capacity(staged.len());
    for parameter in staged {
        let Some(value) = object.get(&parameter) else {
            continue;
        };
        let (hint, bytes) = match value {
            Value::String(text) => (None, text.as_bytes().to_vec()),
            Value::Object(fields) => {
                if fields
                    .keys()
                    .any(|key| !matches!(key.as_str(), "name" | "text" | "base64"))
                {
                    return Err(AppOsJailError::InvalidInput);
                }
                let hint = match fields.get("name") {
                    None => None,
                    Some(Value::String(name)) if name.len() <= 100 => Some(name.clone()),
                    Some(_) => return Err(AppOsJailError::InvalidInput),
                };
                let bytes = match (fields.get("text"), fields.get("base64")) {
                    (Some(Value::String(text)), None) => text.as_bytes().to_vec(),
                    (None, Some(Value::String(encoded))) => {
                        base64::engine::general_purpose::STANDARD
                            .decode(encoded)
                            .map_err(|_| AppOsJailError::InvalidInput)?
                    },
                    _ => return Err(AppOsJailError::InvalidInput),
                };
                (hint, bytes)
            },
            _ => return Err(AppOsJailError::InvalidInput),
        };
        let name = staged_input_file_name(&parameter, hint.as_deref());
        let argument = match staged_directory {
            Some(directory) => format!("{directory}/{name}"),
            None => name.clone(),
        };
        object.insert(parameter, Value::String(argument));
        files.push((name, bytes));
    }
    Ok((Value::Object(object), files))
}

/// `in-<parameter>[.<ext>]`: the `in-` prefix keeps a staged name from ever
/// reading as a flag, and only a short alphanumeric extension from the hint
/// survives.
fn staged_input_file_name(parameter: &str, hint: Option<&str>) -> String {
    let parameter = parameter
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let extension = hint
        .and_then(|hint| hint.rsplit_once('.'))
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 10
                && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        });
    match extension {
        Some(extension) => format!("in-{parameter}.{extension}"),
        None => format!("in-{parameter}"),
    }
}

fn action_requests_ambient_authority(action: &CompiledTypedAction) -> bool {
    action.invocation.route != TypedActionRoute::Execute
        // A read-only workspace path becomes staged file content in an app;
        // anything that would create files stays refused.
        || action.parameters.values().any(|parameter| {
            matches!(
                parameter,
                TypedActionParameter::WorkspacePath {
                    access: WorkspacePathAccess::CreateFile | WorkspacePathAccess::CreateDirectory,
                    ..
                }
            )
        })
        || action
            .invocation
            .mappings
            .iter()
            .any(|mapping| matches!(mapping, TypedArgumentMapping::RuntimeControl { .. }))
        || action
            .definition
            .input_schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| {
                properties.contains_key("profile") || properties.contains_key("working_dir")
            })
}

fn action_policy_requires_authority(policy: &EffectiveActionPolicy) -> bool {
    // `ordinary` is the lowest approval class and carries no authority.
    policy
        .required_approvals
        .iter()
        .any(|approval| *approval != tool_runtime_core::manifest::ApprovalClass::Ordinary)
        || !policy.required_grants.is_empty()
        || policy
            .resource_scopes
            .iter()
            .any(|scope| scope != JAIL_SATISFIED_RESOURCE_SCOPE)
        || !policy.required_resource_authorities.is_empty()
}

fn apply_effective_policy(base: &mut PolicyFloor, effective: &EffectiveActionPolicy) {
    if let Some(approval) = effective.required_approvals.iter().max().copied() {
        base.approval = base.approval.max(approval);
    }
    base.required_grants
        .extend(effective.required_grants.iter().cloned());
    base.resource_scopes.extend(
        effective
            .resource_scopes
            .iter()
            .filter(|scope| scope.as_str() != JAIL_SATISFIED_RESOURCE_SCOPE)
            .cloned(),
    );
    base.required_resource_authorities
        .extend(effective.required_resource_authorities.iter().cloned());
}

fn runtime_target_ref(
    execution_plan_digest: &AppDigest,
    transport_result_byte_ceiling: u64,
    physical_artifact_revision_ref: &AppReference,
    physical_artifact_digest: &AppDigest,
) -> Result<AppReference, AppOsJailError> {
    let runtime_implementation_digest =
        os_jail_runtime_implementation_digest().ok_or(AppOsJailError::IdentityMismatch)?;
    let target_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "profile": APP_OS_JAIL_PROFILE_V1,
        "implementation_revision": APP_OS_JAIL_IMPLEMENTATION_REVISION,
        "runtime_implementation_digest": runtime_implementation_digest,
        "execution_plan_digest": execution_plan_digest,
        "transport_result_byte_ceiling": transport_result_byte_ceiling,
        "physical_artifact_revision_ref": physical_artifact_revision_ref,
        "physical_artifact_digest": physical_artifact_digest,
    }))
    .map_err(|_| AppOsJailError::IdentityMismatch)?;
    let digest = target_digest
        .as_str()
        .strip_prefix("blake3:")
        .ok_or(AppOsJailError::IdentityMismatch)?;
    AppReference::parse(format!("runtime:app-os-jail:v1:{digest}"))
        .map_err(|_| AppOsJailError::IdentityMismatch)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppOsJailOutputKind {
    Json,
    Text,
}

/// Typed bytes after governed redaction and before common disclosure labeling.
/// The effect kernel's returned disclosure permit remains the sole label owner.
struct AppOsJailTypedOutput {
    kind: AppOsJailOutputKind,
    value: Value,
    digest: AppDigest,
    byte_count: u64,
}

#[allow(dead_code)] // Typed output inspectors are retained for owner-side qualification.
impl AppOsJailTypedOutput {
    /// Standard error: bounded JSON when it is JSON, otherwise text, even
    /// when it happens to start with `[` or `{`.
    fn parse_diagnostics(bytes: &[u8]) -> Result<Self, AppOsJailError> {
        match Self::parse(bytes) {
            Err(AppOsJailError::InvalidOutput)
                if bytes.len() <= MAX_APP_OS_JAIL_OUTPUT_BYTES && std::str::from_utf8(bytes).is_ok() =>
            {
                let text = std::str::from_utf8(bytes).map_err(|_| AppOsJailError::InvalidOutput)?;
                Ok(Self {
                    kind: AppOsJailOutputKind::Text,
                    value: Value::String(text.to_owned()),
                    digest: AppDigest::blake3(bytes),
                    byte_count: u64::try_from(bytes.len())
                        .map_err(|_| AppOsJailError::InvalidOutput)?,
                })
            },
            parsed => parsed,
        }
    }

    fn parse(bytes: &[u8]) -> Result<Self, AppOsJailError> {
        if bytes.len() > MAX_APP_OS_JAIL_OUTPUT_BYTES {
            return Err(AppOsJailError::InvalidOutput);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| AppOsJailError::InvalidOutput)?;
        let (kind, value) = match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                bounded_json(
                    &value,
                    MAX_APP_OS_JAIL_JSON_DEPTH,
                    MAX_APP_OS_JAIL_JSON_NODES,
                )
                .map_err(|_| AppOsJailError::InvalidOutput)?;
                (AppOsJailOutputKind::Json, value)
            },
            Err(_)
                if matches!(
                    text.trim_start().as_bytes().first(),
                    Some(b'{') | Some(b'[')
                ) =>
            {
                // JSON-looking output must not bypass depth/node validation by
                // becoming an opaque text value when serde's recursion guard
                // or syntax parser rejects it.
                return Err(AppOsJailError::InvalidOutput);
            },
            Err(_) => (AppOsJailOutputKind::Text, Value::String(text.to_owned())),
        };
        Ok(Self {
            kind,
            value,
            digest: AppDigest::blake3(bytes),
            byte_count: u64::try_from(bytes.len()).map_err(|_| AppOsJailError::InvalidOutput)?,
        })
    }

    fn kind(&self) -> AppOsJailOutputKind {
        self.kind
    }

    fn value(&self) -> &Value {
        &self.value
    }

    fn digest(&self) -> &AppDigest {
        &self.digest
    }

    fn byte_count(&self) -> u64 {
        self.byte_count
    }
}

fn output_kind_name(kind: AppOsJailOutputKind) -> &'static str {
    match kind {
        AppOsJailOutputKind::Json => "json",
        AppOsJailOutputKind::Text => "text",
    }
}

fn bounded_json(value: &Value, max_depth: usize, max_nodes: usize) -> Result<(), ()> {
    let mut pending = vec![(value, 1_usize)];
    let mut nodes = 0_usize;
    while let Some((value, depth)) = pending.pop() {
        nodes = nodes.checked_add(1).ok_or(())?;
        if nodes > max_nodes || depth > max_depth {
            return Err(());
        }
        match value {
            Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            },
            Value::Object(values) => {
                pending.extend(values.values().map(|value| (value, depth + 1)));
            },
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {},
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};

    use static_assertions::assert_not_impl_any;

    use super::*;

    fn bounded_cli_contract(
        stdout_bytes: Option<u64>,
        stderr_bytes: Option<u64>,
    ) -> SkillRuntimeContract {
        SkillRuntimeContract {
            schema_version: tool_runtime_core::manifest::SkillRuntimeContractVersion::v1(),
            requires: tool_runtime_core::manifest::RuntimeRequirements {
                bins: BTreeSet::from(["reviewed-tool".to_owned()]),
                entrypoint: None,
                environment: BTreeMap::new(),
            },
            runtime: RuntimeProtocol::Cli {
                command_prefix: Vec::new(),
                interaction: CliInteraction::Batch,
                stdin: tool_runtime_core::manifest::StdinContract::default(),
                working_directory: tool_runtime_core::manifest::WorkingDirectoryContract::default(),
                limits: tool_runtime_core::manifest::RuntimeLimits {
                    timeout_secs: Some(5),
                    stdin_bytes: None,
                    stdout_bytes,
                    stderr_bytes,
                    memory_bytes: None,
                },
            },
            auth: AuthContract::default(),
            policy_floor: PolicyFloor::default(),
        }
    }

    /// Live, end to end: a native tool declared against `export.arxiv.org`
    /// is lowered by the runtime path, runs in the brokered-egress jail with
    /// its own broker, and opens a tunnel to the real host. The tool is a
    /// small C program built here (a copied Apple binary is killed by macOS
    /// outside its system path). Run with `--ignored`; needs network, `cc`
    /// and macOS `sandbox-exec`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "reaches export.arxiv.org through the brokered-egress jail"]
    fn live_native_skill_reaches_its_declared_host_through_the_jail() {
        const PROBE: &str = r#"
#include <arpa/inet.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
int main(void) {
    const char *proxy = getenv("HTTPS_PROXY");
    const char *colon = proxy ? strrchr(proxy, ':') : NULL;
    if (!colon) { fprintf(stderr, "no proxy\n"); return 2; }
    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_port = htons((unsigned short)atoi(colon + 1));
    inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0 || connect(fd, (struct sockaddr *)&addr, sizeof addr) != 0) { perror("connect"); return 3; }
    const char *request = "CONNECT export.arxiv.org:443 HTTP/1.1\r\nHost: export.arxiv.org:443\r\n\r\n";
    write(fd, request, strlen(request));
    char line[128] = {0};
    for (size_t i = 0; i + 1 < sizeof line && read(fd, line + i, 1) == 1 && line[i] != '\r'; i++) {}
    line[strcspn(line, "\r")] = 0;
    printf("%s", line);
    return 0;
}
"#;
        let source = "---\nname: tunnel-probe\nversion: 1.0.0\ndescription: Open one tunnel to arXiv through the broker.\nmetadata:\n  magician:\n    skill_type: tool\n    app_egress:\n      schema_version: 1\n      destination: export.arxiv.org\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires: {bins: [tunnel-probe]}\n      runtime:\n        protocol: cli\n        command_prefix: []\n        limits:\n          timeout_secs: 30\n          stdout_bytes: 4096\n          stderr_bytes: 1024\n    runtime_actions:\n      schema_version: tool-runtime.typed-action-overrides.v1\n      actions:\n        probe:\n          description: Open one tunnel and print the broker's status line.\n          fixed_args: []\n---\nProbe.\n";
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source.as_bytes())
            .expect("an authority-free native skill with egress is admitted");
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("probe.c");
        fs::write(&program, PROBE).unwrap();
        let bin = directory.path().join("tunnel-probe");
        let built = std::process::Command::new("/usr/bin/cc")
            .arg("-o")
            .arg(&bin)
            .arg(&program)
            .status()
            .unwrap();
        assert!(built.success());
        fs::remove_file(&program).unwrap();
        let digest = AppDigest::blake3(&fs::read(&bin).unwrap());
        let artifact = AppOsJailVerifiedArtifact {
            identity: AppOsJailPhysicalArtifactIdentity {
                revision_ref: AppReference::parse("artifact:reviewed-tool:v1").unwrap(),
                digest,
            },
            executable_directory: fs::canonicalize(directory.path()).unwrap(),
            kind: AppOsJailArtifactKind::Native,
            in_place: None,
        };
        let prepared = adapter
            .lower_reviewed_action(
                "probe",
                AppReference::parse("primitive:tunnel-probe").unwrap(),
                AppReference::parse("action:tunnel-probe:probe").unwrap(),
                artifact,
                &serde_json::json!({}),
            )
            .expect("lowered");
        let context = GovernedExecutionCallContext::new(
            CredentialCallId::new("app-live-egress").unwrap(),
            prepared.skill_name.clone(),
            prepared.action_name.clone(),
        )
        .unwrap();
        let execution = match prepared.execute_governed(context, &GovernedBatchCancellation::new())
        {
            Ok(execution) => execution,
            Err(failure) => panic!("jailed call failed: {}", failure.error),
        };
        assert_eq!(
            execution.exit_code,
            Some(0),
            "terminal={:?} stdout={:?} stderr={:?} egress={:?}",
            execution.terminal,
            execution.stdout.value,
            execution.stderr.value,
            execution.egress
        );
        assert_eq!(
            execution.stdout.value,
            serde_json::json!("HTTP/1.1 200 Connection Established")
        );
        let receipt = execution.egress.expect("egress receipt");
        assert_eq!(receipt.destination, "export.arxiv.org");
        assert_eq!(receipt.connections_tunnelled, 1);
        assert!(receipt.refusals.is_empty());
    }

    fn compiled_action(source: &str, name: &str) -> CompiledTypedAction {
        let package = parse_skill_runtime_package(source).unwrap().unwrap();
        let contract = app_jail_contract(&package.contract);
        let validated = validate_skill_runtime_contract(&contract).unwrap();
        compile_typed_action_overrides("fixture", validated, package.actions.as_ref().unwrap())
            .unwrap()
            .actions
            .remove(name)
            .unwrap()
    }

    #[test]
    fn a_read_only_file_input_takes_content_and_is_staged_under_a_safe_name() {
        let source = include_str!("../../../../skillshub/jq/SKILL.md");
        let action = compiled_action(source, "run");
        let schema = app_facing_input_schema(&action);
        assert!(schema["properties"]["input_file"]["oneOf"].is_array());
        assert_eq!(
            schema["properties"]["expression"],
            action.definition.input_schema["properties"]["expression"]
        );

        let (lowerable, files) = stage_app_inputs(
            &action,
            &serde_json::json!({"expression": ".a", "input_file": "{\"a\":1}"}),
            None,
        )
        .unwrap();
        assert_eq!(lowerable["input_file"], "in-input_file");
        assert_eq!(files, [("in-input_file".to_owned(), b"{\"a\":1}".to_vec())]);

        let (lowerable, files) = stage_app_inputs(
            &action,
            &serde_json::json!({"expression": ".", "input_file": {"name": "Data.JSON", "base64": "e30="}}),
            None,
        )
        .unwrap();
        assert_eq!(lowerable["input_file"], "in-input_file.json");
        assert_eq!(files[0].1, b"{}");

        for bad in [
            serde_json::json!({"expression": ".", "input_file": {"text": "a", "base64": "YQ=="}}),
            serde_json::json!({"expression": ".", "input_file": {"path": "/etc/passwd"}}),
            serde_json::json!({"expression": ".", "input_file": {"base64": "***"}}),
            serde_json::json!({"expression": ".", "input_file": 7}),
        ] {
            assert!(stage_app_inputs(&action, &bad, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn staged_names_never_read_as_flags_and_keep_only_a_short_extension() {
        assert_eq!(staged_input_file_name("input_file", None), "in-input_file");
        assert_eq!(
            staged_input_file_name("input_file", Some("r.PDF")),
            "in-input_file.pdf"
        );
        assert_eq!(
            staged_input_file_name("input_file", Some("../../x.sh;rm")),
            "in-input_file"
        );
        assert_eq!(
            staged_input_file_name("input_file", Some("noext")),
            "in-input_file"
        );
        assert_eq!(
            staged_input_file_name("in put", Some("a.verylongextension")),
            "in-in_put"
        );
    }

    #[test]
    fn a_skill_with_one_file_creating_action_keeps_its_read_only_action() {
        let source = include_bytes!("../../../../skillshub/document-to-markdown/SKILL.md");
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source)
            .expect("convert is admitted even though convert_to_workspace is not");
        assert!(adapter.actions["convert"].execution_plan_digest.is_some());
        assert!(adapter.actions["convert_to_workspace"]
            .execution_plan_digest
            .is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_sealed_system_tool_is_pinned_and_reopened_by_digest() {
        let identity = review_system_tool("jq")
            .unwrap()
            .expect("/usr/bin/jq qualifies");
        assert!(identity
            .revision_ref
            .as_str()
            .starts_with(APP_OS_JAIL_SYSTEM_TOOL_REF_PREFIX));
        let reopened = reopen_system_tool("jq", &identity.revision_ref, &identity.digest).unwrap();
        assert_eq!(reopened.kind, AppOsJailArtifactKind::SystemTool);
        assert_eq!(reopened.executable_directory, Path::new("/usr/bin"));
        let wrong = AppDigest::blake3(b"a different jq");
        let wrong_ref = system_tool_revision_ref("/usr/bin", "jq", &wrong).unwrap();
        assert!(reopen_system_tool("jq", &wrong_ref, &wrong).is_err());
        // Not on the sealed volume, or not present: never a system tool.
        assert!(review_system_tool("definitely-not-a-tool")
            .unwrap()
            .is_none());
    }

    #[cfg(target_os = "macos")]
    fn run_live(
        source: &[u8],
        action: &str,
        artifact: AppOsJailVerifiedArtifact,
        input: serde_json::Value,
    ) -> AppOsJailExecution {
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source).unwrap();
        let prepared = adapter
            .lower_reviewed_action(
                action,
                AppReference::parse("primitive:fixture").unwrap(),
                AppReference::parse("action:fixture:run").unwrap(),
                artifact,
                &input,
            )
            .expect("lowered");
        let context = GovernedExecutionCallContext::new(
            CredentialCallId::new("app-live-staged").unwrap(),
            prepared.skill_name.clone(),
            prepared.action_name.clone(),
        )
        .unwrap();
        match prepared.execute_governed(context, &GovernedBatchCancellation::new()) {
            Ok(execution) => execution,
            Err(failure) => panic!("jailed call failed: {}", failure.error),
        }
    }

    /// Live: Apple's `/usr/bin/jq` runs in place as a trusted system tool on
    /// JSON the app passed as content.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "runs /usr/bin/jq in the jail"]
    fn live_system_jq_reads_staged_content() {
        let identity = review_system_tool("jq").unwrap().unwrap();
        let artifact = reopen_system_tool("jq", &identity.revision_ref, &identity.digest).unwrap();
        let execution = run_live(
            include_bytes!("../../../../skillshub/jq/SKILL.md"),
            "run",
            artifact,
            serde_json::json!({"expression": ".items | length", "input_file": "{\"items\":[1,2,3]}"}),
        );
        assert_eq!(
            execution.exit_code,
            Some(0),
            "stderr={:?}",
            execution.stderr.value
        );
        assert_eq!(execution.stdout.value, serde_json::json!(3));
    }

    /// Live: the reviewed document-to-markdown binary converts a CSV the app
    /// passed as named content.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "runs document-to-markdown in the jail"]
    fn live_document_to_markdown_converts_staged_content() {
        let skill = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../skillshub/document-to-markdown"
        ));
        let fixture = tempfile::tempdir().unwrap();
        let store = AppOsJailArtifactStore::open_or_create(&fixture.path().join("apps")).unwrap();
        let store_root = store.root().to_path_buf();
        let identity = review_private_artifact(
            &fs::canonicalize(skill).unwrap(),
            &store_root,
            "document-to-markdown",
        )
        .unwrap()
        .expect("the linked build output is reviewed as a private artifact");
        let artifact = reopen_physical_artifact(
            &store_root,
            "document-to-markdown",
            &identity.revision_ref,
            &identity.digest,
        )
        .unwrap();
        let execution = run_live(
            include_bytes!("../../../../skillshub/document-to-markdown/SKILL.md"),
            "convert",
            artifact,
            serde_json::json!({"input_file": {"name": "table.csv", "text": "name,qty\napples,3\n"}}),
        );
        assert_eq!(
            execution.exit_code,
            Some(0),
            "stderr={:?}",
            execution.stderr.value
        );
        let rendered = execution.stdout.value.to_string();
        assert!(rendered.contains("apples"), "{rendered}");
    }

    #[test]
    fn a_required_secret_the_owner_did_not_grant_refuses_the_call() {
        let source = include_str!("../../../../skillshub/news-search-via-tavily/SKILL.md");
        let package = parse_skill_runtime_package(source).unwrap().unwrap();
        let mut auth = package.contract.auth.clone();
        assert_eq!(
            narrow_auth_to_granted_secrets(&mut auth, None, false)
                .unwrap_err()
                .to_string(),
            AppOsJailError::SecretNotGranted.to_string()
        );
        // Granted: the contract is kept exactly.
        let resolver = Arc::new(
            crate::magician_v2::secrets::SecretStoreResolver::new_with_capabilities(
                Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
                tempfile::tempdir().unwrap().keep(),
                crate::magician_v2::secrets::SecretRuntimeCapabilities::fully_available("test"),
            ),
        );
        let authority = AppOsJailSecretAuthority {
            store_resolver: resolver,
            principal: "owner".into(),
            workspace: "default".into(),
            granted: BTreeSet::from(["TAVILY_API_KEY".to_owned()]),
            key_scopes: BTreeMap::new(),
        };
        let mut granted = package.contract.auth.clone();
        narrow_auth_to_granted_secrets(&mut granted, Some(&authority), false).unwrap();
        assert_eq!(granted, package.contract.auth);
    }

    #[test]
    fn an_optional_secret_left_ungranted_runs_without_it() {
        let source = include_str!("../../../../skillshub/github-search/SKILL.md");
        let package = parse_skill_runtime_package(source).unwrap().unwrap();
        let mut auth = package.contract.auth.clone();
        narrow_auth_to_granted_secrets(&mut auth, None, false).unwrap();
        assert_eq!(auth, AuthContract::default());
    }

    /// Live, end to end: the reviewed Tavily skill runs in the brokered-egress
    /// jail with a key issued from a vault by the sealed adapter. The key is
    /// a deliberately invalid test value: reaching `api.tavily.com` at all
    /// proves it was injected (without it the script stops locally with
    /// "TAVILY_API_KEY not set" and opens no connection).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "reaches api.tavily.com through the brokered-egress jail"]
    fn live_secret_is_injected_and_only_reaches_its_host() {
        let source = include_bytes!("../../../../skillshub/news-search-via-tavily/SKILL.md");
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source)
            .expect("a secrets skill with egress is admitted");
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("tavily-search");
        fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../skillshub/news-search-via-tavily/bin/tavily-search"
            ),
            &bin,
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let bytes = fs::read(&bin).unwrap();
        let artifact = AppOsJailVerifiedArtifact {
            identity: AppOsJailPhysicalArtifactIdentity {
                revision_ref: AppReference::parse("artifact:reviewed-tool:v1").unwrap(),
                digest: AppDigest::blake3(&bytes),
            },
            executable_directory: fs::canonicalize(directory.path()).unwrap(),
            kind: AppOsJailArtifactKind::Python3Script,
            in_place: None,
        };
        let vault_root = tempfile::tempdir().unwrap();
        let resolver = Arc::new(
            crate::magician_v2::secrets::SecretStoreResolver::new_with_capabilities(
                Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
                vault_root.path().to_path_buf(),
                crate::magician_v2::secrets::SecretRuntimeCapabilities::fully_available("test"),
            ),
        );
        resolver
            .resolve_for_scope("owner", "default")
            .unwrap()
            .store_provisioned(
                "TAVILY_API_KEY",
                "Test Tavily key",
                std::collections::HashMap::from([(
                    "value".to_owned(),
                    "tvly-invalid-test-key".to_owned(),
                )]),
                crate::magician_v2::secrets::InjectionTarget::Header {
                    name: "Authorization".to_owned(),
                    prefix: Some("Bearer ".to_owned()),
                },
                crate::magician_v2::secrets::SecretPolicy {
                    allowed_domains: vec!["api.tavily.com".to_owned()],
                    ..Default::default()
                },
            )
            .unwrap();
        let prepared = adapter
            .lower_reviewed_action(
                "run",
                AppReference::parse("primitive:news-search-via-tavily").unwrap(),
                AppReference::parse("action:news-search-via-tavily:run").unwrap(),
                artifact,
                &serde_json::json!({"query": "agents"}),
            )
            .expect("lowered")
            .with_secret_authority(Some(AppOsJailSecretAuthority {
                store_resolver: Arc::clone(&resolver),
                principal: "owner".into(),
                workspace: "default".into(),
                granted: BTreeSet::from(["TAVILY_API_KEY".to_owned()]),
                key_scopes: BTreeMap::new(),
            }));
        let context = GovernedExecutionCallContext::new(
            CredentialCallId::new("app-live-secret").unwrap(),
            prepared.skill_name.clone(),
            prepared.action_name.clone(),
        )
        .unwrap();
        let execution = match prepared.execute_governed(context, &GovernedBatchCancellation::new())
        {
            Ok(execution) => execution,
            Err(failure) => panic!("jailed call failed: {}", failure.error),
        };
        let receipt = execution.egress.clone().expect("egress receipt");
        assert!(
            receipt.connections_tunnelled >= 1,
            "the key was not injected: stdout={:?} stderr={:?} egress={receipt:?}",
            execution.stdout.value,
            execution.stderr.value
        );
        assert_eq!(receipt.destination, "api.tavily.com");
        assert!(receipt.refusals.is_empty());
        let rendered = format!("{:?}{:?}", execution.stdout.value, execution.stderr.value);
        assert!(
            !rendered.contains("tvly-invalid-test-key"),
            "the key never appears in output"
        );
    }

    #[test]
    fn only_native_executables_and_exact_python3_scripts_are_artifacts() {
        assert_eq!(
            artifact_kind(b"\x7fELF\x02\x01"),
            Some(AppOsJailArtifactKind::Native)
        );
        assert_eq!(
            artifact_kind(&[0xcf, 0xfa, 0xed, 0xfe, 0, 0]),
            Some(AppOsJailArtifactKind::Native)
        );
        for script in [
            &b"#!/usr/bin/env python3\nprint(1)\n"[..],
            b"#!/usr/bin/python3\n",
        ] {
            assert_eq!(
                artifact_kind(script),
                Some(AppOsJailArtifactKind::Python3Script)
            );
        }
        for refused in [
            &b"#!/bin/sh\nexit 0\n"[..],
            b"#!/usr/bin/env python3 -u\n",
            b"#!/usr/bin/env python\n",
            b"#!/usr/local/bin/python3\n",
            b"#!/usr/bin/env bash\n",
            b"print(1)\n",
            b"",
        ] {
            assert_eq!(
                artifact_kind(refused),
                None,
                "{:?}",
                String::from_utf8_lossy(refused)
            );
        }
    }

    /// Live, end to end: the reviewed `arxiv-search` skill (a Python script)
    /// runs in the brokered-egress jail under the host's pinned interpreter,
    /// reaches only `export.arxiv.org`, and returns real results inside the
    /// app output budget. Run with `--ignored`; needs network, macOS
    /// `sandbox-exec` and a trusted Python 3 (CommandLineTools on stock macOS).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "runs arxiv-search against export.arxiv.org in the jail"]
    fn live_python_web_skill_runs_in_the_jail_through_the_broker() {
        let source = include_bytes!("../../../../skillshub/arxiv-search/SKILL.md");
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("arxiv-search");
        fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../skillshub/arxiv-search/bin/arxiv-search"
            ),
            &bin,
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let bytes = fs::read(&bin).unwrap();
        assert_eq!(
            artifact_kind(&bytes),
            Some(AppOsJailArtifactKind::Python3Script)
        );
        let artifact = AppOsJailVerifiedArtifact {
            identity: AppOsJailPhysicalArtifactIdentity {
                revision_ref: AppReference::parse("artifact:reviewed-tool:v1").unwrap(),
                digest: AppDigest::blake3(&bytes),
            },
            executable_directory: fs::canonicalize(directory.path()).unwrap(),
            kind: AppOsJailArtifactKind::Python3Script,
            in_place: None,
        };
        let prepared = adapter
            .lower_reviewed_action(
                "run",
                AppReference::parse("primitive:arxiv-search").unwrap(),
                AppReference::parse("action:arxiv-search:run").unwrap(),
                artifact,
                &serde_json::json!({"query": "agents", "limit": 2}),
            )
            .expect("lowered");
        let context = GovernedExecutionCallContext::new(
            CredentialCallId::new("app-live-python").unwrap(),
            prepared.skill_name.clone(),
            prepared.action_name.clone(),
        )
        .unwrap();
        let execution = match prepared.execute_governed(context, &GovernedBatchCancellation::new())
        {
            Ok(execution) => execution,
            Err(failure) => panic!("jailed call failed: {}", failure.error),
        };
        assert_eq!(
            execution.exit_code,
            Some(0),
            "terminal={:?} stdout={:?} stderr={:?} egress={:?}",
            execution.terminal,
            execution.stdout.value,
            execution.stderr.value,
            execution.egress
        );
        let items = execution.stdout.value["items"]
            .as_array()
            .expect("json items");
        assert!(!items.is_empty());
        let receipt = execution.egress.expect("egress receipt");
        assert_eq!(receipt.destination, "export.arxiv.org");
        assert!(receipt.connections_tunnelled >= 1);
        assert!(receipt.refusals.is_empty());
    }

    #[test]
    fn a_reviewed_web_skill_is_admitted_with_its_declared_egress() {
        let source = include_bytes!("../../../../skillshub/arxiv-search/SKILL.md");
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source)
            .expect("arxiv-search is admitted: workspace scope is the jail's private workdir");
        let egress = adapter.egress.as_ref().expect("declared egress");
        assert_eq!(egress.destination(), "export.arxiv.org");
        let ceiling =
            reviewed_transport_result_byte_ceiling(&adapter.package.contract, Some(egress))
                .expect("budgeted ceiling");
        assert!(ceiling <= MAX_APP_OS_JAIL_DURABLE_RESULT_BYTES);
        assert!(adapter
            .actions
            .values()
            .all(|plan| plan.transport_result_byte_ceiling == ceiling));
        // The run contract no longer carries the workspace scope the jail
        // satisfies, so the governed authorizer sees a default floor.
        assert_eq!(
            adapter.package.contract.policy_floor,
            PolicyFloor::default()
        );

        // Without its declaration the same skill keeps the no-network recipe:
        // admitted (budgeted streams), but with no egress.
        let text = std::str::from_utf8(source).unwrap();
        let undeclared = text.replacen("    app_egress:\n", "    app_egress_removed:\n", 1);
        let local =
            AppOsJailPhysicalOwnerAdapter::from_reviewed_source(undeclared.as_bytes()).unwrap();
        assert!(local.egress.is_none());
    }

    #[test]
    fn egress_changes_the_plan_digest_and_attests_an_external_destination() {
        let source = include_bytes!("../../../../skillshub/arxiv-search/SKILL.md");
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source).unwrap();
        let egress = adapter.egress.clone().unwrap();
        let (name, action) = adapter.compiled.actions.iter().next().unwrap();
        let with = implementation_plan_digest(
            &adapter.source_digest,
            &adapter.package.contract,
            name,
            action,
            Some(&egress),
        )
        .unwrap();
        let without = implementation_plan_digest(
            &adapter.source_digest,
            &bounded_cli_contract(Some(1_024), Some(512)),
            name,
            action,
            None,
        );
        assert_ne!(Some(with), without);

        let tool_ref = AppReference::parse("capability:arxiv-search").unwrap();
        let target_ref = AppReference::parse("runtime:app-os-jail:v1:test").unwrap();
        let issued_at = chrono::Utc::now();
        let admission = AppOsJailEgressAdmission::for_call(Some(&egress), None, false).unwrap();
        let target = os_jail_egress_target(&tool_ref, &target_ref, &admission, issued_at).unwrap();
        assert_eq!(
            target.runtime_ref(),
            None,
            "egress calls are external, not local"
        );
        assert_eq!(
            target,
            os_jail_egress_target(&tool_ref, &target_ref, &admission, issued_at).unwrap(),
            "re-attesting a prepared action is byte-identical"
        );
    }

    #[test]
    fn transport_ceiling_requires_explicit_stream_contract_and_rejects_oversize() {
        assert_eq!(
            reviewed_transport_result_byte_ceiling(
                &bounded_cli_contract(Some(1_024), Some(512)),
                None
            ),
            Some(
                (1_024 + 512) * APP_OS_JAIL_RESULT_JSON_EXPANSION
                    + APP_OS_JAIL_RESULT_ENVELOPE_BYTES,
            ),
        );
        assert_eq!(
            reviewed_transport_result_byte_ceiling(&bounded_cli_contract(None, Some(512)), None),
            None,
        );
        assert_eq!(
            reviewed_transport_result_byte_ceiling(
                &bounded_cli_contract(
                    Some(tool_runtime_core::manifest_validation::MAX_RUNTIME_STREAM_BYTES),
                    Some(tool_runtime_core::manifest_validation::MAX_RUNTIME_STREAM_BYTES),
                ),
                None,
            ),
            Some(
                (APP_OS_JAIL_STDOUT_BUDGET + APP_OS_JAIL_STDERR_BUDGET)
                    * APP_OS_JAIL_RESULT_JSON_EXPANSION
                    + APP_OS_JAIL_RESULT_ENVELOPE_BYTES
            ),
            "oversized authored streams run on the app budget",
        );
    }

    #[test]
    fn a_skill_with_egress_runs_on_the_app_stream_budget_instead_of_being_refused() {
        let egress = super::super::os_jail_egress::parse_app_egress_declaration(
            "---\nname: web\ndescription: d\nmetadata:\n  magician:\n    app_egress:\n      schema_version: 1\n      destination: api.example.com\n---\n",
        )
        .unwrap();
        let web = bounded_cli_contract(Some(10 * 1024 * 1024), Some(2 * 1024 * 1024));
        assert_eq!(
            reviewed_transport_result_byte_ceiling(&web, None),
            reviewed_transport_result_byte_ceiling(&web, egress.as_ref())
        );
        let ceiling = reviewed_transport_result_byte_ceiling(&web, egress.as_ref()).unwrap();
        assert_eq!(
            ceiling,
            (APP_OS_JAIL_STDOUT_BUDGET + APP_OS_JAIL_STDERR_BUDGET)
                * APP_OS_JAIL_RESULT_JSON_EXPANSION
                + APP_OS_JAIL_RESULT_ENVELOPE_BYTES
        );
        assert!(ceiling <= MAX_APP_OS_JAIL_DURABLE_RESULT_BYTES);
        // A skill already inside the budget keeps its own smaller limits.
        let small = bounded_cli_contract(Some(1_024), Some(512));
        assert_eq!(
            reviewed_transport_result_byte_ceiling(&small, egress.as_ref()),
            reviewed_transport_result_byte_ceiling(&small, None)
        );
    }

    #[test]
    fn runtime_target_identity_binds_reviewed_transport_ceiling() {
        let plan = AppDigest::blake3(b"plan");
        let revision = AppReference::parse("artifact:reviewed-tool:v1").unwrap();
        let artifact = AppDigest::blake3(b"artifact");
        let smaller = runtime_target_ref(&plan, 8 * 1024, &revision, &artifact).unwrap();
        let larger = runtime_target_ref(&plan, 16 * 1024, &revision, &artifact).unwrap();
        assert_ne!(smaller, larger);
    }

    #[test]
    fn first_jail_vertical_refuses_additive_governed_authority() {
        let mut policy = EffectiveActionPolicy {
            required_approvals: BTreeSet::new(),
            required_grants: BTreeSet::new(),
            resource_scopes: BTreeSet::new(),
            required_resource_authorities: BTreeSet::new(),
        };
        assert!(!action_policy_requires_authority(&policy));
        policy.required_grants.insert("workspace-read".to_owned());
        assert!(action_policy_requires_authority(&policy));
    }

    #[cfg(unix)]
    fn write_native_fixture(path: &Path, tail: u8) {
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
        let mut bytes = vec![0x7f, b'E', b'L', b'F'];
        bytes.extend(vec![tail; 32]);
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_artifact_review_refuses_drift_delete_symlink_and_scripts() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let bin = source.join("bin");
        let apps = fixture.path().join("apps");
        fs::create_dir_all(&bin).unwrap();
        let source = fs::canonicalize(source).unwrap();
        let executable = source.join("bin").join("reviewed-tool");
        write_native_fixture(&executable, 1);
        let store = AppOsJailArtifactStore::open_or_create(&apps).unwrap();
        let identity = review_private_artifact(&source, store.root(), "reviewed-tool")
            .unwrap()
            .expect("native private artifact");
        reopen_physical_artifact(
            store.root(),
            "reviewed-tool",
            identity.revision_ref(),
            identity.digest(),
        )
        .expect("unchanged content-addressed artifact");

        let stored = store
            .root()
            .join(physical_artifact_key("reviewed-tool", identity.digest()))
            .join("reviewed-tool");

        // Equal bytes under another executable name must never cohabit one
        // mounted bundle: otherwise either action could exec its unreviewed
        // sibling after admission.
        let alias = source.join("bin").join("alias-tool");
        fs::copy(&executable, &alias).unwrap();
        fs::set_permissions(&alias, fs::Permissions::from_mode(0o700)).unwrap();
        let alias_identity = review_private_artifact(&source, store.root(), "alias-tool")
            .unwrap()
            .expect("equal bytes under an exact distinct name");
        assert_ne!(identity.revision_ref(), alias_identity.revision_ref());

        let adjacent = stored.parent().unwrap().join("unreviewed-sibling");
        write_native_fixture(&adjacent, 1);
        assert!(reopen_physical_artifact(
            store.root(),
            "reviewed-tool",
            identity.revision_ref(),
            identity.digest(),
        )
        .is_err());
        fs::remove_file(adjacent).unwrap();

        write_native_fixture(&stored, 2);
        assert!(reopen_physical_artifact(
            store.root(),
            "reviewed-tool",
            identity.revision_ref(),
            identity.digest(),
        )
        .is_err());

        fs::remove_file(&stored).unwrap();
        symlink(&executable, &stored).unwrap();
        assert!(reopen_physical_artifact(
            store.root(),
            "reviewed-tool",
            identity.revision_ref(),
            identity.digest(),
        )
        .is_err());
        fs::remove_file(&stored).unwrap();
        assert!(reopen_physical_artifact(
            store.root(),
            "reviewed-tool",
            identity.revision_ref(),
            identity.digest(),
        )
        .is_err());

        let script = source.join("bin").join("script-tool");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            review_private_artifact(&source, store.root(), "script-tool")
                .unwrap()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn selected_artifact_revalidation_does_not_scan_unselected_actions() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let bin = source.join("bin");
        let apps = fixture.path().join("apps");
        fs::create_dir_all(&bin).unwrap();
        let source = fs::canonicalize(source).unwrap();
        let selected = source.join("bin").join("selected-tool");
        let unselected = source.join("bin").join("unselected-tool");
        write_native_fixture(&selected, 1);
        write_native_fixture(&unselected, 2);
        let store = AppOsJailArtifactStore::open_or_create(&apps).unwrap();
        let selected_identity = review_private_artifact(&source, store.root(), "selected-tool")
            .unwrap()
            .unwrap();
        let unselected_identity = review_private_artifact(&source, store.root(), "unselected-tool")
            .unwrap()
            .unwrap();
        let unselected_stored = store
            .root()
            .join(physical_artifact_key(
                "unselected-tool",
                unselected_identity.digest(),
            ))
            .join("unselected-tool");
        write_native_fixture(&unselected_stored, 3);

        reopen_physical_artifact(
            store.root(),
            "selected-tool",
            selected_identity.revision_ref(),
            selected_identity.digest(),
        )
        .expect("the exact selected artifact remains independently valid");
        assert!(reopen_physical_artifact(
            store.root(),
            "unselected-tool",
            unselected_identity.revision_ref(),
            unselected_identity.digest(),
        )
        .is_err());
    }

    #[test]
    fn typed_output_is_bounded_and_does_not_trust_embedded_labels() {
        let bytes = br#"{"handling_labels":{"classification":"public"},"value":7}"#;
        let output = AppOsJailTypedOutput::parse(bytes).unwrap();
        assert_eq!(output.kind(), AppOsJailOutputKind::Json);
        assert_eq!(output.value()["value"], 7);
        assert_eq!(output.byte_count(), bytes.len() as u64);
    }

    #[test]
    fn deeply_nested_output_is_refused_iteratively() {
        let mut value = Value::Null;
        for _ in 0..=MAX_APP_OS_JAIL_JSON_DEPTH {
            value = Value::Array(vec![value]);
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            AppOsJailTypedOutput::parse(&bytes),
            Err(AppOsJailError::InvalidOutput)
        ));
    }

    #[test]
    fn malformed_json_looking_output_is_not_reclassified_as_text() {
        assert!(matches!(
            AppOsJailTypedOutput::parse(br#"{"value":1"#),
            Err(AppOsJailError::InvalidOutput)
        ));
        let text = AppOsJailTypedOutput::parse(b"ordinary text").unwrap();
        assert_eq!(text.kind(), AppOsJailOutputKind::Text);
        // Standard error is diagnostics: a bracketed log line is text there.
        let log = AppOsJailTypedOutput::parse_diagnostics(b"[TOOL] started\n").unwrap();
        assert_eq!(log.kind(), AppOsJailOutputKind::Text);
        assert!(AppOsJailTypedOutput::parse(b"[TOOL] started\n").is_err());
    }

    fn skillshub(skill: &str) -> PathBuf {
        fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../skillshub").join(skill))
            .unwrap()
    }

    #[test]
    fn a_skill_that_spawns_its_runtime_runs_in_place_and_single_files_do_not() {
        let minimax = include_bytes!("../../../../skillshub/web-search-via-minimax/SKILL.md");
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(minimax)
            .expect("a provider label and a config-file key are admitted in place");
        assert!(adapter.requires_in_place);
        assert_eq!(adapter.egress.as_ref().unwrap().destinations(), ["api.minimax.io"]);
        assert!(adapter.actions["run"].execution_plan_digest.is_some());
        assert!(adapter.runs_in_place(&skillshub("web-search-via-minimax"), "minimax-websearch"));
        assert!(source_runs_in_place(&adapter.package.contract, None));

        // A single exact-python3 file with no companions keeps its copied kind.
        let tavily = include_bytes!("../../../../skillshub/news-search-via-tavily/SKILL.md");
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(tavily).unwrap();
        assert!(!adapter.requires_in_place);
        assert!(!adapter.runs_in_place(&skillshub("news-search-via-tavily"), "tavily-search"));
        // A skill with no `bin/` of its own (a system tool) never runs in place.
        let jq = include_bytes!("../../../../skillshub/jq/SKILL.md");
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(jq).unwrap();
        assert!(!adapter.runs_in_place(&skillshub("jq"), "jq"));
    }

    /// The recipes existing single-file locks were reviewed under, restated
    /// exactly: only the implementation digest (which folds in every jail
    /// source and so changes with any release) may move them.
    #[test]
    fn existing_kinds_keep_their_plan_recipes() {
        for (skill, source) in [
            (
                "arxiv-search",
                include_str!("../../../../skillshub/arxiv-search/SKILL.md"),
            ),
            (
                "news-search-via-tavily",
                include_str!("../../../../skillshub/news-search-via-tavily/SKILL.md"),
            ),
            ("jq", include_str!("../../../../skillshub/jq/SKILL.md")),
        ] {
            let adapter =
                AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source.as_bytes()).unwrap();
            let package = parse_skill_runtime_package(source).unwrap().unwrap();
            let contract = app_jail_contract(&package.contract);
            let egress = parse_app_egress_declaration(source).unwrap();
            for (name, action) in &adapter.compiled.actions {
                let Some(actual) = implementation_plan_digest(
                    &adapter.source_digest,
                    &contract,
                    name,
                    action,
                    egress.as_ref(),
                ) else {
                    continue;
                };
                let ceiling =
                    reviewed_transport_result_byte_ceiling(&contract, egress.as_ref()).unwrap();
                let runtime = os_jail_runtime_implementation_digest().unwrap();
                let expected = match &egress {
                    Some(egress) => {
                        let mut recipe = serde_json::json!({
                            "profile": APP_OS_JAIL_PROFILE_V1,
                            "implementation_revision": APP_OS_JAIL_IMPLEMENTATION_REVISION,
                            "runtime_implementation_digest": runtime,
                            "core_profile": GOVERNED_PROCESS_JAIL_BROKERED_EGRESS_V1,
                            "egress": {
                                "profile": APP_OS_JAIL_EGRESS_PROFILE_V1,
                                "destination": egress.destination(),
                                "port": APP_OS_JAIL_EGRESS_PORT,
                                "limits": APP_OS_JAIL_EGRESS_LIMITS,
                                "stdout_budget": APP_OS_JAIL_STDOUT_BUDGET,
                                "stderr_budget": APP_OS_JAIL_STDERR_BUDGET,
                            },
                            "source_digest": adapter.source_digest,
                            "transport_result_byte_ceiling": ceiling,
                            "action": name,
                            "definition_schema": &action.definition.input_schema,
                            "invocation": &action.invocation,
                            "effective_policy": &action.effective_policy,
                        });
                        if contract.auth != AuthContract::default() {
                            recipe["auth"] = serde_json::to_value(&contract.auth).unwrap();
                        }
                        recipe
                    },
                    None => serde_json::json!({
                        "profile": APP_OS_JAIL_PROFILE_V1,
                        "implementation_revision": APP_OS_JAIL_IMPLEMENTATION_REVISION,
                        "runtime_implementation_digest": runtime,
                        "core_profile": GOVERNED_PROCESS_JAIL_V1,
                        "source_digest": adapter.source_digest,
                        "transport_result_byte_ceiling": ceiling,
                        "action": name,
                        "definition_schema": &action.definition.input_schema,
                        "invocation": &action.invocation,
                        "effective_policy": &action.effective_policy,
                    }),
                };
                assert_eq!(
                    actual,
                    AppDigest::blake3_canonical_json(&expected).unwrap(),
                    "{skill}:{name}"
                );
            }
        }
    }

    #[test]
    fn one_admitted_host_keeps_its_attestation_and_several_bind_every_host() {
        let tool_ref = AppReference::parse("capability:web").unwrap();
        let target_ref = AppReference::parse("runtime:app-os-jail:v1:test").unwrap();
        let issued_at = chrono::Utc::now();
        let single = AppOsJailEgressAdmission::Hosts {
            hosts: vec!["api.example.com".into()],
        };
        let target = os_jail_egress_target(&tool_ref, &target_ref, &single, issued_at).unwrap();
        // The exact configuration every single-destination call was attested
        // with before host sets existed.
        let reviewed = AppDigest::blake3_canonical_json(&serde_json::json!({
            "profile": APP_OS_JAIL_EGRESS_PROFILE_V1,
            "jail_profile": GOVERNED_PROCESS_JAIL_BROKERED_EGRESS_V1,
            "runtime_target_ref": target_ref,
            "destination": "api.example.com",
            "port": APP_OS_JAIL_EGRESS_PORT,
            "limits": APP_OS_JAIL_EGRESS_LIMITS,
            "resolution": "broker_resolves_public_addresses_only",
        }))
        .unwrap();
        assert_eq!(
            target.endpoint().unwrap().configuration_digest(),
            &reviewed
        );
        assert_eq!(
            target.destination().unwrap().as_str(),
            "destination:api.example.com"
        );

        let several = AppOsJailEgressAdmission::Hosts {
            hosts: vec!["a.example.com".into(), "b.example.com".into()],
        };
        let target = os_jail_egress_target(&tool_ref, &target_ref, &several, issued_at).unwrap();
        assert_ne!(target.endpoint().unwrap().configuration_digest(), &reviewed);
        let any = os_jail_egress_target(
            &tool_ref,
            &target_ref,
            &AppOsJailEgressAdmission::AnyPublicHost,
            issued_at,
        )
        .unwrap();
        assert_eq!(
            any.destination().unwrap().as_str(),
            super::super::os_jail_egress::APP_OS_JAIL_ANY_PUBLIC_HOST_REF
        );
        assert_ne!(
            any.endpoint().unwrap().configuration_digest(),
            target.endpoint().unwrap().configuration_digest()
        );
    }

    /// A fixture skill that reads its key from `$MMX_CONFIG_DIR/config.json`
    /// and prints only a digest of what it read, plus whether it can see its
    /// own `config/.env` and the home directory.
    #[cfg(target_os = "macos")]
    const CONFIG_FILE_FIXTURE: &str = "#!/usr/bin/env python3\nimport hashlib, json, os, sys\nconfig = os.path.join(os.environ.get('MMX_CONFIG_DIR', '/nonexistent'), 'config.json')\ntry:\n    key = json.load(open(config))['api_key']\nexcept Exception as error:\n    print(json.dumps({'error': type(error).__name__})); sys.exit(0)\nhere = os.path.dirname(os.path.dirname(os.path.realpath(sys.argv[0])))\ndef readable(path):\n    try:\n        open(path).read(1); return True\n    except Exception:\n        return False\nprint(json.dumps({'key_sha256': hashlib.sha256(key.encode()).hexdigest(), 'dir_private': oct(os.stat(os.environ['MMX_CONFIG_DIR']).st_mode & 0o777), 'config_env_readable': readable(os.path.join(here, 'config', '.env')), 'skill_readable': readable(os.path.join(here, 'SKILL.md'))}))\n";

    #[cfg(target_os = "macos")]
    const CONFIG_FILE_FIXTURE_SKILL: &str = "---\nname: config-file-fixture\nversion: 1.0.0\ndescription: Reads a key from a config directory.\nmetadata:\n  magician:\n    skill_type: tool\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires: {bins: [config-file-fixture]}\n      runtime:\n        protocol: cli\n        command_prefix: []\n        limits:\n          timeout_secs: 30\n          stdout_bytes: 4096\n          stderr_bytes: 1024\n      auth:\n        kind: secrets\n        requirement: required\n        provider: minimax\n        secret_bindings:\n          - name: FIXTURE_KEY\n            secret_ref: FIXTURE_KEY\n        injections:\n          - source: {kind: secret, binding: FIXTURE_KEY}\n            target: {kind: config_directory, name: MMX_CONFIG_DIR}\n    runtime_actions:\n      schema_version: tool-runtime.typed-action-overrides.v1\n      actions:\n        run:\n          description: Read the key.\n          fixed_args: []\n---\nFixture.\n";

    #[cfg(target_os = "macos")]
    fn in_memory_vault(
        secret_ref: &str,
        value: &str,
        allowed_domains: Vec<String>,
    ) -> (tempfile::TempDir, Arc<crate::magician_v2::secrets::SecretStoreResolver>) {
        let vault_root = tempfile::tempdir().unwrap();
        let resolver = Arc::new(
            crate::magician_v2::secrets::SecretStoreResolver::new_with_capabilities(
                Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
                vault_root.path().to_path_buf(),
                crate::magician_v2::secrets::SecretRuntimeCapabilities::fully_available("test"),
            ),
        );
        resolver
            .resolve_for_scope("owner", "default")
            .unwrap()
            .store_provisioned(
                secret_ref,
                "Test key",
                std::collections::HashMap::from([("value".to_owned(), value.to_owned())]),
                crate::magician_v2::secrets::InjectionTarget::Header {
                    name: "Authorization".to_owned(),
                    prefix: None,
                },
                crate::magician_v2::secrets::SecretPolicy {
                    allowed_domains,
                    ..Default::default()
                },
            )
            .unwrap();
        (vault_root, resolver)
    }

    /// Run `bin/<executable>` of `package` in place, as the workflow would.
    #[cfg(target_os = "macos")]
    #[allow(clippy::too_many_arguments)]
    fn run_in_place(
        source: &[u8],
        package: &Path,
        action: &str,
        input: serde_json::Value,
        secret: Option<(&str, Arc<crate::magician_v2::secrets::SecretStoreResolver>)>,
        grant: Option<AppOsJailNetworkGrant>,
        scope: Option<AppOsJailKeyScope>,
    ) -> Result<AppOsJailExecution, AppOsJailExecutionFailure> {
        let prepared = prepare_in_place(source, package, action, input, secret, grant, scope);
        let context = GovernedExecutionCallContext::new(
            CredentialCallId::new("app-live-in-place").unwrap(),
            prepared.skill_name.clone(),
            prepared.action_name.clone(),
        )
        .unwrap();
        prepared.execute_governed(context, &GovernedBatchCancellation::new())
    }

    /// Lower `bin/<executable>` of `package` for an in-place run, as the
    /// workflow would, with the owner's key, pin and network grant.
    #[cfg(target_os = "macos")]
    fn prepare_in_place(
        source: &[u8],
        package: &Path,
        action: &str,
        input: serde_json::Value,
        secret: Option<(&str, Arc<crate::magician_v2::secrets::SecretStoreResolver>)>,
        grant: Option<AppOsJailNetworkGrant>,
        scope: Option<AppOsJailKeyScope>,
    ) -> AppOsJailPreparedAction {
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source).unwrap();
        let executable = adapter.compiled.actions[action].invocation.executable.clone();
        assert!(adapter.runs_in_place(package, &executable));
        let derived = adapter
            .derive_in_place(package, &executable)
            .unwrap_or_else(|error| panic!("derivation: {error}"));
        let artifact = AppOsJailVerifiedArtifact {
            identity: AppOsJailPhysicalArtifactIdentity {
                revision_ref: derived.revision_ref().unwrap(),
                digest: derived.program_digest().clone(),
            },
            executable_directory: derived.program_directory().to_path_buf(),
            kind: AppOsJailArtifactKind::InPlaceSkill,
            in_place: Some(Box::new(derived)),
        };
        let prepared = adapter
            .lower_reviewed_action(
                action,
                AppReference::parse("primitive:fixture").unwrap(),
                AppReference::parse("action:fixture:run").unwrap(),
                artifact,
                &input,
            )
            .expect("lowered")
            .with_network_grant(grant)
            .with_secret_authority(secret.map(|(secret_ref, store_resolver)| {
                AppOsJailSecretAuthority {
                    store_resolver,
                    principal: "owner".into(),
                    workspace: "default".into(),
                    granted: BTreeSet::from([secret_ref.to_owned()]),
                    key_scopes: scope
                        .map(|scope| BTreeMap::from([(secret_ref.to_owned(), scope)]))
                        .unwrap_or_default(),
                }
            }));
        prepared
    }

    /// In place, a config-file key reaches the tool only through a fresh
    /// private directory: the tool reads exactly the vault value, the
    /// directory is 0700 and gone after the call, the value is in no
    /// identity or output, and the skill's own `config/` stays unreadable.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "runs a fixture skill in place in the jail"]
    fn a_config_file_key_is_delivered_only_through_the_private_directory() {
        let skills = tempfile::tempdir().unwrap();
        let skills_root = fs::canonicalize(skills.path()).unwrap();
        fs::set_permissions(&skills_root, fs::Permissions::from_mode(0o755)).unwrap();
        let package = skills_root.join("config-file-fixture");
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::create_dir_all(package.join("config")).unwrap();
        fs::write(package.join("config/.env"), "FIXTURE_KEY=never-read\n").unwrap();
        fs::write(package.join("SKILL.md"), CONFIG_FILE_FIXTURE_SKILL).unwrap();
        let program = package.join("bin/config-file-fixture");
        fs::write(&program, CONFIG_FILE_FIXTURE).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

        const VALUE: &str = "fixture-secret-value-7f3a";
        let (_vault, resolver) = in_memory_vault("FIXTURE_KEY", VALUE, Vec::new());
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(
            CONFIG_FILE_FIXTURE_SKILL.as_bytes(),
        )
        .unwrap();
        let derived = adapter
            .derive_in_place(&package, "config-file-fixture")
            .unwrap();
        let identity_text = format!(
            "{:?}{:?}{:?}",
            derived.revision_ref(),
            derived.identity(),
            adapter.actions["run"].execution_plan_digest
        );
        assert!(!identity_text.contains(VALUE));

        let execution = match run_in_place(
            CONFIG_FILE_FIXTURE_SKILL.as_bytes(),
            &package,
            "run",
            serde_json::json!({}),
            Some(("FIXTURE_KEY", resolver)),
            // The fixture declares no host: its key is pinned to one of the
            // app's hosts, which the broker then admits alone.
            Some(AppOsJailNetworkGrant::from_destination_refs(
                ["destination:api.example.com"],
                false,
            )),
            Some(AppOsJailKeyScope::Hosts(BTreeSet::from([
                "api.example.com".to_owned()
            ]))),
        ) {
            Ok(execution) => execution,
            Err(failure) => panic!("jailed call failed: {}", failure.error),
        };
        assert_eq!(execution.exit_code, Some(0), "{:?}", execution.stderr.value);
        let output = &execution.stdout.value;
        use sha2::Digest as _;
        assert_eq!(
            output["key_sha256"],
            serde_json::json!(format!("{:x}", sha2::Sha256::digest(VALUE.as_bytes()))),
            "{output}"
        );
        assert_eq!(output["dir_private"], "0o700", "{output}");
        assert_eq!(output["config_env_readable"], false, "{output}");
        assert_eq!(output["skill_readable"], true, "{output}");
        let rendered = format!("{:?}{:?}", execution.stdout.value, execution.stderr.value);
        assert!(!rendered.contains(VALUE));
        // (The private directory's removal is covered by
        // `a_private_credential_root_is_0700_and_removed_on_drop`.)
    }

    /// `MINIMAX_API_KEY` as the secrets adapter sees it: the runtime root's
    /// `.env`, else the default `~/MagicianNotes/.env`. Never printed.
    #[cfg(target_os = "macos")]
    fn runtime_env_secret(name: &str) -> Option<String> {
        let mut files = vec![crate::magician_v2::process_storage::runtime_root().join(".env")];
        if let Some(home) = std::env::var_os("HOME") {
            files.push(PathBuf::from(home).join("MagicianNotes/.env"));
        }
        files.into_iter().find_map(|file| {
            fs::read_to_string(file).ok()?.lines().find_map(|line| {
                let value = line.trim().strip_prefix(name)?.strip_prefix('=')?;
                let value = value.trim().trim_matches('"').trim_matches('\'');
                (!value.is_empty()).then(|| value.to_owned())
            })
        })
    }

    /// Live, end to end: `web-search-via-minimax` (a Python wrapper that
    /// spawns the Node `mmx` CLI from `skillshub/node_modules` under the
    /// vendored `skillshub/.node`) runs in place in the brokered jail with
    /// the owner-ticked `MINIMAX_API_KEY` delivered as `MMX_CONFIG_DIR`, and
    /// the app granted MiniMax's API host. Run with `--include-ignored`;
    /// needs network, the vendored Node and the key in the runtime `.env`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "reaches api.minimax.io in place through the brokered jail"]
    fn live_minimax_web_search_runs_in_place_with_its_config_file_key() {
        let Some(key) = runtime_env_secret("MINIMAX_API_KEY") else {
            eprintln!("skipped: MINIMAX_API_KEY is not in the runtime .env");
            return;
        };
        let package = skillshub("web-search-via-minimax");
        if !skillshub_runtimes_installed(
            include_bytes!("../../../../skillshub/web-search-via-minimax/SKILL.md"),
            &package,
            "run",
        ) {
            return;
        }
        let (_vault, resolver) =
            in_memory_vault("MINIMAX_API_KEY", &key, vec!["api.minimax.io".to_owned()]);
        // A malicious app may ask for more; the skill declares
        // `api.minimax.io`, so its key and its broker stay there.
        let grant = AppOsJailNetworkGrant::from_destination_refs(
            [
                "destination:api.minimax.io",
                "destination:collector.evil.com",
            ],
            true,
        );
        let execution = match run_in_place(
            include_bytes!("../../../../skillshub/web-search-via-minimax/SKILL.md"),
            &package,
            "run",
            serde_json::json!({"query": "rust programming language", "max_results": 3}),
            Some(("MINIMAX_API_KEY", resolver)),
            Some(grant),
            None,
        ) {
            Ok(execution) => execution,
            Err(failure) => panic!("jailed call failed: {}", failure.error),
        };
        let receipt = execution.egress.clone().expect("egress receipt");
        eprintln!(
            "minimax receipt: destination={} contacted={:?} refusals={:?} tunnelled={} bytes_up={} bytes_down={}",
            receipt.destination,
            receipt.contacted,
            receipt.refusals,
            receipt.connections_tunnelled,
            receipt.bytes_up,
            receipt.bytes_down
        );
        let rendered = format!("{:?}{:?}", execution.stdout.value, execution.stderr.value);
        assert!(!rendered.contains(&key), "the key never appears in output");
        assert_eq!(
            execution.exit_code,
            Some(0),
            "stdout={} stderr={}",
            execution.stdout.value,
            execution.stderr.value
        );
        let results = execution.stdout.value["results"]
            .as_array()
            .expect("normalized results");
        assert!(!results.is_empty());
        assert!(receipt.refusals.is_empty(), "{receipt:?}");
        assert!(receipt
            .contacted
            .iter()
            .all(|usage| usage.host == "api.minimax.io"));
        assert!(receipt.connections_tunnelled >= 1);
    }

    /// Connect to a started broker, send `CONNECT host:443` and return the
    /// status line.
    #[cfg(target_os = "macos")]
    fn broker_status(broker: &AppOsJailEgressBroker, host: &str) -> String {
        let GovernedEgressBrokerEndpoint::LoopbackTcp { port } = broker.endpoint() else {
            panic!("macOS brokers listen on loopback TCP");
        };
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port.get())).unwrap();
        stream
            .set_read_timeout(Some(StdDuration::from_secs(5)))
            .unwrap();
        stream
            .write_all(format!("CONNECT {host}:443 HTTP/1.1\r\n\r\n").as_bytes())
            .unwrap();
        let mut line = Vec::new();
        let mut byte = [0_u8; 1];
        while stream.read(&mut byte).is_ok_and(|read| read == 1) && byte[0] != b'\r' {
            line.push(byte[0]);
        }
        String::from_utf8_lossy(&line).into_owned()
    }

    /// Whether a real skillshub skill can be derived in place here: it needs
    /// the gitignored installed runtimes (`skillshub/node_modules`,
    /// `skillshub/.node`, `skillshub/.venv`), absent on a clean checkout and
    /// in CI. Says why when it cannot.
    #[cfg(target_os = "macos")]
    fn skillshub_runtimes_installed(source: &[u8], package: &Path, action: &str) -> bool {
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source).unwrap();
        let executable = adapter.compiled.actions[action]
            .invocation
            .executable
            .clone();
        match adapter.derive_in_place(package, &executable) {
            Ok(_) => true,
            Err(error) => {
                eprintln!(
                    "skipped: {} cannot run in place here ({error}); install the skillshub \
                     runtimes (make setup-skillshub-deps) to run this part",
                    package.display()
                );
                false
            },
        }
    }

    /// A malicious app asks for `[api.minimax.io, collector.evil.com]` and
    /// "any public host". A key only ever reaches the declared host, or the
    /// host(s) the owner picked for it; the broker refuses the other host
    /// while the key is injected.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_key_reaches_only_the_declared_or_pinned_host() {
        let malicious = || {
            AppOsJailNetworkGrant::from_destination_refs(
                [
                    "destination:api.minimax.io",
                    "destination:collector.evil.com",
                ],
                true,
            )
        };
        // Declared: web-search-via-minimax declares api.minimax.io. It runs
        // from the installed skillshub runtimes, so that part skips cleanly
        // without them; the fixture part below always runs.
        let minimax_source =
            include_bytes!("../../../../skillshub/web-search-via-minimax/SKILL.md");
        let minimax = skillshub("web-search-via-minimax");
        if skillshub_runtimes_installed(minimax_source, &minimax, "run") {
            let (_vault, resolver) = in_memory_vault("MINIMAX_API_KEY", "k", Vec::new());
            let declared = prepare_in_place(
                minimax_source,
                &minimax,
                "run",
                serde_json::json!({"query": "q"}),
                Some(("MINIMAX_API_KEY", resolver)),
                Some(malicious()),
                None,
            );
            let admission = declared.egress_admission().unwrap().unwrap();
            assert_eq!(
                admission,
                AppOsJailEgressAdmission::Hosts {
                    hosts: vec!["api.minimax.io".into()]
                }
            );
            let broker = AppOsJailEgressBroker::start(&admission).unwrap();
            assert_eq!(
                broker_status(&broker, "collector.evil.com"),
                "HTTP/1.1 403 Forbidden"
            );
            assert_eq!(broker.finish().refusals, ["host_not_granted"]);
        }

        // Undeclared: the owner pinned the key to api.minimax.io.
        let skills = tempfile::tempdir().unwrap();
        let skills_root = fs::canonicalize(skills.path()).unwrap();
        fs::set_permissions(&skills_root, fs::Permissions::from_mode(0o755)).unwrap();
        let package = skills_root.join("config-file-fixture");
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::write(package.join("SKILL.md"), CONFIG_FILE_FIXTURE_SKILL).unwrap();
        let program = package.join("bin/config-file-fixture");
        fs::write(&program, CONFIG_FILE_FIXTURE).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let undeclared = |scope: Option<AppOsJailKeyScope>, grant: AppOsJailNetworkGrant| {
            let (_vault, resolver) = in_memory_vault("FIXTURE_KEY", "k", Vec::new());
            prepare_in_place(
                CONFIG_FILE_FIXTURE_SKILL.as_bytes(),
                &package,
                "run",
                serde_json::json!({}),
                Some(("FIXTURE_KEY", resolver)),
                Some(grant),
                scope,
            )
        };
        let hosts = |hosts: &[&str]| {
            AppOsJailKeyScope::Hosts(hosts.iter().map(|host| (*host).to_owned()).collect())
        };
        let picked = undeclared(Some(hosts(&["api.minimax.io"])), malicious());
        let admission = picked.egress_admission().unwrap().unwrap();
        assert_eq!(
            admission,
            AppOsJailEgressAdmission::Hosts {
                hosts: vec!["api.minimax.io".into()]
            },
            "the picked set overrides the other host and \"any public host\""
        );
        assert_eq!(
            vault_domains(Some(&admission)),
            AppOsJailVaultDomains::One("api.minimax.io".into())
        );
        let broker = AppOsJailEgressBroker::start(&admission).unwrap();
        assert_eq!(
            broker_status(&broker, "collector.evil.com"),
            "HTTP/1.1 403 Forbidden"
        );
        assert_eq!(
            broker_status(&broker, "example.org"),
            "HTTP/1.1 403 Forbidden"
        );
        drop(broker);
        // A picked set of several hosts is the admitted set (and, once the
        // vault takes domain sets, the vault request).
        let both = undeclared(
            Some(hosts(&["api.minimax.io", "collector.evil.com"])),
            malicious(),
        );
        let admission = both.egress_admission().unwrap().unwrap();
        assert_eq!(
            admission,
            AppOsJailEgressAdmission::Hosts {
                hosts: vec!["api.minimax.io".into(), "collector.evil.com".into()]
            }
        );
        // The set passes through to the vault request as a domain set.
        let domains = vault_domains(Some(&admission));
        assert_eq!(
            domains,
            AppOsJailVaultDomains::Set(vec!["api.minimax.io".into(), "collector.evil.com".into()])
        );
        assert_eq!(
            credential_route_domains(&domains).unwrap(),
            crate::magician_v2::secrets::policy::RequestedDomains::hosts([
                "api.minimax.io",
                "collector.evil.com"
            ])
            .unwrap()
        );
        // Picked hosts the app does not grant (any-host alone never counts)
        // refuse the call.
        assert!(undeclared(
            Some(hosts(&["api.minimax.io"])),
            AppOsJailNetworkGrant::from_destination_refs([], true)
        )
        .egress_admission()
        .is_err());
        // "Any site" is used only with the app's any-host grant, and the
        // vault request is then Any.
        let any_site = undeclared(Some(AppOsJailKeyScope::AnySite), malicious())
            .egress_admission()
            .unwrap();
        assert_eq!(any_site, Some(AppOsJailEgressAdmission::AnyPublicHost));
        assert_eq!(vault_domains(any_site.as_ref()), AppOsJailVaultDomains::Any);
        assert_eq!(
            credential_route_domains(&AppOsJailVaultDomains::Any).unwrap(),
            crate::magician_v2::secrets::policy::RequestedDomains::Any
        );
        assert!(undeclared(
            Some(AppOsJailKeyScope::AnySite),
            AppOsJailNetworkGrant::from_destination_refs(["destination:api.minimax.io"], false)
        )
        .egress_admission()
        .is_err());
        // Without a scope the key is simply not used.
        let unscoped = undeclared(None, malicious());
        assert_eq!(
            unscoped.egress_admission().unwrap(),
            Some(AppOsJailEgressAdmission::AnyPublicHost)
        );
    }

    /// A fixture skill that declares no host and takes two keys.
    const TWO_KEY_FIXTURE_SKILL: &str = "---\nname: two-key-fixture\nversion: 1.0.0\ndescription: Uses two keys.\nmetadata:\n  magician:\n    skill_type: tool\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires: {bins: [two-key-fixture]}\n      runtime:\n        protocol: cli\n        command_prefix: []\n        limits:\n          timeout_secs: 30\n          stdout_bytes: 4096\n          stderr_bytes: 1024\n      auth:\n        kind: secrets\n        requirement: required\n        secret_bindings:\n          - name: KEY_A\n            secret_ref: KEY_A\n          - name: KEY_B\n            secret_ref: KEY_B\n        injections:\n          - source: {kind: secret, binding: KEY_A}\n            target: {kind: environment, name: KEY_A}\n          - source: {kind: secret, binding: KEY_B}\n            target: {kind: environment, name: KEY_B}\n    runtime_actions:\n      schema_version: tool-runtime.typed-action-overrides.v1\n      actions:\n        run:\n          description: Use both keys.\n          fixed_args: []\n---\nFixture.\n";

    /// A package directory holding `skill` with `bin/<name>` as the
    /// config-file fixture program.
    #[cfg(target_os = "macos")]
    fn fixture_package(skill: &str, name: &str) -> (tempfile::TempDir, PathBuf) {
        let skills = tempfile::tempdir().unwrap();
        let skills_root = fs::canonicalize(skills.path()).unwrap();
        fs::set_permissions(&skills_root, fs::Permissions::from_mode(0o755)).unwrap();
        let package = skills_root.join(name);
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::write(package.join("SKILL.md"), skill).unwrap();
        let program = package.join("bin").join(name);
        fs::write(&program, CONFIG_FILE_FIXTURE).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        (skills, package)
    }

    /// Two keys of one tool with different scopes: the call reaches only
    /// where both may go. An empty intersection refuses the call, and a
    /// picked key beside an "any site" key narrows the call to the picked
    /// hosts.
    #[cfg(target_os = "macos")]
    #[test]
    fn two_keys_with_different_scopes_intersect() {
        let (_skills, package) = fixture_package(TWO_KEY_FIXTURE_SKILL, "two-key-fixture");
        let grant = || {
            AppOsJailNetworkGrant::from_destination_refs(
                [
                    "destination:a.example.com",
                    "destination:b.example.com",
                    "destination:c.example.com",
                ],
                true,
            )
        };
        let hosts = |hosts: &[&str]| {
            AppOsJailKeyScope::Hosts(hosts.iter().map(|host| (*host).to_owned()).collect())
        };
        let admission = |a: AppOsJailKeyScope, b: AppOsJailKeyScope| {
            let (_vault, resolver) = in_memory_vault("KEY_A", "a", Vec::new());
            let mut prepared = prepare_in_place(
                TWO_KEY_FIXTURE_SKILL.as_bytes(),
                &package,
                "run",
                serde_json::json!({}),
                Some(("KEY_A", resolver)),
                Some(grant()),
                Some(a),
            );
            let authority = prepared.secret_authority.as_mut().unwrap();
            authority.granted.insert("KEY_B".to_owned());
            authority.key_scopes.insert("KEY_B".to_owned(), b);
            prepared.egress_admission()
        };
        assert_eq!(
            admission(
                hosts(&["a.example.com", "b.example.com"]),
                hosts(&["b.example.com", "c.example.com"])
            )
            .unwrap(),
            Some(AppOsJailEgressAdmission::Hosts {
                hosts: vec!["b.example.com".into()]
            })
        );
        assert!(matches!(
            admission(hosts(&["a.example.com"]), hosts(&["c.example.com"])),
            Err(AppOsJailError::SecretNotGranted)
        ));
        for (a, b) in [
            (hosts(&["a.example.com"]), AppOsJailKeyScope::AnySite),
            (AppOsJailKeyScope::AnySite, hosts(&["a.example.com"])),
        ] {
            assert_eq!(
                admission(a, b).unwrap(),
                Some(AppOsJailEgressAdmission::Hosts {
                    hosts: vec!["a.example.com".into()]
                })
            );
        }
        assert_eq!(
            admission(AppOsJailKeyScope::AnySite, AppOsJailKeyScope::AnySite).unwrap(),
            Some(AppOsJailEgressAdmission::AnyPublicHost)
        );
    }

    /// A contract's fixed environment reaches the child only after MagicRun's
    /// manifest validation of the exact contract the call provides from:
    /// loader and interpreter injection names are refused before anything
    /// launches, whether the reviewed skill declares them or they are added
    /// to the effective contract afterwards.
    #[cfg(target_os = "macos")]
    #[test]
    fn injection_environment_names_never_reach_an_in_place_child() {
        const NAMES: [&str; 12] = [
            "DYLD_INSERT_LIBRARIES",
            "DYLD_LIBRARY_PATH",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "LD_AUDIT",
            "NODE_OPTIONS",
            "NODE_PATH",
            "PYTHONPATH",
            "PYTHONSTARTUP",
            "PYTHONHOME",
            "BASH_ENV",
            "PERL5OPT",
        ];
        let declaring = |name: &str| {
            CONFIG_FILE_FIXTURE_SKILL.replace(
                "requires: {bins: [config-file-fixture]}",
                &format!("requires: {{bins: [config-file-fixture], environment: {{{name}: x}}}}"),
            )
        };
        // A plain public name is admitted, so the refusals below are the
        // names themselves.
        AppOsJailPhysicalOwnerAdapter::from_reviewed_source(declaring("FIXTURE_MODE").as_bytes())
            .expect("a plain fixed environment value is admitted");
        for name in NAMES {
            // Declared by the skill: the review refuses it.
            assert!(
                AppOsJailPhysicalOwnerAdapter::from_reviewed_source(declaring(name).as_bytes())
                    .is_err(),
                "{name}"
            );
        }
        let (_skills, package) = fixture_package(CONFIG_FILE_FIXTURE_SKILL, "config-file-fixture");
        for name in NAMES {
            // Added to the effective contract after lowering: the call's
            // own validation refuses it before launch.
            let (_vault, resolver) = in_memory_vault("FIXTURE_KEY", "k", Vec::new());
            let mut prepared = prepare_in_place(
                CONFIG_FILE_FIXTURE_SKILL.as_bytes(),
                &package,
                "run",
                serde_json::json!({}),
                Some(("FIXTURE_KEY", resolver)),
                Some(AppOsJailNetworkGrant::from_destination_refs(
                    ["destination:api.example.com"],
                    false,
                )),
                Some(AppOsJailKeyScope::Hosts(BTreeSet::from([
                    "api.example.com".to_owned(),
                ]))),
            );
            prepared
                .effective_contract
                .requires
                .environment
                .insert(name.to_owned(), "x".to_owned());
            let context = GovernedExecutionCallContext::new(
                CredentialCallId::new("app-env-injection").unwrap(),
                prepared.skill_name.clone(),
                prepared.action_name.clone(),
            )
            .unwrap();
            let failure = prepared
                .execute_governed(context, &GovernedBatchCancellation::new())
                .err()
                .unwrap_or_else(|| panic!("{name} was not refused"));
            assert!(
                matches!(failure.error, AppOsJailError::IdentityMismatch),
                "{name}: {:?}",
                failure.error
            );
            assert!(failure.audit.is_none(), "{name}: nothing launched");
        }
    }

    /// Re-approval, through the install and dispatch path an app call takes:
    /// the review locks the in-place skill (`review_private_artifacts` into
    /// the primitive lock), dispatch revalidates that lock, lowers and runs
    /// it; a changed skill is refused (`SkillChangedSinceApproval`) until the
    /// owner re-reviews and approves the new lock, which runs.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "runs a fixture skill in place in the jail"]
    fn an_approved_in_place_skill_is_refused_after_a_change_until_re_approved() {
        let (_skills, package) = fixture_package(CONFIG_FILE_FIXTURE_SKILL, "config-file-fixture");
        let source = CONFIG_FILE_FIXTURE_SKILL.as_bytes();
        let store_root = tempfile::tempdir().unwrap();
        let store =
            AppOsJailArtifactStore::open_or_create(&store_root.path().join("apps")).unwrap();
        let descriptor =
            super::super::primitive_catalog::tool_skill_descriptor(source).expect("descriptor");
        let unlocked = AppLockedPrimitiveBinding::from_descriptor(&descriptor).unwrap();
        // Review and approve: the lock the installation stores.
        let approve = || {
            let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source).unwrap();
            let reviewed = adapter
                .review_private_artifacts(&unlocked, &package, &store)
                .unwrap()
                .into_iter()
                .map(|(name, identity)| {
                    (
                        name,
                        (identity.revision_ref().clone(), identity.digest().clone()),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            assert_eq!(reviewed.len(), 1, "the in-place action is locked");
            unlocked.clone().with_physical_artifacts(&reviewed).unwrap()
        };
        // One app call against a stored lock.
        let run =
            |locked: &AppLockedPrimitiveBinding| -> Result<AppOsJailExecution, AppOsJailError> {
                let mut adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(source)?;
                adapter.bind_locked_actions(locked)?;
                let action = &locked.actions()[0];
                let artifact =
                    adapter.revalidate_locked_artifact(locked, action, &store, Some(&package))?;
                let (_vault, resolver) = in_memory_vault("FIXTURE_KEY", "v", Vec::new());
                let prepared = adapter
                    .lower_locked_action(locked, action, artifact, b"{}")?
                    .with_network_grant(Some(AppOsJailNetworkGrant::from_destination_refs(
                        ["destination:api.example.com"],
                        false,
                    )))
                    .with_secret_authority(Some(AppOsJailSecretAuthority {
                        store_resolver: resolver,
                        principal: "owner".into(),
                        workspace: "default".into(),
                        granted: BTreeSet::from(["FIXTURE_KEY".to_owned()]),
                        key_scopes: BTreeMap::from([(
                            "FIXTURE_KEY".to_owned(),
                            AppOsJailKeyScope::Hosts(BTreeSet::from(
                                ["api.example.com".to_owned()],
                            )),
                        )]),
                    }));
                let context = GovernedExecutionCallContext::new(
                    CredentialCallId::new("app-re-approval").unwrap(),
                    prepared.skill_name.clone(),
                    prepared.action_name.clone(),
                )
                .unwrap();
                prepared
                    .execute_governed(context, &GovernedBatchCancellation::new())
                    .map_err(|failure| failure.error)
            };
        let ran = |execution: AppOsJailExecution| {
            assert_eq!(execution.exit_code, Some(0), "{:?}", execution.stderr.value);
            assert!(
                execution.stdout.value["key_sha256"].is_string(),
                "{}",
                execution.stdout.value
            );
        };
        let approved = approve();
        ran(run(&approved).expect("the approved skill runs"));
        // The skill changes after approval.
        fs::write(package.join("helper.py"), "X = 1\n").unwrap();
        assert!(matches!(
            run(&approved),
            Err(AppOsJailError::SkillChangedSinceApproval)
        ));
        // Re-review and approve: the new lock runs; the old one stays refused.
        let re_approved = approve();
        assert_ne!(re_approved, approved);
        ran(run(&re_approved).expect("the re-approved skill runs"));
        assert!(matches!(
            run(&approved),
            Err(AppOsJailError::SkillChangedSinceApproval)
        ));
    }

    #[cfg(target_os = "macos")]
    fn youtube_grant() -> AppOsJailNetworkGrant {
        // As the app's network policy grants it: declared ∩ granted.
        AppOsJailNetworkGrant::from_destination_refs(["destination:www.youtube.com"], false)
    }

    /// Live: `youtube-search` runs in place with its `yt-dlp` companion.
    /// yt-dlp is not installed everywhere; without it the test says so and
    /// skips (nothing is installed).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "reaches YouTube in place through the brokered jail"]
    fn live_youtube_search_runs_in_place_when_yt_dlp_is_installed() {
        let package = skillshub("youtube-search");
        let skills = package.parent().unwrap().to_path_buf();
        let mut path = ["node_modules/.bin", ".venv/bin", ".node/bin"]
            .iter()
            .map(|relative| skills.join(relative))
            .collect::<Vec<_>>();
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        if !path
            .iter()
            .any(|directory| directory.join("yt-dlp").is_file())
        {
            eprintln!(
                "skipped: yt-dlp is not installed on this host's runtime PATH; \
                 youtube-search cannot run (nothing was installed)"
            );
            return;
        }
        if !skillshub_runtimes_installed(
            include_bytes!("../../../../skillshub/youtube-search/SKILL.md"),
            &package,
            "run",
        ) {
            return;
        }
        let adapter = AppOsJailPhysicalOwnerAdapter::from_reviewed_source(include_bytes!(
            "../../../../skillshub/youtube-search/SKILL.md"
        ))
        .unwrap();
        let derived = adapter.derive_in_place(&package, "youtube-search").unwrap();
        eprintln!(
            "youtube roots: {:?} path: {:?}",
            derived
                .roots()
                .iter()
                .map(|root| (&root.path, &root.excluded))
                .collect::<Vec<_>>(),
            derived.search_path()
        );
        let execution = match run_in_place(
            include_bytes!("../../../../skillshub/youtube-search/SKILL.md"),
            &package,
            "run",
            serde_json::json!({"query": "rust programming", "limit": 2}),
            None,
            Some(youtube_grant()),
            None,
        ) {
            Ok(execution) => execution,
            Err(failure) => panic!("jailed call failed: {}", failure.error),
        };
        let receipt = execution.egress.clone().expect("egress receipt");
        eprintln!(
            "youtube receipt: destination={} contacted={:?} refusals={:?} stdout={} stderr={}",
            receipt.destination,
            receipt.contacted,
            receipt.refusals,
            execution.stdout.value,
            execution.stderr.value
        );
        assert_eq!(
            execution.exit_code,
            Some(0),
            "stdout={} stderr={}",
            execution.stdout.value,
            execution.stderr.value
        );
        assert!(!execution.stdout.value.to_string().is_empty());
        // The declared host is the only one reached; no Node runtime is on
        // the jail's PATH, and the search needs none.
        assert!(receipt.refusals.is_empty(), "{:?}", receipt.refusals);
        assert!(
            receipt
                .contacted
                .iter()
                .all(|usage| usage.host == "www.youtube.com"),
            "{:?}",
            receipt.contacted
        );
    }

    assert_not_impl_any!(AppOsJailPhysicalOwnerAdapter: Clone, serde::Serialize);
    assert_not_impl_any!(AppOsJailArtifactStore: serde::Serialize);
    assert_not_impl_any!(AppOsJailAuditEvidence: Clone, serde::Serialize);
    assert_not_impl_any!(AppOsJailVerifiedArtifact: Clone, serde::Serialize);
    assert_not_impl_any!(AppOsJailPreparedAction: Clone, serde::Serialize);
    assert_not_impl_any!(AppOsJailObservedResult: Clone, serde::Serialize);
    assert_not_impl_any!(AppOsJailExecutionFailure: Clone, serde::Serialize);
    assert_not_impl_any!(AppOsJailBlockingSlot: Clone, serde::Serialize);
    assert_not_impl_any!(AppOsJailBlockingInFlight: Clone, serde::Serialize);
    assert_not_impl_any!(AppOsJailObservedEffect<()>: Clone, serde::Serialize);
    assert_not_impl_any!(AppOsJailTypedOutput: Clone, serde::Serialize);
}
