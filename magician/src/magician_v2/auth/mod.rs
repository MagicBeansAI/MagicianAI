//! Magician identity, authentication, and workspace authorization.
//!
//! This module owns [`ScopeRef`] — the `(principal, workspace)` pair every
//! principal-scoped API resolves through. The struct's fields are private:
//! a scope is something Magician *knows*, not something a caller *says*.
//! There are deliberately few ways to build one:
//!
//! * [`ScopeRef::system_internal_unauthenticated`] — for system work with no
//!   request behind it (schedulers, reconcilers, migrations, startup
//!   seeding). The name is deliberately ugly so it is greppable in review;
//!   nothing client-supplied may reach it.
//! * `ScopeRef::from_session` (added with the auth store) — the only path
//!   from an HTTP request to a scope. The principal comes from a verified
//!   session; the workspace is the caller's choice, checked against what
//!   the session's principal owns.
//!
//! Historical definition: `artifact_v2/service.rs`; re-exported there so
//! existing imports keep resolving. Design:
//! `docs/archive/plans/2026-08-23-magician-identity-and-auth-design.md` §4 and
//! `docs/archive/plans/2026-08-23-magician-auth-identity-workspace-design.md` §4.

use serde::{Deserialize, Serialize};

pub mod bot_tokens;
pub mod credentials;
pub mod identity;
pub mod middleware;
pub mod sessions;
pub mod share_grants;
pub mod social;
pub mod store;
pub mod workspace_registry;

pub use bot_tokens::{bot_token_registry, BotGrant, BotTokenRegistry, BOT_TOKEN_PREFIX};
pub use credentials::{AuthMethod, CredentialKind, Provider};
pub use identity::{validate_principal_name, Identity, PRINCIPAL_PATTERN};
pub use sessions::{
    floored_tools, ApiToken, BearerIdentity, BearerKind, MintGrantSpec, Session, TerminalGrant,
    API_TOKEN_PREFIX, GRANT_TOKEN_PREFIX, GRANT_TTL_MAX_HOURS, GRANT_TTL_MIN_HOURS,
    NEVER_ON_THE_PLANE, SESSION_TOKEN_PREFIX,
};
pub use share_grants::{
    federated_read, federated_sources, load_for_scope, FederatedReadError, FederatedRecord,
    FederatedSource, ShareClass, ShareGrant, ShareGrants, ShareGrantsError, ShareGrantsFile,
    SHARE_GRANTS_FILE, SHARE_GRANTS_SCHEMA_VERSION,
};
pub use store::{AuthStore, AuthStoreError};
pub use workspace_registry::{Workspace, DEFAULT_WORKSPACE_ID};

/// Everything auth refuses, in one place. Handlers map these to the repo's
/// ad-hoc JSON error convention; the store returns them verbatim.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("principal name {0:?} is invalid: must match {PRINCIPAL_PATTERN}")]
    InvalidPrincipalName(String),
    #[error("identity {0:?} already exists")]
    IdentityExists(String),
    #[error("identity {0:?} does not exist")]
    IdentityNotFound(String),
    #[error("{provider} subject is already linked to identity {identity:?}")]
    ProviderSubjectAlreadyLinked {
        provider: String,
        subject: String,
        identity: String,
    },
    #[error("username or password is incorrect")]
    InvalidCredentials,
    #[error("password hashing failed: {0}")]
    PasswordHash(String),
    #[error("principal {principal:?} does not own workspace {workspace:?}")]
    UnownedWorkspace {
        principal: String,
        workspace: String,
    },
    /// Terminal-grant `ttl_hours` outside 1..=2160 — the bounds live in
    /// `sessions` as `GRANT_TTL_MIN_HOURS`/`GRANT_TTL_MAX_HOURS`.
    #[error("grant ttl_hours {0} is out of range: must be 1..=2160 hours")]
    InvalidGrantTtl(u64),
    #[error("workspace {0:?} already exists")]
    WorkspaceExists(String),
    #[error("the default workspace cannot be deleted or renamed")]
    DefaultWorkspaceProtected,
    #[error("workspace {0:?} still has live state on disk; clear it before deleting")]
    WorkspaceHasLiveState(String),
    /// The slug belongs to a workspace deleted with its data whose directory
    /// is removed at the next server start; until then the name is taken.
    #[error("workspace {0:?} was deleted with its data and is removed at the next restart")]
    WorkspacePendingPurge(String),
    #[error("auth store failure: {0}")]
    Store(#[from] AuthStoreError),
}

/// A `(principal, workspace)` pair whose origin the type system can vouch
/// for.
///
/// Serialization is unchanged from the historical shape (`{"principal": …,
/// "workspace": …}`) so durable records — pause states above all — roundtrip
/// identically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeRef {
    principal: String,
    workspace: String,
}

impl ScopeRef {
    /// For system work with no request behind it — schedulers,
    /// reconcilers, migrations, startup seeding — and, still today, the
    /// legacy header-claim path in `api_scope`
    /// (`resolve_required_scope_ref`), the single seam through which the
    /// ~60 HTTP-derived call sites build their scopes. The middleware has
    /// landed, which changes what that path means rather than removing
    /// it: with a valid bearer,
    /// `auth::middleware::authenticate_request` engraves proven
    /// `x-principal`/`x-workspace` (resolved via [`Self::from_session`])
    /// over whatever the client sent before the handler reads them; in
    /// `open` mode without a bearer the middleware engraves the fixed
    /// `anonymous/default` scope — and only while this install has no
    /// identity at all; once one exists the middleware refuses bearerless
    /// calls in every mode, so the adopted tree never stays reachable
    /// without a credential. So HTTP-derived callers reach this
    /// constructor only with server-owned compatibility context.
    /// TODO(auth-migration): give the HTTP-derived arm its own dedicated
    /// constructor (or extend middleware coverage to every route) so the
    /// type system stops vouching for claims it cannot see.
    /// Deliberately ugly to name and greppable in review.
    pub fn system_internal_unauthenticated(principal: &str, workspace: &str) -> Self {
        Self {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        }
    }

    /// The only path from an HTTP request to a scope (workspace design §4).
    ///
    /// The **principal comes only from the identity** — the session's
    /// proven identity, whose `scope_root` may alias `anonymous` — never
    /// from a parameter or header. The **workspace is the caller's
    /// choice**, validated against the identity's ownership registry:
    /// absent lands on `default`, unowned is refused.
    /// From a bot daemon's in-memory grant.
    ///
    /// The scope comes from the GRANT, never from the request: the runtime
    /// minted the token immediately before spawning that bot, for the scope
    /// whose `bots/` directory the config was read from. A caller can present
    /// the token but cannot influence what it resolves to.
    ///
    /// Separate from [`Self::system_internal_unauthenticated`] on purpose —
    /// there IS a request behind this one, and a distinct constructor keeps
    /// both greppable in review.
    pub fn from_bot_grant(grant: &bot_tokens::BotGrant) -> Self {
        Self {
            principal: grant.principal.clone(),
            workspace: grant.workspace.clone(),
        }
    }

    pub fn from_session(
        scopes_root: &std::path::Path,
        identity: &Identity,
        requested_workspace: Option<&str>,
    ) -> Result<Self, AuthError> {
        let workspace = match requested_workspace
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            None => DEFAULT_WORKSPACE_ID.to_string(),
            Some(requested) => {
                let owned = workspace_registry::owns(scopes_root, identity, requested)
                    .map_err(AuthError::from)?;
                if !owned {
                    return Err(AuthError::UnownedWorkspace {
                        principal: identity.scope_root.clone(),
                        workspace: requested.to_string(),
                    });
                }
                requested.to_string()
            },
        };
        Ok(Self {
            principal: identity.scope_root.clone(),
            workspace,
        })
    }

    /// The proven identity this scope belongs to.
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// The authorized workspace this scope addresses.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_ref_serde_roundtrip_keeps_the_historical_shape() {
        let scope = ScopeRef::system_internal_unauthenticated("anonymous", "default");
        let serialized = serde_json::to_string(&scope).expect("serialize");
        assert_eq!(
            serialized,
            r#"{"principal":"anonymous","workspace":"default"}"#
        );
        let parsed: ScopeRef = serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(parsed, scope);
    }

    #[test]
    fn accessors_expose_fields_without_allowing_construction() {
        let scope = ScopeRef::system_internal_unauthenticated("owner", "default");
        assert_eq!(scope.principal(), "owner");
        assert_eq!(scope.workspace(), "default");
    }
}
