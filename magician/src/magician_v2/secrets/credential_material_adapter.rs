//! Scoped adapters from Magician's existing secret/grant authority into the sealed
//! `tool-runtime-core` credential-preparation boundary.
//!
//! Static-secret governed runtimes use this boundary in production. Delegated
//! credential construction remains reserved for the verified executor's exact
//! agent/provider admission path.

#![allow(
    dead_code,
    reason = "delegated credential constructors remain reserved for a later migrated route"
)]

use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use tool_runtime_core::{
    credential_preparation::{
        CredentialMaterialBindingName, CredentialMaterialKind, CredentialMaterialResolver,
        CredentialMaterialSink, CredentialPreparationError, CredentialPreparationPlan,
        CredentialResolutionFailure, CredentialSecretReference,
    },
    credential_profiles::{CredentialProviderId, CredentialScope},
    manifest::{AuthKind, AuthRequirement},
    manifest_validation::ValidatedSkillRuntimeContract,
};
use zeroize::{Zeroize, Zeroizing};

use crate::magician_v2::agents::storage::validate_agent_identifier;
use crate::magician_v2::execution::verified_executor::types::DelegatedCredentialAdmission;

use super::policy::{
    BoundGrantRedemption, BoundGrantRedemptionError, DelegatedGrantAuthority,
    GrantBindingExpectation, RedemptionPayload, RequestedDomains,
};
use super::{SecretAuditEvent, SecretRef, SecretStore, SecretStoreError, SecretStoreResolver};

pub const DELEGATED_CREDENTIAL_BINDING: &str = "delegated_credential";
const ADAPTER_GRANT_TTL_SECS: i64 = 30;
const MAX_ROUTE_IDENTIFIER_BYTES: usize = 256;
const MAX_ROUTE_DOMAIN_BYTES: usize = 2 * 1024;
const MAX_AGENT_ID_BYTES: usize = 256;
const MAX_DELEGATED_TOKEN_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialMaterialAdapterErrorCode {
    InvalidRoute,
    InvalidAgent,
    InvalidSecretReference,
    InvalidSecretEnvironment,
    ContractMismatch,
    PlanMismatch,
}

/// Stable value-free construction error for the authority adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialMaterialAdapterError {
    pub code: CredentialMaterialAdapterErrorCode,
    pub field: &'static str,
    pub message: &'static str,
}

impl CredentialMaterialAdapterError {
    const fn new(
        code: CredentialMaterialAdapterErrorCode,
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

impl fmt::Display for CredentialMaterialAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for CredentialMaterialAdapterError {}

/// Runtime-owned route to which a secret or grant was authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialGrantRoute {
    tool: String,
    action: String,
    /// The domains the action may reach: none, the legacy single domain, a
    /// set, or every site. Grants are issued and redeemed with exactly this
    /// scope.
    domains: RequestedDomains,
}

impl CredentialGrantRoute {
    pub fn new(
        tool: impl Into<String>,
        action: impl Into<String>,
        domain: Option<String>,
    ) -> Result<Self, CredentialMaterialAdapterError> {
        let tool = tool.into();
        let action = action.into();
        if !is_route_identifier(&tool) || !is_route_identifier(&action) {
            return Err(invalid_route());
        }
        if domain.as_deref().is_some_and(|value| {
            value.is_empty()
                || value.len() > MAX_ROUTE_DOMAIN_BYTES
                || !value.is_ascii()
                || value.chars().any(char::is_control)
                || value.chars().any(char::is_whitespace)
        }) {
            return Err(invalid_route());
        }
        Ok(Self {
            tool,
            action,
            domains: RequestedDomains::from_legacy(domain.as_deref()),
        })
    }

    /// A route whose action may reach a domain set or every site
    /// (`RequestedDomains::hosts(..)` / `RequestedDomains::Any`). The vault
    /// allows a set only when each host matches the secret's allowed domains,
    /// and `Any` only when the secret is unrestricted or allows `*`. It does
    /// no public/private filtering: the caller's egress broker must.
    pub fn new_scoped(
        tool: impl Into<String>,
        action: impl Into<String>,
        domains: RequestedDomains,
    ) -> Result<Self, CredentialMaterialAdapterError> {
        let mut route = Self::new(tool, action, None)?;
        route.domains = domains;
        Ok(route)
    }
}

struct SecretReferenceAuthority {
    binding: CredentialMaterialBindingName,
    secret_ref: CredentialSecretReference,
    required: bool,
}

/// Module-private, non-copyable, non-debuggable delegated grant. The only production
/// construction path moves it directly from verified admission into the sealed adapter.
struct DelegatedCredentialToken {
    token: Zeroizing<String>,
    store: Arc<SecretStore>,
}

enum CredentialAuthoritySource {
    SecretReferences {
        references: Vec<SecretReferenceAuthority>,
        legacy_environment: Option<ScopedSecretEnvironmentAuthority>,
    },
    Delegated {
        binding: CredentialMaterialBindingName,
        token: Zeroizing<String>,
        store: Arc<SecretStore>,
        authority: DelegatedGrantAuthority,
    },
}

const MAX_SCOPED_SECRET_ENV_BYTES: u64 = 1024 * 1024;

/// Runtime-owned transitional authority over one scope-local skill `.env`.
/// The file is never model-addressable and is revalidated immediately around
/// its bounded read. Canonical provisioned vault entries always win.
struct ScopedSecretEnvironmentAuthority {
    path: PathBuf,
    identity: ScopedSecretEnvironmentIdentity,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ScopedSecretEnvironmentIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    owner: u32,
    #[cfg(unix)]
    change_time_secs: i64,
    #[cfg(unix)]
    change_time_nanos: i64,
    len: u64,
}

impl ScopedSecretEnvironmentAuthority {
    fn open(path: &Path) -> Result<Self, CredentialMaterialAdapterError> {
        if !path.is_absolute() {
            return Err(invalid_secret_environment());
        }
        // Resolve first, then judge the resolved target. A canonical path's final
        // component cannot be a symlink by definition, so the anti-aliasing
        // property is established BY RESOLUTION rather than by demanding the
        // caller already spell the path canonically. That demand is what the
        // installed scope layout structurally cannot satisfy: it exposes each
        // skill's `config/` as a symlink into a sibling `.skill-state` tree so
        // credentials survive the installer's atomic package swap, which means a
        // scope-installed `.env` is never reachable by a canonical spelling.
        //
        // Nothing else is relaxed. Regular-file, size, mode, and ownership are
        // all judged on the resolved file, and the identity pinned here is the
        // canonical target's — re-verified before, during, and after every read.
        // An attacker able to plant a symlink inside the scope directory could
        // equally write the `.env` itself, so refusing the link never bought a
        // capability the mode and ownership checks do not already cover.
        let canonical = fs::canonicalize(path).map_err(|_| invalid_secret_environment())?;
        let metadata =
            fs::symlink_metadata(&canonical).map_err(|_| invalid_secret_environment())?;
        let identity = scoped_secret_environment_identity(&metadata)?;
        Ok(Self {
            path: canonical,
            identity,
        })
    }

    fn read_declared(
        &self,
        references: &[&SecretReferenceAuthority],
    ) -> Result<BTreeMap<String, Zeroizing<String>>, CredentialPreparationError> {
        let before = fs::symlink_metadata(&self.path).map_err(|_| {
            CredentialPreparationError::resolution(CredentialResolutionFailure::Unavailable)
        })?;
        if scoped_secret_environment_identity(&before).map_err(map_adapter_error)? != self.identity
        {
            return Err(CredentialPreparationError::resolution(
                CredentialResolutionFailure::Unavailable,
            ));
        }
        let file = fs::File::open(&self.path).map_err(|_| {
            CredentialPreparationError::resolution(CredentialResolutionFailure::Unavailable)
        })?;
        if scoped_secret_environment_identity(&file.metadata().map_err(|_| {
            CredentialPreparationError::resolution(CredentialResolutionFailure::Unavailable)
        })?)
        .map_err(map_adapter_error)?
            != self.identity
        {
            return Err(CredentialPreparationError::resolution(
                CredentialResolutionFailure::Unavailable,
            ));
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(
            usize::try_from(self.identity.len).unwrap_or_default(),
        ));
        (&file)
            .take(MAX_SCOPED_SECRET_ENV_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| {
                CredentialPreparationError::resolution(CredentialResolutionFailure::Unavailable)
            })?;
        if bytes.len() as u64 > MAX_SCOPED_SECRET_ENV_BYTES {
            return Err(CredentialPreparationError::resolution(
                CredentialResolutionFailure::Unavailable,
            ));
        }
        if scoped_secret_environment_identity(&file.metadata().map_err(|_| {
            CredentialPreparationError::resolution(CredentialResolutionFailure::Unavailable)
        })?)
        .map_err(map_adapter_error)?
            != self.identity
        {
            return Err(CredentialPreparationError::resolution(
                CredentialResolutionFailure::Unavailable,
            ));
        }
        let mut available = BTreeMap::<String, Zeroizing<String>>::new();
        let entries = dotenvy::from_read_iter(Cursor::new(bytes.as_slice()));
        for entry in entries {
            let (name, value) = entry.map_err(|_| {
                CredentialPreparationError::resolution(CredentialResolutionFailure::Unavailable)
            })?;
            if references
                .iter()
                .any(|reference| reference.secret_ref.as_str() == name)
            {
                available.insert(name, Zeroizing::new(value));
            }
        }
        let after = fs::symlink_metadata(&self.path).map_err(|_| {
            CredentialPreparationError::resolution(CredentialResolutionFailure::Unavailable)
        })?;
        if scoped_secret_environment_identity(&after).map_err(map_adapter_error)? != self.identity {
            return Err(CredentialPreparationError::resolution(
                CredentialResolutionFailure::Unavailable,
            ));
        }
        if references.iter().any(|reference| {
            reference.required
                && available
                    .get(reference.secret_ref.as_str())
                    .is_none_or(|value| value.is_empty())
        }) {
            return Err(CredentialPreparationError::resolution(
                CredentialResolutionFailure::Missing,
            ));
        }
        Ok(available)
    }
}

/// One-shot resolver bound to a validated contract, exact preparation plan, scope, and
/// runtime route. It deliberately has no `Clone`, `Debug`, or serialization surface.
pub struct ScopedCredentialMaterialAdapter {
    store_resolver: Arc<SecretStoreResolver>,
    expected_plan: CredentialPreparationPlan,
    route: CredentialGrantRoute,
    source: CredentialAuthoritySource,
    attempted: bool,
}

impl Drop for ScopedCredentialMaterialAdapter {
    fn drop(&mut self) {
        let CredentialAuthoritySource::Delegated { token, store, .. } = &mut self.source else {
            return;
        };
        if token.is_empty() {
            return;
        }
        store.discard_grants(&[token.as_str()]);
        token.zeroize();
    }
}

impl ScopedCredentialMaterialAdapter {
    pub fn for_secret_references(
        store_resolver: Arc<SecretStoreResolver>,
        validated: ValidatedSkillRuntimeContract<'_>,
        plan: &CredentialPreparationPlan,
        route: CredentialGrantRoute,
    ) -> Result<Self, CredentialMaterialAdapterError> {
        Self::for_secret_references_with_legacy_environment(
            store_resolver,
            validated,
            plan,
            route,
            None,
        )
    }

    pub fn for_secret_references_with_legacy_environment(
        store_resolver: Arc<SecretStoreResolver>,
        validated: ValidatedSkillRuntimeContract<'_>,
        plan: &CredentialPreparationPlan,
        route: CredentialGrantRoute,
        legacy_environment: Option<&Path>,
    ) -> Result<Self, CredentialMaterialAdapterError> {
        let contract = validated.contract();
        if !matches!(
            contract.auth.kind,
            AuthKind::Secrets | AuthKind::CliProfile | AuthKind::OAuthSession
        ) || !plan.matches_validated_contract(validated)
        {
            return Err(contract_mismatch());
        }

        let mut plan_bindings = BTreeMap::new();
        for binding in plan.bindings() {
            plan_bindings.insert(
                binding.name().as_str(),
                (binding.kind(), binding.is_required()),
            );
        }
        if plan_bindings.len() != contract.auth.secret_bindings.len() {
            return Err(plan_mismatch());
        }

        let mut references = Vec::with_capacity(contract.auth.secret_bindings.len());
        for authored in &contract.auth.secret_bindings {
            let binding = CredentialMaterialBindingName::new(authored.name.clone())
                .map_err(|_| plan_mismatch())?;
            let Some((kind, required)) = plan_bindings.get(binding.as_str()).copied() else {
                return Err(plan_mismatch());
            };
            if kind != CredentialMaterialKind::SecretBinding {
                return Err(plan_mismatch());
            }
            if contract.auth.kind == AuthKind::Secrets
                && required
                    != matches!(
                        contract.auth.requirement,
                        AuthRequirement::Required | AuthRequirement::Conditional
                    )
            {
                return Err(plan_mismatch());
            }
            let secret_ref = CredentialSecretReference::new(authored.secret_ref.clone())
                .map_err(|_| invalid_secret_reference())?;
            references.push(SecretReferenceAuthority {
                binding,
                secret_ref,
                required,
            });
        }
        references.sort_by(|left, right| left.binding.cmp(&right.binding));
        let expected_minimum = match contract.auth.requirement {
            AuthRequirement::Required | AuthRequirement::Conditional => {
                contract.auth.secret_bindings.len()
            },
            AuthRequirement::AtLeastOne => 1,
            AuthRequirement::None | AuthRequirement::Optional => 0,
        };
        if plan.minimum_present() != expected_minimum {
            return Err(plan_mismatch());
        }
        let legacy_environment = legacy_environment
            .map(ScopedSecretEnvironmentAuthority::open)
            .transpose()?;

        Ok(Self {
            store_resolver,
            expected_plan: plan.clone(),
            route,
            source: CredentialAuthoritySource::SecretReferences {
                references,
                legacy_environment,
            },
            attempted: false,
        })
    }

    /// Consume one exact verified-executor admission, issue the existing short-lived
    /// single-use grant, and move it directly into sealed credential preparation.
    /// No token is returned to the caller or represented in model/durable state.
    pub fn for_verified_delegated_admission(
        store_resolver: Arc<SecretStoreResolver>,
        validated: ValidatedSkillRuntimeContract<'_>,
        plan: &CredentialPreparationPlan,
        admission: DelegatedCredentialAdmission,
    ) -> Result<Self, CredentialPreparationError> {
        let (scope, secret_ref, route, provider, agent_id) = admission.into_parts();
        let (binding, authority) =
            validate_delegated_adapter_binding(validated, plan, &scope, &provider, &agent_id)
                .map_err(map_adapter_error)?;
        let DelegatedCredentialToken { token, store } = issue_bound_delegated_credential(
            store_resolver.as_ref(),
            &scope,
            &secret_ref,
            &route,
            &provider,
            &agent_id,
        )?;

        Ok(Self {
            store_resolver,
            expected_plan: plan.clone(),
            route,
            source: CredentialAuthoritySource::Delegated {
                binding,
                token,
                store,
                authority,
            },
            attempted: false,
        })
    }

    /// Test-only/internal adversarial constructor. Production callers must enter via
    /// `for_verified_delegated_admission`, which owns issuance and token transfer.
    #[cfg(any(test, feature = "test-fixtures"))]
    fn for_delegated_credential(
        store_resolver: Arc<SecretStoreResolver>,
        validated: ValidatedSkillRuntimeContract<'_>,
        plan: &CredentialPreparationPlan,
        route: CredentialGrantRoute,
        agent_id: impl Into<String>,
        token: DelegatedCredentialToken,
    ) -> Result<Self, CredentialMaterialAdapterError> {
        let agent_id = agent_id.into();
        let provider = validated
            .contract()
            .auth
            .provider
            .as_deref()
            .and_then(|provider| CredentialProviderId::new(provider).ok())
            .ok_or_else(contract_mismatch)?;
        let (expected_binding, authority) = validate_delegated_adapter_binding(
            validated,
            plan,
            plan.scope(),
            &provider,
            &agent_id,
        )?;

        let DelegatedCredentialToken { token, store } = token;
        Ok(Self {
            store_resolver,
            expected_plan: plan.clone(),
            route,
            source: CredentialAuthoritySource::Delegated {
                binding: expected_binding,
                token,
                store,
                authority,
            },
            attempted: false,
        })
    }
}

fn validate_delegated_adapter_binding(
    validated: ValidatedSkillRuntimeContract<'_>,
    plan: &CredentialPreparationPlan,
    expected_scope: &CredentialScope,
    expected_provider: &CredentialProviderId,
    agent_id: &str,
) -> Result<(CredentialMaterialBindingName, DelegatedGrantAuthority), CredentialMaterialAdapterError>
{
    let contract = validated.contract();
    if contract.auth.kind != AuthKind::DelegatedCredential
        || !plan.matches_validated_contract(validated)
        || plan.scope() != expected_scope
    {
        return Err(contract_mismatch());
    }
    let contract_provider = contract
        .auth
        .provider
        .as_deref()
        .and_then(|provider| CredentialProviderId::new(provider).ok())
        .ok_or_else(contract_mismatch)?;
    if &contract_provider != expected_provider {
        return Err(contract_mismatch());
    }
    let expected_binding = CredentialMaterialBindingName::new(DELEGATED_CREDENTIAL_BINDING)
        .map_err(|_| plan_mismatch())?;
    if plan.bindings().len() != 1
        || plan.bindings()[0].name() != &expected_binding
        || plan.bindings()[0].kind() != CredentialMaterialKind::DelegatedCredential
    {
        return Err(plan_mismatch());
    }
    if !is_agent_id(agent_id) {
        return Err(invalid_agent());
    }
    Ok((
        expected_binding,
        delegated_authority(expected_scope, expected_provider, agent_id),
    ))
}

/// Module-private issuance primitive used only by the verified-admission factory and
/// adversarial tests. No production caller can receive its token type.
fn issue_bound_delegated_credential(
    store_resolver: &SecretStoreResolver,
    scope: &CredentialScope,
    secret_ref: &CredentialSecretReference,
    route: &CredentialGrantRoute,
    provider: &CredentialProviderId,
    agent_id: &str,
) -> Result<DelegatedCredentialToken, CredentialPreparationError> {
    if !is_agent_id(agent_id) {
        return Err(CredentialPreparationError::resolution(
            CredentialResolutionFailure::BindingMismatch,
        ));
    }
    let store = store_resolver
        .resolve_for_scope(scope.principal.as_str(), scope.workspace.as_str())
        .map_err(map_store_error)?;
    let authority = delegated_authority(scope, provider, agent_id);
    let token = store
        .issue_delegated_grant_scoped(
            secret_ref.as_str(),
            &route.tool,
            &route.action,
            &route.domains,
            authority,
            Some(ADAPTER_GRANT_TTL_SECS),
        )
        .map_err(map_store_error)?;
    if token.is_empty()
        || token.len() > MAX_DELEGATED_TOKEN_BYTES
        || token.chars().any(char::is_control)
    {
        store.discard_grants(&[token.as_str()]);
        return Err(CredentialPreparationError::resolution(
            CredentialResolutionFailure::Internal,
        ));
    }
    Ok(DelegatedCredentialToken {
        token: Zeroizing::new(token),
        store,
    })
}

impl CredentialMaterialResolver for ScopedCredentialMaterialAdapter {
    fn resolve_once(
        &mut self,
        plan: &CredentialPreparationPlan,
        sink: &mut CredentialMaterialSink<'_>,
    ) -> Result<(), CredentialPreparationError> {
        if self.attempted {
            return Err(CredentialPreparationError::resolution(
                CredentialResolutionFailure::AlreadyRedeemed,
            ));
        }
        self.attempted = true;
        if plan != &self.expected_plan {
            return Err(CredentialPreparationError::resolution(
                CredentialResolutionFailure::BindingMismatch,
            ));
        }
        if matches!(
            &self.source,
            CredentialAuthoritySource::SecretReferences { references, .. } if references.is_empty()
        ) {
            return Ok(());
        }

        match &mut self.source {
            CredentialAuthoritySource::SecretReferences {
                references,
                legacy_environment,
            } => {
                let store = self
                    .store_resolver
                    .resolve_for_scope(
                        plan.scope().principal.as_str(),
                        plan.scope().workspace.as_str(),
                    )
                    .map_err(map_store_error)?;
                resolve_secret_references(
                    &store,
                    &self.route,
                    references,
                    legacy_environment.as_ref(),
                    sink,
                )
            },
            CredentialAuthoritySource::Delegated {
                binding,
                token,
                store,
                authority,
            } => {
                let redemption = {
                    let request = BoundGrantRedemption {
                        token: token.as_str(),
                        expected: GrantBindingExpectation::DelegatedScoped {
                            tool: &self.route.tool,
                            action: &self.route.action,
                            domains: &self.route.domains,
                            authority,
                        },
                    };
                    store.redeem_bound_grants(std::slice::from_ref(&request))
                };
                if redemption.is_err() {
                    store.discard_grants(&[token.as_str()]);
                }
                token.zeroize();
                let mut payloads = redemption.map_err(map_redemption_error)?;
                let Some(payload) = payloads.pop() else {
                    return Err(CredentialPreparationError::resolution(
                        CredentialResolutionFailure::Internal,
                    ));
                };
                provide_canonical_value(binding, &payload, sink)?;
                store
                    .record_usage(payload.secret_id(), None)
                    .map_err(map_store_error)
            },
        }
    }
}

fn resolve_secret_references(
    store: &SecretStore,
    route: &CredentialGrantRoute,
    references: &[SecretReferenceAuthority],
    legacy_environment: Option<&ScopedSecretEnvironmentAuthority>,
    sink: &mut CredentialMaterialSink<'_>,
) -> Result<(), CredentialPreparationError> {
    if references.is_empty() {
        return Ok(());
    }
    let (canonical, legacy): (Vec<_>, Vec<_>) = references
        .iter()
        .partition(|reference| store.contains_provisioned(reference.secret_ref.as_str()));
    let legacy_values = if legacy.is_empty() {
        BTreeMap::new()
    } else if let Some(environment) = legacy_environment {
        environment.read_declared(&legacy)?
    } else if legacy.iter().any(|reference| reference.required) {
        return Err(CredentialPreparationError::resolution(
            CredentialResolutionFailure::Missing,
        ));
    } else {
        BTreeMap::new()
    };
    resolve_canonical_secret_references(store, route, &canonical, sink)?;
    for reference in legacy {
        match legacy_values.get(reference.secret_ref.as_str()) {
            Some(value) => sink.provide(&reference.binding, value.as_bytes().to_vec())?,
            None if !reference.required => {},
            None => {
                return Err(CredentialPreparationError::resolution(
                    CredentialResolutionFailure::Missing,
                ));
            },
        }
    }
    if !legacy_values.is_empty() {
        store
            .try_audit_event(
                SecretAuditEvent::new("governed_legacy_secret_environment")
                    .with_tool(&route.tool)
                    .with_action(&route.action),
            )
            .map_err(map_store_error)?;
    }
    Ok(())
}

fn resolve_canonical_secret_references(
    store: &SecretStore,
    route: &CredentialGrantRoute,
    references: &[&SecretReferenceAuthority],
    sink: &mut CredentialMaterialSink<'_>,
) -> Result<(), CredentialPreparationError> {
    if references.is_empty() {
        return Ok(());
    }
    let mut grouped =
        BTreeMap::<&CredentialSecretReference, Vec<&CredentialMaterialBindingName>>::new();
    for reference in references {
        grouped
            .entry(&reference.secret_ref)
            .or_default()
            .push(&reference.binding);
    }
    let groups = grouped.into_iter().collect::<Vec<_>>();

    let mut tokens = Vec::<Zeroizing<String>>::with_capacity(groups.len());
    for (secret_ref, _) in &groups {
        match store.issue_grant_scoped(
            secret_ref.as_str(),
            &route.tool,
            &route.action,
            &route.domains,
            Some(ADAPTER_GRANT_TTL_SECS),
        ) {
            Ok(SecretRef::Grant(token)) => tokens.push(Zeroizing::new(token)),
            Ok(_) => {
                discard_tokens(store, &tokens);
                return Err(CredentialPreparationError::resolution(
                    CredentialResolutionFailure::Internal,
                ));
            },
            Err(error) => {
                discard_tokens(store, &tokens);
                return Err(map_store_error(error));
            },
        }
    }

    let requests = tokens
        .iter()
        .map(|token| BoundGrantRedemption {
            token: token.as_str(),
            expected: GrantBindingExpectation::SecretScoped {
                tool: &route.tool,
                action: &route.action,
                domains: &route.domains,
            },
        })
        .collect::<Vec<_>>();
    let payloads = match store.redeem_bound_grants(&requests) {
        Ok(payloads) => payloads,
        Err(error) => {
            discard_tokens(store, &tokens);
            return Err(map_redemption_error(error));
        },
    };
    if payloads.len() != groups.len() {
        return Err(CredentialPreparationError::resolution(
            CredentialResolutionFailure::Internal,
        ));
    }
    for ((secret_ref, _), payload) in groups.iter().zip(&payloads) {
        if payload.secret_id() != secret_ref.as_str() {
            return Err(CredentialPreparationError::resolution(
                CredentialResolutionFailure::BindingMismatch,
            ));
        }
        if payload.with_canonical_value(|_| ()).is_none() {
            return Err(CredentialPreparationError::resolution(
                CredentialResolutionFailure::BindingMismatch,
            ));
        }
    }
    for ((_, bindings), payload) in groups.iter().zip(&payloads) {
        for binding in bindings {
            provide_canonical_value(binding, payload, sink)?;
        }
    }
    for ((secret_ref, _), _) in groups.into_iter().zip(payloads) {
        store
            .record_usage(secret_ref.as_str(), None)
            .map_err(map_store_error)?;
    }
    Ok(())
}

fn provide_canonical_value(
    binding: &CredentialMaterialBindingName,
    payload: &RedemptionPayload,
    sink: &mut CredentialMaterialSink<'_>,
) -> Result<(), CredentialPreparationError> {
    payload
        .with_canonical_value(|value| sink.provide(binding, value.as_bytes().to_vec()))
        .ok_or_else(|| {
            CredentialPreparationError::resolution(CredentialResolutionFailure::BindingMismatch)
        })?
}

fn discard_tokens(store: &SecretStore, tokens: &[Zeroizing<String>]) {
    let token_refs = tokens
        .iter()
        .map(|token| token.as_str())
        .collect::<Vec<_>>();
    store.discard_grants(&token_refs);
}

fn delegated_authority(
    scope: &CredentialScope,
    provider: &CredentialProviderId,
    agent_id: &str,
) -> DelegatedGrantAuthority {
    DelegatedGrantAuthority::new(
        scope.principal.as_str(),
        scope.workspace.as_str(),
        provider.as_str(),
        agent_id,
    )
}

fn map_store_error(error: SecretStoreError) -> CredentialPreparationError {
    let failure = match error {
        SecretStoreError::SecretNotFound(_) => CredentialResolutionFailure::Missing,
        SecretStoreError::GrantNotFound(_) => CredentialResolutionFailure::Stale,
        SecretStoreError::PolicyDenied(_) | SecretStoreError::ApprovalRequired(_) => {
            CredentialResolutionFailure::Denied
        },
        SecretStoreError::FeatureDisabled { .. } => CredentialResolutionFailure::Unavailable,
        SecretStoreError::InlineProvisionedSecret => CredentialResolutionFailure::BindingMismatch,
        SecretStoreError::McpOAuthInvalidRecord
        | SecretStoreError::McpOAuthConflict
        | SecretStoreError::McpOAuthCapacity
        | SecretStoreError::McpOAuthUnavailable
        | SecretStoreError::McpOAuthCommitStateUnknown => CredentialResolutionFailure::Internal,
        SecretStoreError::Encryption(_)
        | SecretStoreError::Io(_)
        | SecretStoreError::Serde(_)
        | SecretStoreError::AuditUnavailable => CredentialResolutionFailure::Unavailable,
        // Only bounded ephemeral registration raises this; provisioned
        // resolution never does, and a passed deadline is not a policy.
        SecretStoreError::DeadlinePassed => CredentialResolutionFailure::Unavailable,
    };
    CredentialPreparationError::resolution(failure)
}

fn map_adapter_error(error: CredentialMaterialAdapterError) -> CredentialPreparationError {
    let failure = match error.code {
        CredentialMaterialAdapterErrorCode::InvalidAgent
        | CredentialMaterialAdapterErrorCode::ContractMismatch
        | CredentialMaterialAdapterErrorCode::PlanMismatch => {
            CredentialResolutionFailure::BindingMismatch
        },
        CredentialMaterialAdapterErrorCode::InvalidRoute
        | CredentialMaterialAdapterErrorCode::InvalidSecretReference
        | CredentialMaterialAdapterErrorCode::InvalidSecretEnvironment => {
            CredentialResolutionFailure::Internal
        },
    };
    CredentialPreparationError::resolution(failure)
}

fn map_redemption_error(error: BoundGrantRedemptionError) -> CredentialPreparationError {
    let failure = match error {
        BoundGrantRedemptionError::Expired => CredentialResolutionFailure::Expired,
        BoundGrantRedemptionError::MissingOrAlreadyRedeemed => CredentialResolutionFailure::Stale,
        BoundGrantRedemptionError::BindingMismatch | BoundGrantRedemptionError::DuplicateToken => {
            CredentialResolutionFailure::BindingMismatch
        },
        BoundGrantRedemptionError::EmptyBatch
        | BoundGrantRedemptionError::BatchTooLarge
        | BoundGrantRedemptionError::Internal => CredentialResolutionFailure::Internal,
    };
    CredentialPreparationError::resolution(failure)
}

fn is_route_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ROUTE_IDENTIFIER_BYTES
        && !matches!(value, "." | "..")
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+' | b':')
        })
}

fn is_agent_id(value: &str) -> bool {
    value.len() <= MAX_AGENT_ID_BYTES && validate_agent_identifier(value).is_ok()
}

const fn invalid_route() -> CredentialMaterialAdapterError {
    CredentialMaterialAdapterError::new(
        CredentialMaterialAdapterErrorCode::InvalidRoute,
        "route",
        "the credential route must contain bounded runtime-owned identifiers",
    )
}

const fn invalid_agent() -> CredentialMaterialAdapterError {
    CredentialMaterialAdapterError::new(
        CredentialMaterialAdapterErrorCode::InvalidAgent,
        "agent_id",
        "the delegated credential agent identity is invalid",
    )
}

const fn invalid_secret_reference() -> CredentialMaterialAdapterError {
    CredentialMaterialAdapterError::new(
        CredentialMaterialAdapterErrorCode::InvalidSecretReference,
        "secret_ref",
        "the scoped secret reference is invalid",
    )
}

const fn invalid_secret_environment() -> CredentialMaterialAdapterError {
    CredentialMaterialAdapterError::new(
        CredentialMaterialAdapterErrorCode::InvalidSecretEnvironment,
        "secret_environment",
        "the legacy secret environment must be one bounded private regular file",
    )
}

#[cfg(unix)]
fn scoped_secret_environment_identity(
    metadata: &fs::Metadata,
) -> Result<ScopedSecretEnvironmentIdentity, CredentialMaterialAdapterError> {
    let mode = metadata.mode();
    if !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_SCOPED_SECRET_ENV_BYTES
        || mode & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(invalid_secret_environment());
    }
    Ok(ScopedSecretEnvironmentIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode,
        owner: metadata.uid(),
        change_time_secs: metadata.ctime(),
        change_time_nanos: metadata.ctime_nsec(),
        len: metadata.len(),
    })
}

#[cfg(not(unix))]
fn scoped_secret_environment_identity(
    _metadata: &fs::Metadata,
) -> Result<ScopedSecretEnvironmentIdentity, CredentialMaterialAdapterError> {
    Err(invalid_secret_environment())
}

const fn contract_mismatch() -> CredentialMaterialAdapterError {
    CredentialMaterialAdapterError::new(
        CredentialMaterialAdapterErrorCode::ContractMismatch,
        "runtime_contract",
        "the validated authentication contract is incompatible with this adapter",
    )
}

const fn plan_mismatch() -> CredentialMaterialAdapterError {
    CredentialMaterialAdapterError::new(
        CredentialMaterialAdapterErrorCode::PlanMismatch,
        "credential_plan",
        "the credential preparation plan does not exactly match the validated contract",
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{
        collections::{BTreeSet, HashMap},
        fmt,
        sync::{Arc, Barrier},
        thread,
    };

    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};

    use static_assertions::{assert_impl_all, assert_not_impl_any};
    use tempfile::TempDir;
    use tool_runtime_core::{
        credential_preparation::{with_prepared_credential_material, CredentialPreparationBinding},
        credential_profiles::{
            CreateCredentialProfileReference, CredentialProfileBinding, CredentialProfileError,
            CredentialProfileKey, CredentialProfileRegistry, CredentialProfileRegistrySnapshot,
            CredentialProfileStatus, SetCredentialProfileDisabled, UpdateCredentialProfileMetadata,
        },
        manifest::{
            AuthContract, AuthRequirement, AuthStorage, CliInteraction, InjectionBinding,
            InjectionSource, InjectionTarget as ManifestInjectionTarget, PolicyFloor,
            ProfileSelection, RuntimeLimits, RuntimeProtocol, RuntimeRequirements,
            SecretBindingRef, SkillRuntimeContract, SkillRuntimeContractVersion, StdinContract,
            WorkingDirectoryContract,
        },
        manifest_validation::validate_skill_runtime_contract,
        profile_selection::{select_credential_profile, CredentialProfileSelectionRequest},
    };

    use crate::magician_v2::secrets::{
        InMemoryKeyProvider, InjectionTarget as SecretInjectionTarget, PolicyResult, SecretPolicy,
        SecretRuntimeCapabilities,
    };

    use super::*;

    struct EmptyRegistry;

    impl CredentialProfileRegistry for EmptyRegistry {
        fn snapshot(
            &self,
            scope: &CredentialScope,
        ) -> Result<CredentialProfileRegistrySnapshot, CredentialProfileError> {
            CredentialProfileRegistrySnapshot::new(scope.clone(), Vec::new())
        }

        fn status(
            &self,
            _key: &CredentialProfileKey,
        ) -> Result<Option<CredentialProfileStatus>, CredentialProfileError> {
            Ok(None)
        }

        fn create_reference(
            &self,
            _request: CreateCredentialProfileReference,
        ) -> Result<CredentialProfileStatus, CredentialProfileError> {
            panic!("read-only test registry")
        }

        fn update_metadata(
            &self,
            _request: UpdateCredentialProfileMetadata,
        ) -> Result<CredentialProfileStatus, CredentialProfileError> {
            panic!("read-only test registry")
        }

        fn set_disabled(
            &self,
            _request: SetCredentialProfileDisabled,
        ) -> Result<CredentialProfileStatus, CredentialProfileError> {
            panic!("read-only test registry")
        }
    }

    fn scope(principal: &str) -> CredentialScope {
        CredentialScope::new(principal, "default").expect("valid scope")
    }

    fn none_selection(
        selected_scope: &CredentialScope,
    ) -> tool_runtime_core::profile_selection::CredentialProfileSelectionDecision {
        let request = CredentialProfileSelectionRequest::new(
            selected_scope.clone(),
            None,
            CredentialProfileBinding::Provider,
            &ProfileSelection::None,
            None,
        )
        .expect("selection request");
        select_credential_profile(&EmptyRegistry, &request).expect("none selection")
    }

    fn implicit_selection(
        selected_scope: &CredentialScope,
        provider: &str,
    ) -> tool_runtime_core::profile_selection::CredentialProfileSelectionDecision {
        let request = CredentialProfileSelectionRequest::new(
            selected_scope.clone(),
            Some(provider),
            CredentialProfileBinding::Provider,
            &ProfileSelection::Implicit,
            None,
        )
        .expect("implicit selection request");
        select_credential_profile(&EmptyRegistry, &request).expect("implicit selection")
    }

    fn preparation_plan(
        selected_scope: &CredentialScope,
        auth_kind: AuthKind,
        binding_name: &str,
        material_kind: CredentialMaterialKind,
    ) -> CredentialPreparationPlan {
        CredentialPreparationPlan::new(
            selected_scope.clone(),
            auth_kind,
            &none_selection(selected_scope),
            vec![CredentialPreparationBinding::new(
                CredentialMaterialBindingName::new(binding_name).expect("binding name"),
                material_kind,
                1024,
            )
            .expect("preparation binding")],
        )
        .expect("preparation plan")
    }

    fn cli_contract(auth: AuthContract) -> SkillRuntimeContract {
        SkillRuntimeContract {
            schema_version: SkillRuntimeContractVersion::v1(),
            requires: RuntimeRequirements {
                bins: BTreeSet::from(["fixture-cli".to_owned()]),
                entrypoint: Default::default(),
                environment: Default::default(),
            },
            runtime: RuntimeProtocol::Cli {
                command_prefix: Vec::new(),
                interaction: CliInteraction::Batch,
                stdin: StdinContract::default(),
                working_directory: WorkingDirectoryContract::default(),
                limits: RuntimeLimits::default(),
            },
            auth,
            policy_floor: PolicyFloor::default(),
        }
    }

    fn secret_contract(binding_name: &str, secret_ref: &str) -> SkillRuntimeContract {
        cli_contract(AuthContract {
            kind: AuthKind::Secrets,
            requirement: AuthRequirement::Required,
            secret_bindings: vec![SecretBindingRef {
                name: binding_name.to_owned(),
                secret_ref: secret_ref.to_owned(),
            }],
            injections: vec![InjectionBinding {
                source: InjectionSource::Secret {
                    binding: binding_name.to_owned(),
                },
                target: ManifestInjectionTarget::Environment {
                    name: "FIXTURE_API_KEY".to_owned(),
                },
            }],
            ..AuthContract::default()
        })
    }

    fn delegated_contract(provider: &str) -> SkillRuntimeContract {
        cli_contract(AuthContract {
            kind: AuthKind::DelegatedCredential,
            requirement: AuthRequirement::Required,
            provider: Some(provider.to_owned()),
            storage: AuthStorage::EphemeralGrant,
            ..AuthContract::default()
        })
    }

    fn test_resolver() -> (Arc<SecretStoreResolver>, TempDir) {
        let root = TempDir::new().expect("temporary secret root");
        let resolver = SecretStoreResolver::new_with_capabilities(
            Box::new(InMemoryKeyProvider::new()),
            root.path().to_path_buf(),
            SecretRuntimeCapabilities::fully_available("in_memory"),
        );
        (Arc::new(resolver), root)
    }

    fn provision(
        resolver: &SecretStoreResolver,
        selected_scope: &CredentialScope,
        secret_ref: &str,
        fields: HashMap<String, String>,
    ) {
        provision_with_policy(
            resolver,
            selected_scope,
            secret_ref,
            fields,
            SecretPolicy::default(),
        );
    }

    fn provision_with_policy(
        resolver: &SecretStoreResolver,
        selected_scope: &CredentialScope,
        secret_ref: &str,
        fields: HashMap<String, String>,
        policy: SecretPolicy,
    ) {
        resolver
            .resolve_for_scope(
                selected_scope.principal.as_str(),
                selected_scope.workspace.as_str(),
            )
            .expect("scoped store")
            .store_provisioned(
                secret_ref,
                "Fixture credential",
                fields,
                SecretInjectionTarget::Header {
                    name: "Authorization".to_owned(),
                    prefix: Some("Bearer ".to_owned()),
                },
                policy,
            )
            .expect("provision fixture credential");
    }

    fn route() -> CredentialGrantRoute {
        CredentialGrantRoute::new(
            "generic_runtime",
            "execute",
            Some("api.example.com".to_owned()),
        )
        .expect("credential route")
    }

    fn delegated_token(
        resolver: &SecretStoreResolver,
        selected_scope: &CredentialScope,
        raw: impl Into<String>,
    ) -> DelegatedCredentialToken {
        DelegatedCredentialToken {
            token: Zeroizing::new(raw.into()),
            store: resolver
                .resolve_for_scope(
                    selected_scope.principal.as_str(),
                    selected_scope.workspace.as_str(),
                )
                .expect("delegated token store"),
        }
    }

    fn verified_admission(
        selected_scope: CredentialScope,
        secret_ref: CredentialSecretReference,
        selected_route: CredentialGrantRoute,
        provider: CredentialProviderId,
        agent_id: &str,
    ) -> DelegatedCredentialAdmission {
        crate::magician_v2::execution::verified_executor::admit_delegated_credential(
            selected_scope,
            secret_ref,
            selected_route,
            provider,
            agent_id.to_owned(),
        )
    }

    fn run_adapter(
        plan: &CredentialPreparationPlan,
        adapter: &mut ScopedCredentialMaterialAdapter,
    ) -> Result<(usize, Option<CredentialMaterialKind>), CredentialPreparationError> {
        let binding = plan.bindings()[0].name().clone();
        with_prepared_credential_material(plan, adapter, |material| {
            (material.len(), material.kind(&binding))
        })
    }

    #[test]
    fn adapter_and_delegated_token_are_sealed_send_only_boundaries() {
        assert_impl_all!(ScopedCredentialMaterialAdapter: Send);
        assert_not_impl_any!(ScopedCredentialMaterialAdapter: Clone, fmt::Debug, serde::Serialize);
        assert_not_impl_any!(DelegatedCredentialToken: Clone, fmt::Debug, serde::Serialize);
        assert_not_impl_any!(DelegatedCredentialAdmission: Clone, fmt::Debug, serde::Serialize);
    }

    #[test]
    fn verified_admission_issues_directly_into_sealed_preparation() {
        let (resolver, _root) = test_resolver();
        let selected_scope = scope("owner");
        let provider = CredentialProviderId::new("provider-a").expect("provider");
        let secret_ref = CredentialSecretReference::new("DELEGATED_SECRET").expect("secret ref");
        provision(
            &resolver,
            &selected_scope,
            secret_ref.as_str(),
            HashMap::from([("value".to_owned(), "delegated-canary".to_owned())]),
        );
        let contract = delegated_contract(provider.as_str());
        let plan = preparation_plan(
            &selected_scope,
            AuthKind::DelegatedCredential,
            DELEGATED_CREDENTIAL_BINDING,
            CredentialMaterialKind::DelegatedCredential,
        );
        let admission =
            verified_admission(selected_scope, secret_ref, route(), provider, "agent-a");
        let mut adapter = ScopedCredentialMaterialAdapter::for_verified_delegated_admission(
            Arc::clone(&resolver),
            validate_skill_runtime_contract(&contract).expect("delegated contract"),
            &plan,
            admission,
        )
        .expect("sealed delegated adapter");

        assert_eq!(
            run_adapter(&plan, &mut adapter).expect("sealed preparation"),
            (1, Some(CredentialMaterialKind::DelegatedCredential))
        );
        assert_eq!(
            run_adapter(&plan, &mut adapter)
                .expect_err("adapter remains one shot")
                .resolution_failure,
            Some(CredentialResolutionFailure::AlreadyRedeemed)
        );
    }

    #[test]
    fn verified_admission_mismatch_fails_before_grant_issuance() {
        struct Case<'a> {
            scope: &'a str,
            provider: &'a str,
            agent: &'a str,
        }
        let cases = [
            Case {
                scope: "other-owner",
                provider: "provider-a",
                agent: "agent-a",
            },
            Case {
                scope: "owner",
                provider: "provider-b",
                agent: "agent-a",
            },
            Case {
                scope: "owner",
                provider: "provider-a",
                agent: "../CANARY_AGENT",
            },
        ];

        for case in cases {
            let (resolver, _root) = test_resolver();
            let selected_scope = scope("owner");
            let contract = delegated_contract("provider-a");
            let plan = preparation_plan(
                &selected_scope,
                AuthKind::DelegatedCredential,
                DELEGATED_CREDENTIAL_BINDING,
                CredentialMaterialKind::DelegatedCredential,
            );
            let admission = verified_admission(
                scope(case.scope),
                CredentialSecretReference::new("DELEGATED_SECRET").expect("secret ref"),
                route(),
                CredentialProviderId::new(case.provider).expect("provider"),
                case.agent,
            );
            let error = ScopedCredentialMaterialAdapter::for_verified_delegated_admission(
                resolver,
                validate_skill_runtime_contract(&contract).expect("delegated contract"),
                &plan,
                admission,
            )
            .err()
            .expect("binding mismatch");
            assert_eq!(
                error.resolution_failure,
                Some(CredentialResolutionFailure::BindingMismatch)
            );
            assert!(!format!("{error:?} {error}").contains("CANARY_AGENT"));
        }
    }

    #[test]
    fn verified_admission_rejection_is_iterative_on_a_small_stack() {
        thread::Builder::new()
            .name("delegated-admission-small-stack".to_owned())
            .stack_size(128 * 1024)
            .spawn(|| {
                let (resolver, _root) = test_resolver();
                let selected_scope = scope("owner");
                let contract = delegated_contract("provider-a");
                let plan = preparation_plan(
                    &selected_scope,
                    AuthKind::DelegatedCredential,
                    DELEGATED_CREDENTIAL_BINDING,
                    CredentialMaterialKind::DelegatedCredential,
                );
                for _ in 0..10_000 {
                    let admission = verified_admission(
                        selected_scope.clone(),
                        CredentialSecretReference::new("DELEGATED_SECRET").expect("secret ref"),
                        route(),
                        CredentialProviderId::new("provider-b").expect("provider"),
                        "agent-a",
                    );
                    let error = ScopedCredentialMaterialAdapter::for_verified_delegated_admission(
                        Arc::clone(&resolver),
                        validate_skill_runtime_contract(&contract).expect("delegated contract"),
                        &plan,
                        admission,
                    )
                    .err()
                    .expect("provider mismatch");
                    assert_eq!(
                        error.resolution_failure,
                        Some(CredentialResolutionFailure::BindingMismatch)
                    );
                }
            })
            .expect("spawn")
            .join()
            .expect("admission rejection must not panic or overflow");
    }

    #[test]
    fn scoped_secret_reference_prepares_once_without_exposing_the_value() {
        let (resolver, _root) = test_resolver();
        let selected_scope = scope("owner");
        provision(
            &resolver,
            &selected_scope,
            "FIXTURE_API_KEY",
            HashMap::from([("value".to_owned(), "canary-secret-value".to_owned())]),
        );
        let contract = secret_contract("api_key", "FIXTURE_API_KEY");
        let plan = preparation_plan(
            &selected_scope,
            AuthKind::Secrets,
            "api_key",
            CredentialMaterialKind::SecretBinding,
        );
        let mut adapter = ScopedCredentialMaterialAdapter::for_secret_references(
            Arc::clone(&resolver),
            validate_skill_runtime_contract(&contract).expect("valid secret contract"),
            &plan,
            route(),
        )
        .expect("secret adapter");

        assert_eq!(
            run_adapter(&plan, &mut adapter).expect("sealed preparation"),
            (1, Some(CredentialMaterialKind::SecretBinding))
        );
        assert_eq!(
            run_adapter(&plan, &mut adapter)
                .expect_err("adapter is one shot")
                .resolution_failure,
            Some(CredentialResolutionFailure::AlreadyRedeemed)
        );
    }

    #[test]
    fn secret_adapter_requires_exact_contract_plan_and_canonical_value() {
        let (resolver, _root) = test_resolver();
        let selected_scope = scope("owner");
        let contract = secret_contract("api_key", "FIXTURE_API_KEY");
        let wrong_plan = preparation_plan(
            &selected_scope,
            AuthKind::Secrets,
            "other_key",
            CredentialMaterialKind::SecretBinding,
        );
        assert_eq!(
            ScopedCredentialMaterialAdapter::for_secret_references(
                Arc::clone(&resolver),
                validate_skill_runtime_contract(&contract).expect("valid secret contract"),
                &wrong_plan,
                route(),
            )
            .err()
            .expect("plan mismatch")
            .code,
            CredentialMaterialAdapterErrorCode::PlanMismatch
        );

        provision(
            &resolver,
            &selected_scope,
            "FIXTURE_API_KEY",
            HashMap::from([
                ("value".to_owned(), "canary-secret-value".to_owned()),
                ("extra".to_owned(), "must-not-be-guessed".to_owned()),
            ]),
        );
        let plan = preparation_plan(
            &selected_scope,
            AuthKind::Secrets,
            "api_key",
            CredentialMaterialKind::SecretBinding,
        );
        let mut adapter = ScopedCredentialMaterialAdapter::for_secret_references(
            Arc::clone(&resolver),
            validate_skill_runtime_contract(&contract).expect("valid secret contract"),
            &plan,
            route(),
        )
        .expect("secret adapter");
        assert_eq!(
            run_adapter(&plan, &mut adapter)
                .expect_err("ambiguous secret fields fail closed")
                .resolution_failure,
            Some(CredentialResolutionFailure::BindingMismatch)
        );
    }

    #[test]
    fn duplicate_bindings_resolve_and_charge_one_secret_use() {
        let (resolver, _root) = test_resolver();
        let selected_scope = scope("owner");
        provision_with_policy(
            &resolver,
            &selected_scope,
            "SHARED_SECRET",
            HashMap::from([("value".to_owned(), "shared-canary".to_owned())]),
            SecretPolicy {
                max_uses_per_day: Some(2),
                ..SecretPolicy::default()
            },
        );
        let contract = cli_contract(AuthContract {
            kind: AuthKind::Secrets,
            requirement: AuthRequirement::Required,
            secret_bindings: vec![
                SecretBindingRef {
                    name: "first".to_owned(),
                    secret_ref: "SHARED_SECRET".to_owned(),
                },
                SecretBindingRef {
                    name: "second".to_owned(),
                    secret_ref: "SHARED_SECRET".to_owned(),
                },
            ],
            injections: vec![
                InjectionBinding {
                    source: InjectionSource::Secret {
                        binding: "first".to_owned(),
                    },
                    target: ManifestInjectionTarget::Environment {
                        name: "FIRST_SECRET".to_owned(),
                    },
                },
                InjectionBinding {
                    source: InjectionSource::Secret {
                        binding: "second".to_owned(),
                    },
                    target: ManifestInjectionTarget::Environment {
                        name: "SECOND_SECRET".to_owned(),
                    },
                },
            ],
            ..AuthContract::default()
        });
        let plan = CredentialPreparationPlan::new(
            selected_scope.clone(),
            AuthKind::Secrets,
            &none_selection(&selected_scope),
            ["first", "second"]
                .into_iter()
                .map(|name| {
                    CredentialPreparationBinding::new(
                        CredentialMaterialBindingName::new(name).expect("binding name"),
                        CredentialMaterialKind::SecretBinding,
                        1024,
                    )
                    .expect("preparation binding")
                })
                .collect(),
        )
        .expect("preparation plan");
        let mut adapter = ScopedCredentialMaterialAdapter::for_secret_references(
            Arc::clone(&resolver),
            validate_skill_runtime_contract(&contract).expect("valid contract"),
            &plan,
            route(),
        )
        .expect("secret adapter");

        assert_eq!(
            with_prepared_credential_material(&plan, &mut adapter, |material| material.len())
                .expect("single-use preparation"),
            2
        );
        assert_eq!(
            resolver
                .resolve_for_scope("owner", "default")
                .expect("scoped store")
                .evaluate_access(
                    "SHARED_SECRET",
                    "generic_runtime",
                    "execute",
                    Some("api.example.com"),
                )
                .expect("policy decision"),
            PolicyResult::Allowed
        );
    }

    #[test]
    fn secret_resolution_failures_are_value_free() {
        let (resolver, _root) = test_resolver();
        let selected_scope = scope("owner");
        let secret_ref = "CANARY_MISSING_SECRET_REFERENCE";
        let contract = secret_contract("api_key", secret_ref);
        let plan = preparation_plan(
            &selected_scope,
            AuthKind::Secrets,
            "api_key",
            CredentialMaterialKind::SecretBinding,
        );
        let mut adapter = ScopedCredentialMaterialAdapter::for_secret_references(
            resolver,
            validate_skill_runtime_contract(&contract).expect("valid secret contract"),
            &plan,
            route(),
        )
        .expect("secret adapter");

        let error = run_adapter(&plan, &mut adapter).expect_err("secret is missing");
        let diagnostic = format!("{error:?} {error}");
        assert_eq!(
            error.resolution_failure,
            Some(CredentialResolutionFailure::Missing)
        );
        assert!(!diagnostic.contains(secret_ref));
    }

    #[test]
    fn profile_secret_adapter_rechecks_contract_provider_and_skips_empty_store_resolution() {
        let (resolver, _root) = test_resolver();
        let selected_scope = scope("owner");
        let plan = CredentialPreparationPlan::new(
            selected_scope.clone(),
            AuthKind::CliProfile,
            &implicit_selection(&selected_scope, "provider-a"),
            Vec::new(),
        )
        .expect("implicit profile plan");
        let contract = cli_contract(AuthContract {
            kind: AuthKind::CliProfile,
            requirement: AuthRequirement::Required,
            provider: Some("provider-a".to_owned()),
            profile_selection: ProfileSelection::Implicit,
            storage: AuthStorage::CliOwned,
            ..AuthContract::default()
        });
        let mut adapter = ScopedCredentialMaterialAdapter::for_secret_references(
            Arc::clone(&resolver),
            validate_skill_runtime_contract(&contract).expect("valid profile contract"),
            &plan,
            route(),
        )
        .expect("empty profile adapter");
        assert!(
            with_prepared_credential_material(&plan, &mut adapter, |material| material.is_empty())
                .expect("empty preparation")
        );

        let wrong_contract = cli_contract(AuthContract {
            provider: Some("provider-b".to_owned()),
            ..contract.auth.clone()
        });
        assert_eq!(
            ScopedCredentialMaterialAdapter::for_secret_references(
                resolver,
                validate_skill_runtime_contract(&wrong_contract).expect("valid profile contract"),
                &plan,
                route(),
            )
            .err()
            .expect("provider mismatch")
            .code,
            CredentialMaterialAdapterErrorCode::ContractMismatch
        );
    }

    #[test]
    fn delegated_adapter_preserves_exact_binding_and_replay_semantics() {
        let (resolver, _root) = test_resolver();
        let selected_scope = scope("owner");
        let provider = CredentialProviderId::new("provider-a").expect("provider");
        let secret_ref = CredentialSecretReference::new("DELEGATED_SECRET").expect("secret ref");
        provision(
            &resolver,
            &selected_scope,
            secret_ref.as_str(),
            HashMap::from([("value".to_owned(), "delegated-canary".to_owned())]),
        );
        let issued = issue_bound_delegated_credential(
            &resolver,
            &selected_scope,
            &secret_ref,
            &route(),
            &provider,
            "agent-a",
        )
        .expect("delegated grant");
        let replay_copy = issued.token.as_str().to_owned();
        let contract = delegated_contract("provider-a");
        let plan = preparation_plan(
            &selected_scope,
            AuthKind::DelegatedCredential,
            DELEGATED_CREDENTIAL_BINDING,
            CredentialMaterialKind::DelegatedCredential,
        );

        let adapter = |token| {
            ScopedCredentialMaterialAdapter::for_delegated_credential(
                Arc::clone(&resolver),
                validate_skill_runtime_contract(&contract).expect("delegated contract"),
                &plan,
                route(),
                "agent-a",
                token,
            )
            .expect("delegated adapter")
        };
        let mut first = adapter(issued);
        assert_eq!(
            run_adapter(&plan, &mut first).expect("first redemption"),
            (1, Some(CredentialMaterialKind::DelegatedCredential))
        );
        let mut replay = adapter(delegated_token(&resolver, &selected_scope, replay_copy));
        assert_eq!(
            run_adapter(&plan, &mut replay)
                .expect_err("replay fails")
                .resolution_failure,
            Some(CredentialResolutionFailure::Stale)
        );
    }

    #[test]
    fn unused_delegated_adapter_discards_its_private_grant_on_drop() {
        let (resolver, _root) = test_resolver();
        let selected_scope = scope("owner");
        let provider = CredentialProviderId::new("provider-a").expect("provider");
        let secret_ref = CredentialSecretReference::new("DELEGATED_SECRET").expect("secret ref");
        provision(
            &resolver,
            &selected_scope,
            secret_ref.as_str(),
            HashMap::from([("value".to_owned(), "delegated-canary".to_owned())]),
        );
        let issued = issue_bound_delegated_credential(
            &resolver,
            &selected_scope,
            &secret_ref,
            &route(),
            &provider,
            "agent-a",
        )
        .expect("delegated grant");
        let replay_copy = issued.token.as_str().to_owned();
        let contract = delegated_contract("provider-a");
        let plan = preparation_plan(
            &selected_scope,
            AuthKind::DelegatedCredential,
            DELEGATED_CREDENTIAL_BINDING,
            CredentialMaterialKind::DelegatedCredential,
        );
        let adapter = ScopedCredentialMaterialAdapter::for_delegated_credential(
            Arc::clone(&resolver),
            validate_skill_runtime_contract(&contract).expect("delegated contract"),
            &plan,
            route(),
            "agent-a",
            issued,
        )
        .expect("delegated adapter");
        drop(adapter);

        let mut replay = ScopedCredentialMaterialAdapter::for_delegated_credential(
            Arc::clone(&resolver),
            validate_skill_runtime_contract(&contract).expect("delegated contract"),
            &plan,
            route(),
            "agent-a",
            delegated_token(&resolver, &selected_scope, replay_copy),
        )
        .expect("replay adapter");
        assert_eq!(
            run_adapter(&plan, &mut replay)
                .expect_err("dropped adapter must discard its grant")
                .resolution_failure,
            Some(CredentialResolutionFailure::Stale)
        );
    }

    #[test]
    fn concurrent_delegated_redemption_has_exactly_one_winner() {
        const CONTENDERS: usize = 16;

        let (resolver, _root) = test_resolver();
        let selected_scope = scope("owner");
        let provider = CredentialProviderId::new("provider-a").expect("provider");
        let secret_ref = CredentialSecretReference::new("DELEGATED_SECRET").expect("secret ref");
        provision(
            &resolver,
            &selected_scope,
            secret_ref.as_str(),
            HashMap::from([("value".to_owned(), "delegated-canary".to_owned())]),
        );
        let issued = issue_bound_delegated_credential(
            &resolver,
            &selected_scope,
            &secret_ref,
            &route(),
            &provider,
            "agent-a",
        )
        .expect("delegated grant");
        let adversarial_copy = issued.token.as_str().to_owned();
        drop(issued);

        let contract = delegated_contract("provider-a");
        let plan = preparation_plan(
            &selected_scope,
            AuthKind::DelegatedCredential,
            DELEGATED_CREDENTIAL_BINDING,
            CredentialMaterialKind::DelegatedCredential,
        );
        let barrier = Arc::new(Barrier::new(CONTENDERS + 1));
        let mut joins = Vec::new();
        for _ in 0..CONTENDERS {
            let resolver = Arc::clone(&resolver);
            let contract = contract.clone();
            let plan = plan.clone();
            let barrier = Arc::clone(&barrier);
            let adversarial_copy = adversarial_copy.clone();
            joins.push(thread::spawn(move || {
                let mut adapter = ScopedCredentialMaterialAdapter::for_delegated_credential(
                    Arc::clone(&resolver),
                    validate_skill_runtime_contract(&contract).expect("delegated contract"),
                    &plan,
                    route(),
                    "agent-a",
                    delegated_token(&resolver, plan.scope(), adversarial_copy),
                )
                .expect("delegated adapter");
                barrier.wait();
                run_adapter(&plan, &mut adapter)
            }));
        }
        barrier.wait();

        let results = joins
            .into_iter()
            .map(|join| join.join().expect("redemption contender"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| result.as_ref().is_err_and(|error| {
                    error.resolution_failure == Some(CredentialResolutionFailure::Stale)
                }))
                .count(),
            CONTENDERS - 1
        );
    }

    #[test]
    fn parallel_static_secret_resolution_remains_exact_to_each_scope() {
        const ATTEMPTS_PER_SCOPE: usize = 12;

        let (resolver, _root) = test_resolver();
        let allowed_scope = scope("allowed-owner");
        let denied_scope = scope("denied-owner");
        let secret_ref = "SHARED_SECRET_REFERENCE";
        provision_with_policy(
            &resolver,
            &allowed_scope,
            secret_ref,
            HashMap::from([("value".to_owned(), "allowed-scope-canary".to_owned())]),
            SecretPolicy {
                max_uses_per_day: Some(1),
                ..SecretPolicy::default()
            },
        );
        provision_with_policy(
            &resolver,
            &denied_scope,
            secret_ref,
            HashMap::from([("value".to_owned(), "denied-scope-canary".to_owned())]),
            SecretPolicy {
                allowed_tools: vec!["other_tool/other_action".to_owned()],
                ..SecretPolicy::default()
            },
        );

        let contract = secret_contract("api_key", secret_ref);
        let barrier = Arc::new(Barrier::new(ATTEMPTS_PER_SCOPE * 2 + 1));
        let mut joins = Vec::new();
        for (selected_scope, should_succeed) in [(&allowed_scope, true), (&denied_scope, false)] {
            for _ in 0..ATTEMPTS_PER_SCOPE {
                let resolver = Arc::clone(&resolver);
                let contract = contract.clone();
                let selected_scope = selected_scope.clone();
                let barrier = Arc::clone(&barrier);
                joins.push(thread::spawn(move || {
                    let plan = preparation_plan(
                        &selected_scope,
                        AuthKind::Secrets,
                        "api_key",
                        CredentialMaterialKind::SecretBinding,
                    );
                    let mut adapter = ScopedCredentialMaterialAdapter::for_secret_references(
                        resolver,
                        validate_skill_runtime_contract(&contract).expect("secret contract"),
                        &plan,
                        route(),
                    )
                    .expect("secret adapter");
                    barrier.wait();
                    (should_succeed, run_adapter(&plan, &mut adapter))
                }));
            }
        }
        barrier.wait();

        let results = joins
            .into_iter()
            .map(|join| join.join().expect("scope contender"))
            .collect::<Vec<_>>();
        assert_eq!(
            results
                .iter()
                .filter(|(should_succeed, result)| *should_succeed && result.is_ok())
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|(should_succeed, result)| *should_succeed
                    && result.as_ref().is_err_and(|error| {
                        error.resolution_failure == Some(CredentialResolutionFailure::Denied)
                    }))
                .count(),
            ATTEMPTS_PER_SCOPE - 1
        );
        assert_eq!(
            results
                .iter()
                .filter(|(should_succeed, result)| !*should_succeed
                    && result.as_ref().is_err_and(|error| {
                        error.resolution_failure == Some(CredentialResolutionFailure::Denied)
                    }))
                .count(),
            ATTEMPTS_PER_SCOPE
        );
    }

    #[test]
    fn delegated_adapter_rejects_agent_provider_route_scope_and_expiry_drift() {
        struct DriftCase<'a> {
            issue_scope: &'a str,
            expected_scope: &'a str,
            issue_provider: &'a str,
            expected_provider: &'a str,
            issue_agent: &'a str,
            expected_agent: &'a str,
            issue_domain: Option<&'a str>,
            expected_domain: Option<&'a str>,
            ttl_secs: i64,
            expected_failure: CredentialResolutionFailure,
        }

        let cases = [
            DriftCase {
                issue_scope: "owner",
                expected_scope: "owner",
                issue_provider: "provider-a",
                expected_provider: "provider-a",
                issue_agent: "agent-a",
                expected_agent: "agent-b",
                issue_domain: Some("api.example.com"),
                expected_domain: Some("api.example.com"),
                ttl_secs: 30,
                expected_failure: CredentialResolutionFailure::BindingMismatch,
            },
            DriftCase {
                issue_scope: "owner",
                expected_scope: "owner",
                issue_provider: "provider-a",
                expected_provider: "provider-b",
                issue_agent: "agent-a",
                expected_agent: "agent-a",
                issue_domain: Some("api.example.com"),
                expected_domain: Some("api.example.com"),
                ttl_secs: 30,
                expected_failure: CredentialResolutionFailure::BindingMismatch,
            },
            DriftCase {
                issue_scope: "owner",
                expected_scope: "owner",
                issue_provider: "provider-a",
                expected_provider: "provider-a",
                issue_agent: "agent-a",
                expected_agent: "agent-a",
                issue_domain: None,
                expected_domain: Some("api.example.com"),
                ttl_secs: 30,
                expected_failure: CredentialResolutionFailure::BindingMismatch,
            },
            DriftCase {
                issue_scope: "owner",
                expected_scope: "other-owner",
                issue_provider: "provider-a",
                expected_provider: "provider-a",
                issue_agent: "agent-a",
                expected_agent: "agent-a",
                issue_domain: Some("api.example.com"),
                expected_domain: Some("api.example.com"),
                ttl_secs: 30,
                expected_failure: CredentialResolutionFailure::Stale,
            },
            DriftCase {
                issue_scope: "owner",
                expected_scope: "owner",
                issue_provider: "provider-a",
                expected_provider: "provider-a",
                issue_agent: "agent-a",
                expected_agent: "agent-a",
                issue_domain: Some("api.example.com"),
                expected_domain: Some("api.example.com"),
                ttl_secs: -1,
                expected_failure: CredentialResolutionFailure::Expired,
            },
        ];

        for case in cases {
            let (resolver, _root) = test_resolver();
            let issue_scope = scope(case.issue_scope);
            let expected_scope = scope(case.expected_scope);
            let issue_provider =
                CredentialProviderId::new(case.issue_provider).expect("issue provider");
            let secret_ref =
                CredentialSecretReference::new("DELEGATED_SECRET").expect("secret ref");
            provision(
                &resolver,
                &issue_scope,
                secret_ref.as_str(),
                HashMap::from([("value".to_owned(), "delegated-canary".to_owned())]),
            );
            let raw = resolver
                .resolve_for_scope(
                    issue_scope.principal.as_str(),
                    issue_scope.workspace.as_str(),
                )
                .expect("issue store")
                .issue_delegated_grant(
                    secret_ref.as_str(),
                    "generic_runtime",
                    "execute",
                    case.issue_domain,
                    delegated_authority(&issue_scope, &issue_provider, case.issue_agent),
                    Some(case.ttl_secs),
                )
                .expect("delegated grant");
            let contract = delegated_contract(case.expected_provider);
            let plan = preparation_plan(
                &expected_scope,
                AuthKind::DelegatedCredential,
                DELEGATED_CREDENTIAL_BINDING,
                CredentialMaterialKind::DelegatedCredential,
            );
            let expected_route = CredentialGrantRoute::new(
                "generic_runtime",
                "execute",
                case.expected_domain.map(ToOwned::to_owned),
            )
            .expect("expected route");
            let mut adapter = ScopedCredentialMaterialAdapter::for_delegated_credential(
                Arc::clone(&resolver),
                validate_skill_runtime_contract(&contract).expect("delegated contract"),
                &plan,
                expected_route,
                case.expected_agent,
                delegated_token(&resolver, &expected_scope, raw),
            )
            .expect("delegated adapter");

            assert_eq!(
                run_adapter(&plan, &mut adapter)
                    .expect_err("authority drift fails closed")
                    .resolution_failure,
                Some(case.expected_failure)
            );
        }
    }

    #[test]
    fn route_and_agent_diagnostics_never_echo_authored_values() {
        let route_canary = "route with CANARY_ROUTE";
        let route_error =
            CredentialGrantRoute::new(route_canary, "execute", None).expect_err("invalid route");
        assert!(!format!("{route_error:?} {route_error}").contains(route_canary));

        let (resolver, _root) = test_resolver();
        let selected_scope = scope("owner");
        let contract = delegated_contract("provider-a");
        let plan = preparation_plan(
            &selected_scope,
            AuthKind::DelegatedCredential,
            DELEGATED_CREDENTIAL_BINDING,
            CredentialMaterialKind::DelegatedCredential,
        );
        let agent_canary = "../CANARY_AGENT";
        let error = ScopedCredentialMaterialAdapter::for_delegated_credential(
            Arc::clone(&resolver),
            validate_skill_runtime_contract(&contract).expect("delegated contract"),
            &plan,
            route(),
            agent_canary,
            delegated_token(&resolver, &selected_scope, "opaque-test-token"),
        )
        .err()
        .expect("invalid agent");
        assert_eq!(error.code, CredentialMaterialAdapterErrorCode::InvalidAgent);
        assert!(!format!("{error:?} {error}").contains(agent_canary));
    }

    /// Build the real installed shape: `<skill>/config` is a symlink into a
    /// sibling `.skill-state/<skill>/config`, where the credential actually
    /// lives. Returns the aliased path a pack carries and the real target.
    #[cfg(unix)]
    fn installed_scope_environment(root: &Path, mode: u32, body: &str) -> (PathBuf, PathBuf) {
        let state = root.join(".skill-state/example/config");
        fs::create_dir_all(&state).expect("skill state config");
        let target = state.join(".env");
        fs::write(&target, body).expect("write scoped environment");
        fs::set_permissions(&target, fs::Permissions::from_mode(mode)).expect("set mode");
        let skill = root.join("example");
        fs::create_dir_all(&skill).expect("installed skill directory");
        symlink(&state, skill.join("config")).expect("installed config link");
        (skill.join("config/.env"), target)
    }

    #[cfg(unix)]
    #[test]
    fn scoped_environment_resolves_the_installed_symlinked_config_layout() {
        let root = TempDir::new().expect("scoped environment root");
        let (aliased, _target) =
            installed_scope_environment(root.path(), 0o600, "FIXTURE_API_KEY=value\n");

        // The path a pack carries is never canonical — `config` is a symlink,
        // and on macOS the temporary root is itself reached through one. Both
        // must resolve rather than be refused.
        assert_ne!(
            aliased,
            fs::canonicalize(&aliased).expect("canonical alias")
        );
        ScopedSecretEnvironmentAuthority::open(&aliased)
            .expect("the installed symlinked config layout must resolve");
    }

    #[cfg(unix)]
    #[test]
    fn scoped_environment_pins_the_canonical_target_not_the_link() {
        let root = TempDir::new().expect("scoped environment root");
        let (aliased, target) =
            installed_scope_environment(root.path(), 0o600, "FIXTURE_API_KEY=value\n");
        let canonical = fs::canonicalize(&target).expect("canonical target");

        let authority =
            ScopedSecretEnvironmentAuthority::open(&aliased).expect("symlinked layout resolves");

        assert_eq!(authority.path, canonical);
        // The pinned identity is the target's inode, not the link's, and it
        // records this process as the owner — the ownership guard that stands
        // in for the symlink refusal.
        let expected = scoped_secret_environment_identity(
            &fs::symlink_metadata(&canonical).expect("canonical metadata"),
        )
        .expect("canonical identity");
        assert!(authority.identity == expected);
        assert_eq!(authority.identity.owner, unsafe { libc::geteuid() });
    }

    #[cfg(unix)]
    #[test]
    fn scoped_environment_refuses_a_group_or_world_reachable_target() {
        // Resolution must not become a permissive read: the mode guard is
        // judged on the resolved file, exactly as before.
        for mode in [0o640, 0o604, 0o660, 0o666] {
            let root = TempDir::new().expect("scoped environment root");
            let (aliased, _) = installed_scope_environment(root.path(), mode, "KEY=value\n");
            assert_eq!(
                ScopedSecretEnvironmentAuthority::open(&aliased)
                    .err()
                    .expect("a group or world reachable credential must be refused")
                    .code,
                CredentialMaterialAdapterErrorCode::InvalidSecretEnvironment,
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn scoped_environment_refuses_a_dangling_or_empty_target() {
        let root = TempDir::new().expect("scoped environment root");
        let skill = root.path().join("example");
        fs::create_dir_all(&skill).expect("installed skill directory");
        symlink(root.path().join("absent/config"), skill.join("config"))
            .expect("dangling config link");
        // Canonicalization fails closed rather than degrading to the raw path.
        assert_eq!(
            ScopedSecretEnvironmentAuthority::open(&skill.join("config/.env"))
                .err()
                .expect("a dangling credential link must be refused")
                .code,
            CredentialMaterialAdapterErrorCode::InvalidSecretEnvironment,
        );

        let empty_root = TempDir::new().expect("scoped environment root");
        let (empty, _) = installed_scope_environment(empty_root.path(), 0o600, "");
        assert_eq!(
            ScopedSecretEnvironmentAuthority::open(&empty)
                .err()
                .expect("an empty credential file must be refused")
                .code,
            CredentialMaterialAdapterErrorCode::InvalidSecretEnvironment,
        );
    }

    #[cfg(unix)]
    #[test]
    fn scoped_environment_refuses_a_target_that_is_not_a_regular_file() {
        let root = TempDir::new().expect("scoped environment root");
        let state = root.path().join(".skill-state/example/config/.env");
        fs::create_dir_all(&state).expect("directory standing in for the credential");
        let skill = root.path().join("example");
        fs::create_dir_all(&skill).expect("installed skill directory");
        symlink(
            root.path().join(".skill-state/example/config"),
            skill.join("config"),
        )
        .expect("installed config link");
        assert_eq!(
            ScopedSecretEnvironmentAuthority::open(&skill.join("config/.env"))
                .err()
                .expect("a directory must never be read as a credential file")
                .code,
            CredentialMaterialAdapterErrorCode::InvalidSecretEnvironment,
        );
    }
}
