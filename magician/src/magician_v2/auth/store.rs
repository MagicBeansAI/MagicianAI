//! The unified expiring-credential store — `system/auth/`.
//!
//! Design: `docs/archive/plans/2026-08-23-magician-auth-identity-workspace-design.md`
//! §2.1 and §2.4. One subsystem owns identities, credentials, sessions, API
//! tokens, and terminal grants (the plane's `plt_` grants landed in
//! `tokens.json` per its Task 10 — identity doc §7: "the grant store and
//! the credential store are one subsystem").
//!
//! Concurrency: the service is a single process, and every mutation holds
//! the cache lock across read-modify-write — two concurrent mints cannot
//! lose an update. All I/O inside the lock is synchronous (tiny JSON
//! documents, temp+rename+fsync), so no async future ever holds the guard.
//! Reads are mtime-checked pass-throughs: the per-request bearer resolve
//! costs one `stat` on an unchanged file, not a JSON parse.
//!
//! Failure ordering: `create_identity` writes the credential row *before*
//! the identity row. A crash between the two leaves a harmless orphan
//! credential (it references an identity that does not exist, so it can
//! never authenticate) rather than an identity that exists but cannot log
//! in and cannot be re-created.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{Duration, Utc};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::magician_v2::artifact_v2::io::write_bytes_durably_with_mode_sync;

use super::credentials::{AuthMethod, Credential, CredentialKind, CredentialsFile, Provider};
use super::identity::{validate_principal_name, IdentitiesFile, Identity, ADOPTED_SCOPE_ROOT};
use super::sessions::{
    hash_token, mint_token_value, ApiToken, BearerIdentity, BearerKind, MintGrantSpec, Session,
    SessionsFile, TerminalGrant, TokenKind, TokensFile, API_TOKEN_PREFIX, GRANT_TOKEN_PREFIX,
    GRANT_TTL_MAX_HOURS, GRANT_TTL_MIN_HOURS, NEVER_ON_THE_PLANE, SESSION_TOKEN_PREFIX,
};

/// Auth files are credentials — owner-read/write only.
const AUTH_FILE_MODE: u32 = 0o600;

const IDENTITIES_FILE: &str = "identities.json";
const CREDENTIALS_FILE: &str = "credentials.json";
const SESSIONS_FILE: &str = "sessions.json";
const TOKENS_FILE: &str = "tokens.json";

#[derive(Debug, thiserror::Error)]
pub enum AuthStoreError {
    #[error("auth store io: {0}")]
    Io(#[from] std::io::Error),
    #[error("auth store serialization: {0}")]
    Serde(#[from] serde_json::Error),
}

struct CachedFile<T> {
    mtime: Option<SystemTime>,
    len: u64,
    value: Arc<T>,
}

#[derive(Default)]
struct AuthCache {
    identities: Option<CachedFile<IdentitiesFile>>,
    credentials: Option<CachedFile<CredentialsFile>>,
    sessions: Option<CachedFile<SessionsFile>>,
    tokens: Option<CachedFile<TokensFile>>,
}

/// Read `path`, reusing `cached` when (mtime, len) is unchanged. Returns the
/// parsed value plus the observed metadata (`None` when the file does not
/// exist — first boot is an empty document, not an error).
fn load_fresh<T>(
    path: &Path,
    cached: Option<&CachedFile<T>>,
) -> Result<(Arc<T>, Option<(Option<SystemTime>, u64)>), AuthStoreError>
where
    T: serde::de::DeserializeOwned + Default,
{
    let meta = match std::fs::metadata(path) {
        Ok(meta) => Some(meta),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let observed = meta.as_ref().map(|meta| (meta.modified().ok(), meta.len()));
    if let (Some(cached), Some((mtime, len))) = (cached, observed) {
        if cached.mtime == mtime && cached.len == len {
            return Ok((cached.value.clone(), observed));
        }
    }
    let value: T = match (&meta, observed) {
        (Some(_), Some(_)) => {
            let bytes = std::fs::read(path)?;
            if bytes.is_empty() {
                T::default()
            } else {
                serde_json::from_slice(&bytes)?
            }
        },
        _ => T::default(),
    };
    Ok((Arc::new(value), observed))
}

fn write_file<T>(path: &Path, value: &T) -> Result<(), AuthStoreError>
where
    T: serde::Serialize,
{
    let bytes = serde_json::to_vec_pretty(value)?;
    write_bytes_durably_with_mode_sync(path, &bytes, Some(AUTH_FILE_MODE))?;
    Ok(())
}

/// Generates the cache-plumbing for one auth document:
/// a cache-checked reader `$load`, and `$mutate`, which holds the cache
/// lock across the whole read-modify-write so concurrent mutations cannot
/// lose updates.
macro_rules! auth_file_plumbing {
    ($load:ident, $mutate:ident, $field:ident, $ty:ty, $file_name:expr) => {
        fn $load(&self) -> Result<Arc<$ty>, AuthStoreError> {
            let path = self.dir.join($file_name);
            let mut cache = self.cache.lock();
            let (value, observed) = load_fresh(&path, cache.$field.as_ref())?;
            let reused = cache
                .$field
                .as_ref()
                .is_some_and(|cached| Arc::ptr_eq(&cached.value, &value));
            if !reused {
                cache.$field = observed.map(|(mtime, len)| CachedFile {
                    mtime,
                    len,
                    value: value.clone(),
                });
            }
            Ok(value)
        }

        fn $mutate<R>(&self, body: impl FnOnce(&mut $ty) -> R) -> Result<R, AuthStoreError> {
            let path = self.dir.join($file_name);
            let mut cache = self.cache.lock();
            let (current, _) = load_fresh(&path, cache.$field.as_ref())?;
            let mut file = (*current).clone();
            let result = body(&mut file);
            write_file(&path, &file)?;
            let meta = std::fs::metadata(&path).ok();
            cache.$field = Some(CachedFile {
                mtime: meta.as_ref().and_then(|meta| meta.modified().ok()),
                len: meta.map(|meta| meta.len()).unwrap_or(0),
                value: Arc::new(file),
            });
            Ok(result)
        }
    };
}

pub struct AuthStore {
    runtime_root: PathBuf,
    dir: PathBuf,
    cache: Mutex<AuthCache>,
}

impl AuthStore {
    /// Open (creating `system/auth/` if needed) under the resolved runtime
    /// root — the same root `artifact_v2::workspace` resolves.
    pub fn open(runtime_root: &Path) -> Result<Self, AuthStoreError> {
        let dir = runtime_root.join("system").join("auth");
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            runtime_root: runtime_root.to_path_buf(),
            dir,
            cache: Mutex::new(AuthCache::default()),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The resolved runtime root this store lives under — the workspace
    /// registry's `scopes/` tree hangs off the same root.
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    auth_file_plumbing!(
        load_identities,
        mutate_identities,
        identities,
        IdentitiesFile,
        IDENTITIES_FILE
    );
    auth_file_plumbing!(
        load_credentials,
        mutate_credentials,
        credentials,
        CredentialsFile,
        CREDENTIALS_FILE
    );
    auth_file_plumbing!(
        load_sessions,
        mutate_sessions,
        sessions,
        SessionsFile,
        SESSIONS_FILE
    );
    auth_file_plumbing!(load_tokens, mutate_tokens, tokens, TokensFile, TOKENS_FILE);

    // ---- identities ------------------------------------------------------

    pub fn identities_empty(&self) -> Result<bool, AuthStoreError> {
        Ok(self.load_identities()?.identities.is_empty())
    }

    pub fn find_identity(&self, name: &str) -> Result<Option<Identity>, AuthStoreError> {
        Ok(self
            .load_identities()?
            .identities
            .iter()
            .find(|identity| identity.name == name)
            .cloned())
    }

    pub fn list_identities(&self) -> Result<Vec<Identity>, AuthStoreError> {
        Ok(self.load_identities()?.identities.clone())
    }

    /// Create an identity with its first credential. The **first** identity
    /// on an install adopts `scopes/anonymous/` via `scope_root` aliasing —
    /// no data moves (workspace design §2.2, §6). Later identities get a
    /// fresh root named after themselves.
    pub fn create_identity(
        &self,
        name: &str,
        display_name: &str,
        credential: CredentialKind,
    ) -> Result<Identity, super::AuthError> {
        validate_principal_name(name)?;
        if self.find_identity(name)?.is_some() {
            return Err(super::AuthError::IdentityExists(name.to_string()));
        }
        if let CredentialKind::OAuthLink { provider, subject } = &credential {
            if let Some(existing) = self.resolve_oauth_credential(provider, subject)? {
                return Err(super::AuthError::ProviderSubjectAlreadyLinked {
                    provider: provider.as_str().to_string(),
                    subject: subject.clone(),
                    identity: existing,
                });
            }
        }
        // Credential row first — see the module doc's failure-ordering note.
        // (The identities mutation below re-checks the duplicate so a race
        // between two creations cannot write two identity rows; the orphan
        // credential that window can leave is inert by construction.)
        let credential_identity = name.to_string();
        self.mutate_credentials(|file| {
            file.credentials.push(Credential {
                id: Uuid::new_v4(),
                identity: credential_identity,
                kind: credential,
                created_at: Utc::now(),
                last_used: None,
            });
        })
        .map_err(super::AuthError::from)?;
        let identity = self
            .mutate_identities(|file| {
                if file.identities.iter().any(|identity| identity.name == name) {
                    return Err(super::AuthError::IdentityExists(name.to_string()));
                }
                let scope_root = if file.identities.is_empty() {
                    ADOPTED_SCOPE_ROOT.to_string()
                } else {
                    name.to_string()
                };
                let identity = Identity {
                    name: name.to_string(),
                    display_name: display_name.to_string(),
                    scope_root,
                    created_at: Utc::now(),
                };
                file.identities.push(identity.clone());
                Ok(identity)
            })
            .map_err(super::AuthError::from)??;
        // Mint the default workspace's registry row beside the identity
        // (workspace design §2.5) — for the adopted anonymous root this
        // only adds the row; the data already exists.
        super::workspace_registry::ensure_default(&self.runtime_root.join("scopes"), &identity)
            .map_err(super::AuthError::from)?;
        Ok(identity)
    }

    /// Every distinct scope root owned by some identity — the reconciler's
    /// ownership set.
    pub fn scope_roots(&self) -> Result<Vec<String>, AuthStoreError> {
        Ok(self
            .load_identities()?
            .identities
            .iter()
            .map(|identity| identity.scope_root.clone())
            .collect())
    }

    /// Create an identity from a social profile, deriving a pattern-valid
    /// principal name (§13 Q1) with numeric suffixes on collision. The
    /// adoption rule inside `create_identity` still applies: the first
    /// identity on an install adopts `anonymous`.
    pub fn create_social_identity(
        &self,
        provider: Provider,
        subject: &str,
        display_name: &str,
    ) -> Result<Identity, super::AuthError> {
        let base = super::social::derive_principal_name(provider, subject);
        for attempt in 1..=50u32 {
            let name = super::social::collision_variant(&base, attempt);
            if self.find_identity(&name)?.is_some() {
                continue;
            }
            return self.create_identity(
                &name,
                display_name,
                CredentialKind::OAuthLink {
                    provider,
                    subject: subject.to_string(),
                },
            );
        }
        Err(super::AuthError::IdentityExists(base))
    }

    // ---- credentials -----------------------------------------------------

    /// Verify a username/password pair. `Ok(None)` for every failure mode —
    /// unknown user, no password credential, malformed hash, wrong password
    /// — so the answer is not a user-enumeration oracle. A successful verify
    /// records `last_used` on the credential.
    pub fn verify_password(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<Identity>, AuthStoreError> {
        let identity_exists = self
            .load_identities()?
            .identities
            .iter()
            .any(|identity| identity.name == username);
        if !identity_exists {
            // Burn identical argon2 work (hash and verify cost the same) so
            // response timing does not reveal which usernames exist. A
            // precomputed fake hash could parse-reject fast; minting one
            // cannot.
            let _ = super::credentials::hash_password(password);
            return Ok(None);
        }
        let stored_hash = self
            .load_credentials()?
            .credentials
            .iter()
            .filter(|credential| {
                credential.identity == username
                    && matches!(credential.kind, CredentialKind::Password { .. })
            })
            .find_map(|credential| match &credential.kind {
                CredentialKind::Password { hash } => Some(hash.clone()),
                _ => None,
            });
        let Some(stored_hash) = stored_hash else {
            return Ok(None);
        };
        if !super::credentials::verify_password(&stored_hash, password) {
            return Ok(None);
        }
        self.mutate_credentials(|file| {
            let now = Utc::now();
            for credential in file.credentials.iter_mut() {
                if credential.identity == username
                    && matches!(credential.kind, CredentialKind::Password { .. })
                {
                    credential.last_used = Some(now);
                }
            }
        })?;
        Ok(self
            .load_identities()?
            .identities
            .iter()
            .find(|identity| identity.name == username)
            .cloned())
    }

    /// Resolve an OAuth link to its identity name. Keys on (provider,
    /// subject) — never email.
    pub fn resolve_oauth_credential(
        &self,
        provider: &Provider,
        subject: &str,
    ) -> Result<Option<String>, AuthStoreError> {
        Ok(self
            .load_credentials()?
            .credentials
            .iter()
            .find(|credential| match &credential.kind {
                CredentialKind::OAuthLink {
                    provider: p,
                    subject: s,
                } => p == provider && s == subject,
                _ => false,
            })
            .map(|credential| credential.identity.clone()))
    }

    /// Attach another credential to an existing identity (explicit linking,
    /// workspace design §3.2). One credential per (provider, subject).
    pub fn link_credential(
        &self,
        identity: &str,
        kind: CredentialKind,
    ) -> Result<(), super::AuthError> {
        if self.find_identity(identity)?.is_none() {
            return Err(super::AuthError::IdentityNotFound(identity.to_string()));
        }
        self.mutate_credentials(|file| {
            if let CredentialKind::OAuthLink { provider, subject } = &kind {
                let duplicate = file.credentials.iter().any(|credential| {
                    matches!(&credential.kind, CredentialKind::OAuthLink { provider: p, subject: s } if p == provider && s == subject)
                });
                if duplicate {
                    return Err(super::AuthError::ProviderSubjectAlreadyLinked {
                        provider: provider.as_str().to_string(),
                        subject: subject.clone(),
                        identity: identity.to_string(),
                    });
                }
            }
            file.credentials.push(Credential {
                id: Uuid::new_v4(),
                identity: identity.to_string(),
                kind,
                created_at: Utc::now(),
                last_used: None,
            });
            Ok(())
        })?
    }

    // ---- sessions --------------------------------------------------------

    pub fn mint_session(
        &self,
        identity: &str,
        workspace: &str,
        method: AuthMethod,
        ttl_days: u64,
    ) -> Result<(Session, String), super::AuthError> {
        let Some(owner) = self.find_identity(identity)? else {
            return Err(super::AuthError::IdentityNotFound(identity.to_string()));
        };
        let workspace = workspace.trim();
        let scopes_root = self.runtime_root.join("scopes");
        if !super::workspace_registry::owns(&scopes_root, &owner, workspace)? {
            return Err(super::AuthError::UnownedWorkspace {
                principal: owner.scope_root,
                workspace: workspace.to_string(),
            });
        }
        let value = mint_token_value(SESSION_TOKEN_PREFIX);
        let session = Session {
            token_hash: hash_token(&value),
            identity: identity.to_string(),
            workspace: workspace.to_string(),
            method,
            created_at: Utc::now(),
            expires_at: Utc::now() + Duration::days(ttl_days.max(1) as i64),
        };
        // Sweeping on mint bounds the file; resolve stays read-only so the
        // per-request path never writes.
        let minted = session.clone();
        self.mutate_sessions(|file| {
            let now = Utc::now();
            file.sessions.retain(|existing| existing.expires_at > now);
            file.sessions.push(minted);
        })
        .map_err(super::AuthError::from)?;
        Ok((session, value))
    }

    /// Resolve a session token value to its session. Expired sessions
    /// resolve to `None`.
    pub fn resolve_session(&self, token_value: &str) -> Result<Option<Session>, AuthStoreError> {
        let hash = hash_token(token_value);
        let now = Utc::now();
        Ok(self
            .load_sessions()?
            .sessions
            .iter()
            .find(|session| session.token_hash == hash && session.expires_at > now)
            .cloned())
    }

    pub fn revoke_session(&self, token_value: &str) -> Result<bool, AuthStoreError> {
        let hash = hash_token(token_value);
        self.mutate_sessions(|file| {
            let before = file.sessions.len();
            file.sessions.retain(|session| session.token_hash != hash);
            before != file.sessions.len()
        })
    }

    /// Replace a login session with an equally-lived session engraved for a
    /// different owned workspace. The old bearer and new bearer are swapped
    /// in one store mutation, so a workspace switch cannot leave a reusable
    /// identity-only credential behind.
    pub fn rotate_session_scope(
        &self,
        token_value: &str,
        workspace: &str,
    ) -> Result<Option<(Session, String)>, super::AuthError> {
        let Some(current) = self.resolve_session(token_value)? else {
            return Ok(None);
        };
        let Some(owner) = self.find_identity(&current.identity)? else {
            return Ok(None);
        };
        let workspace = workspace.trim();
        let scopes_root = self.runtime_root.join("scopes");
        if !super::workspace_registry::owns(&scopes_root, &owner, workspace)? {
            return Err(super::AuthError::UnownedWorkspace {
                principal: owner.scope_root,
                workspace: workspace.to_string(),
            });
        }

        let value = mint_token_value(SESSION_TOKEN_PREFIX);
        let replacement = Session {
            token_hash: hash_token(&value),
            identity: current.identity,
            workspace: workspace.to_string(),
            method: current.method,
            created_at: current.created_at,
            expires_at: current.expires_at,
        };
        let old_hash = hash_token(token_value);
        let minted = replacement.clone();
        let replaced =
            self.mutate_sessions(|file| {
                let Some(slot) = file.sessions.iter_mut().find(|session| {
                    session.token_hash == old_hash && session.expires_at > Utc::now()
                }) else {
                    return false;
                };
                *slot = minted;
                true
            })?;
        Ok(replaced.then_some((replacement, value)))
    }

    // ---- API tokens ------------------------------------------------------

    pub fn mint_api_token(
        &self,
        identity: &str,
        workspace: &str,
        label: &str,
    ) -> Result<(ApiToken, String), super::AuthError> {
        let Some(owner) = self.find_identity(identity)? else {
            return Err(super::AuthError::IdentityNotFound(identity.to_string()));
        };
        let workspace = workspace.trim();
        let scopes_root = self.runtime_root.join("scopes");
        if !super::workspace_registry::owns(&scopes_root, &owner, workspace)? {
            return Err(super::AuthError::UnownedWorkspace {
                principal: owner.scope_root,
                workspace: workspace.to_string(),
            });
        }
        let value = mint_token_value(API_TOKEN_PREFIX);
        let token = ApiToken {
            id: Uuid::new_v4(),
            token_hash: hash_token(&value),
            identity: identity.to_string(),
            workspace: workspace.to_string(),
            label: label.trim().to_string(),
            created_at: Utc::now(),
            last_used: None,
        };
        let minted = token.clone();
        self.mutate_tokens(|file| {
            file.tokens.push(minted);
        })
        .map_err(super::AuthError::from)?;
        Ok((token, value))
    }

    pub fn list_api_tokens(&self, identity: &str) -> Result<Vec<ApiToken>, AuthStoreError> {
        Ok(self
            .load_tokens()?
            .tokens
            .iter()
            .filter(|token| token.identity == identity)
            .cloned()
            .collect())
    }

    pub fn revoke_api_token(&self, identity: &str, id: Uuid) -> Result<bool, AuthStoreError> {
        self.mutate_tokens(|file| {
            let before = file.tokens.len();
            file.tokens
                .retain(|token| !(token.id == id && token.identity == identity));
            before != file.tokens.len()
        })
    }

    // ---- terminal grants (plane plan Task 10, landed here) ----------------

    /// Mint a `plt_` terminal grant — the third mint path. The identity
    /// must exist (it comes from the session the route proved), the
    /// workspace must be **owned** at mint (it is engraved into the record,
    /// not chosen per request), and `ttl_hours` must be 1..=2160. The
    /// allowlist is floored here: `NEVER_ON_THE_PLANE` names never enter
    /// the record; the route reports what was dropped.
    pub fn mint_grant(
        &self,
        identity: &str,
        spec: MintGrantSpec,
    ) -> Result<(TerminalGrant, String), super::AuthError> {
        let Some(owner) = self.find_identity(identity)? else {
            return Err(super::AuthError::IdentityNotFound(identity.to_string()));
        };
        if !(GRANT_TTL_MIN_HOURS..=GRANT_TTL_MAX_HOURS).contains(&spec.ttl_hours) {
            return Err(super::AuthError::InvalidGrantTtl(spec.ttl_hours));
        }
        let workspace = spec.workspace.trim();
        let scopes_root = self.runtime_root.join("scopes");
        let owned = super::workspace_registry::owns(&scopes_root, &owner, workspace)
            .map_err(super::AuthError::from)?;
        if !owned {
            return Err(super::AuthError::UnownedWorkspace {
                principal: owner.scope_root,
                workspace: workspace.to_string(),
            });
        }
        let value = mint_token_value(GRANT_TOKEN_PREFIX);
        let now = Utc::now();
        let grant = TerminalGrant {
            id: Uuid::new_v4(),
            token_hash: hash_token(&value),
            identity: identity.to_string(),
            workspace: workspace.to_string(),
            label: spec.label.trim().to_string(),
            agent_identity: spec.agent_identity.trim().to_string(),
            harness_engine: spec.harness_engine.trim().to_string(),
            allowed_tools: spec
                .allowed_tools
                .iter()
                .map(|tool| tool.trim().to_string())
                .filter(|tool| !tool.is_empty())
                .filter(|tool| !NEVER_ON_THE_PLANE.contains(&tool.as_str()))
                .collect(),
            created_at: now,
            expires_at: now + Duration::hours(spec.ttl_hours as i64),
            max_usd: spec.max_usd.filter(|usd| usd.is_finite() && *usd > 0.0),
            max_wall_clock_secs: spec.max_wall_clock_secs.filter(|secs| *secs > 0),
            max_concurrent_runs: spec.max_concurrent_runs.filter(|runs| *runs > 0),
        };
        // Sweeping on mint bounds the file; resolve stays read-only so the
        // per-request path never writes (the same reasoning as sessions).
        let minted = grant.clone();
        self.mutate_tokens(|file| {
            file.grants.retain(|existing| existing.expires_at > now);
            file.grants.push(minted);
        })
        .map_err(super::AuthError::from)?;
        Ok((grant, value))
    }

    pub fn list_grants(&self, identity: &str) -> Result<Vec<TerminalGrant>, AuthStoreError> {
        Ok(self
            .load_tokens()?
            .grants
            .iter()
            .filter(|grant| grant.identity == identity)
            .cloned()
            .collect())
    }

    pub fn revoke_grant(&self, identity: &str, id: Uuid) -> Result<bool, AuthStoreError> {
        self.mutate_tokens(|file| {
            let before = file.grants.len();
            file.grants
                .retain(|grant| !(grant.id == id && grant.identity == identity));
            before != file.grants.len()
        })
    }

    /// Resolve a grant token value. `None` for unknown, revoked, and
    /// expired alike — indistinguishable by design (plane Task 10: a
    /// terminal must not be able to tell those apart).
    pub fn resolve_grant(
        &self,
        token_value: &str,
    ) -> Result<Option<TerminalGrant>, AuthStoreError> {
        let hash = hash_token(token_value);
        let now = Utc::now();
        Ok(self
            .load_tokens()?
            .grants
            .iter()
            .find(|grant| grant.token_hash == hash && grant.expires_at > now)
            .cloned())
    }

    // ---- unified bearer resolution ----------------------------------------

    /// The middleware's single resolve door (workspace design §7C; identity
    /// doc §7): sessions, API tokens, and `plt_` terminal grants all
    /// resolve here. The grant's workspace travels on
    /// `BearerKind::Grant` — it was engraved at mint, and the middleware
    /// refuses any selector that differs.
    pub fn resolve_bearer(
        &self,
        token_value: &str,
    ) -> Result<Option<BearerIdentity>, AuthStoreError> {
        match super::sessions::classify_token(token_value) {
            TokenKind::Session => {
                Ok(self
                    .resolve_session(token_value)?
                    .map(|session| BearerIdentity {
                        identity: session.identity,
                        workspace: session.workspace,
                        kind: BearerKind::Session(session.method),
                    }))
            },
            TokenKind::ApiToken => {
                let hash = hash_token(token_value);
                Ok(self
                    .load_tokens()?
                    .tokens
                    .iter()
                    .find(|token| token.token_hash == hash)
                    .map(|token| BearerIdentity {
                        identity: token.identity.clone(),
                        workspace: token.workspace.clone(),
                        kind: BearerKind::ApiToken,
                    }))
            },
            TokenKind::Grant => Ok(self
                .resolve_grant(token_value)?
                .map(|grant| BearerIdentity {
                    identity: grant.identity.clone(),
                    workspace: grant.workspace.clone(),
                    kind: BearerKind::Grant {
                        workspace: grant.workspace.clone(),
                    },
                })),
            // Bot tokens are process-local and are resolved by auth middleware
            // before this persistent-store door. They intentionally have no
            // row in sessions.json/tokens.json.
            TokenKind::Bot => Ok(None),
            TokenKind::Unknown => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, AuthStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let auth = AuthStore::open(dir.path()).expect("open");
        (dir, auth)
    }

    fn password_store(username: &str, password: &str) -> (tempfile::TempDir, AuthStore) {
        let (dir, auth) = store();
        let hash = super::super::credentials::hash_password(password).expect("hash");
        auth.create_identity(username, username, CredentialKind::Password { hash })
            .expect("create");
        (dir, auth)
    }

    #[test]
    fn first_identity_adopts_anonymous_and_later_ones_get_fresh_roots() {
        let (_dir, auth) = store();
        let first = auth
            .create_identity(
                "owner",
                "Alex",
                CredentialKind::Password { hash: "x".into() },
            )
            .expect("first");
        assert_eq!(first.scope_root, "anonymous");
        let second = auth
            .create_identity(
                "partner",
                "Partner",
                CredentialKind::Password { hash: "x".into() },
            )
            .expect("second");
        assert_eq!(second.scope_root, "partner");
    }

    #[test]
    fn duplicate_identity_and_duplicate_oauth_link_are_refused() {
        let (_dir, auth) = store();
        auth.create_identity("owner", "G", CredentialKind::Password { hash: "x".into() })
            .expect("create");
        assert!(matches!(
            auth.create_identity("owner", "G", CredentialKind::Password { hash: "x".into() }),
            Err(super::super::AuthError::IdentityExists(_))
        ));
        auth.link_credential(
            "owner",
            CredentialKind::OAuthLink {
                provider: Provider::Google,
                subject: "sub-1".into(),
            },
        )
        .expect("link");
        assert!(matches!(
            auth.link_credential(
                "owner",
                CredentialKind::OAuthLink {
                    provider: Provider::Google,
                    subject: "sub-1".into()
                },
            ),
            Err(super::super::AuthError::ProviderSubjectAlreadyLinked { .. })
        ));
    }

    #[test]
    fn session_lifecycle_mint_resolve_revoke() {
        let (_dir, auth) = password_store("owner", "pw");
        let (session, value) = auth
            .mint_session("owner", "default", AuthMethod::Password, 30)
            .expect("mint");
        assert!(value.starts_with("mag_"));
        assert_eq!(
            auth.resolve_bearer(&value)
                .expect("resolve")
                .map(|b| b.identity),
            Some("owner".to_string())
        );
        // The plaintext token never touches disk.
        let on_disk = std::fs::read_to_string(auth.dir.join(SESSIONS_FILE)).unwrap();
        assert!(!on_disk.contains(&value));
        assert!(auth.revoke_session(&value).expect("revoke"));
        assert_eq!(auth.resolve_bearer(&value).expect("resolve"), None);
        assert!(!session.token_hash.is_empty());
    }

    #[test]
    fn api_token_lifecycle_and_bearer_classification() {
        let (_dir, auth) = password_store("owner", "pw");
        let (token, value) = auth
            .mint_api_token("owner", "default", "CI")
            .expect("mint");
        assert!(value.starts_with("mag_pat_"));
        let resolved = auth
            .resolve_bearer(&value)
            .expect("resolve")
            .expect("token");
        assert_eq!(resolved.identity, "owner");
        assert_eq!(resolved.kind, BearerKind::ApiToken);
        let listed = auth.list_api_tokens("owner").expect("list");
        assert_eq!(listed.len(), 1);
        assert!(!format!("{listed:?}").contains(&value));
        assert!(auth.revoke_api_token("owner", token.id).expect("revoke"));
        assert_eq!(auth.resolve_bearer(&value).expect("resolve"), None);
    }

    #[test]
    fn unknown_bearer_values_fail_closed() {
        let (_dir, auth) = store();
        // A well-formed `plt_` prefix with no matching row resolves to
        // nothing — unknown, revoked, and expired all look alike.
        assert_eq!(auth.resolve_bearer("plt_whatever").expect("resolve"), None);
        assert_eq!(auth.resolve_bearer("czt_whatever").expect("resolve"), None);
    }

    fn grant_spec(workspace: &str, ttl_hours: u64) -> MintGrantSpec {
        MintGrantSpec {
            label: "laptop terminal".to_string(),
            workspace: workspace.to_string(),
            agent_identity: "plane-agent".to_string(),
            harness_engine: "claude_code".to_string(),
            allowed_tools: vec!["read_file".to_string()],
            ttl_hours,
            max_usd: None,
            max_wall_clock_secs: None,
            max_concurrent_runs: None,
        }
    }

    #[test]
    fn grant_mint_engraves_owned_workspace_and_floors_the_allowlist() {
        let (_dir, auth) = password_store("owner", "pw");
        let spec = MintGrantSpec {
            allowed_tools: vec![
                "read_file".to_string(),
                "run_coding_task".to_string(),
                "claude".to_string(),
            ],
            ..grant_spec("default", 24)
        };
        let (grant, value) = auth.mint_grant("owner", spec).expect("mint");
        assert!(value.starts_with("plt_"));
        assert_eq!(grant.workspace, "default", "workspace is engraved at mint");
        assert_eq!(
            grant.identity, "owner",
            "identity is the session's name, never client-chosen"
        );
        assert_eq!(
            grant.allowed_tools,
            vec!["read_file".to_string()],
            "NEVER_ON_THE_PLANE names never enter the record"
        );
        assert_eq!(grant.expires_at, grant.created_at + Duration::hours(24));
        let resolved = auth
            .resolve_bearer(&value)
            .expect("resolve")
            .expect("grant");
        assert_eq!(resolved.identity, "owner");
        assert_eq!(
            resolved.kind,
            BearerKind::Grant {
                workspace: "default".to_string()
            },
            "the engraved workspace travels on the bearer kind"
        );
        // The plaintext token never touches disk.
        let on_disk = std::fs::read_to_string(auth.dir.join(TOKENS_FILE)).unwrap();
        assert!(!on_disk.contains(&value));
        // The engraved workspace must be owned — minting for a workspace
        // the identity does not own is refused outright.
        assert!(matches!(
            auth.mint_grant("owner", grant_spec("not-owned", 24)),
            Err(super::super::AuthError::UnownedWorkspace { .. })
        ));
    }

    #[test]
    fn grant_mint_validates_ttl_and_identity() {
        let (_dir, auth) = password_store("owner", "pw");
        assert!(matches!(
            auth.mint_grant("owner", grant_spec("default", 0)),
            Err(super::super::AuthError::InvalidGrantTtl(0))
        ));
        assert!(matches!(
            auth.mint_grant("owner", grant_spec("default", 2161)),
            Err(super::super::AuthError::InvalidGrantTtl(2161))
        ));
        // The bounds themselves are mintable.
        auth.mint_grant("owner", grant_spec("default", 1))
            .expect("min ttl");
        auth.mint_grant("owner", grant_spec("default", 2160))
            .expect("max ttl");
        assert!(matches!(
            auth.mint_grant("nobody", grant_spec("default", 24)),
            Err(super::super::AuthError::IdentityNotFound(_))
        ));
    }

    #[test]
    fn revoked_and_expired_grants_resolve_none_indistinguishably() {
        let (_dir, auth) = password_store("owner", "pw");
        // Revoked: mint → revoke → resolve None.
        let (revoked, revoked_value) = auth
            .mint_grant("owner", grant_spec("default", 24))
            .expect("mint");
        assert!(auth
            .resolve_grant(&revoked_value)
            .expect("resolve")
            .is_some());
        assert_eq!(auth.list_grants("owner").expect("list").len(), 1);
        assert!(auth.revoke_grant("owner", revoked.id).expect("revoke"));
        assert!(
            auth.resolve_grant(&revoked_value)
                .expect("resolve")
                .is_none(),
            "revoked resolves like unknown"
        );
        assert_eq!(auth.resolve_bearer(&revoked_value).expect("bearer"), None);
        // Expired: a live record with a hand-moved past expires_at, written
        // through the store's own mutation plumbing.
        let (_expired, expired_value) = auth
            .mint_grant("owner", grant_spec("default", 1))
            .expect("mint again");
        auth.mutate_tokens(|file| {
            for record in file.grants.iter_mut() {
                record.expires_at = Utc::now() - Duration::hours(1);
            }
        })
        .expect("mutate");
        assert!(
            auth.resolve_grant(&expired_value)
                .expect("resolve")
                .is_none(),
            "expired resolves like unknown"
        );
        assert_eq!(auth.resolve_bearer(&expired_value).expect("bearer"), None);
        // The next mint sweeps the expired row (bounded file, like sessions).
        let (fresh, _) = auth
            .mint_grant("owner", grant_spec("default", 24))
            .expect("mint");
        let listed = auth.list_grants("owner").expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, fresh.id);
        assert!(!auth
            .revoke_grant("owner", revoked.id)
            .expect("revoke again"));
    }

    #[test]
    fn verify_password_is_not_a_user_enumeration_oracle() {
        let (_dir, auth) = password_store("owner", "pw");
        let known = auth.verify_password("owner", "wrong").expect("verify");
        let unknown = auth.verify_password("nobody", "wrong").expect("verify");
        assert_eq!(known, None);
        assert_eq!(unknown, None);
        assert_eq!(
            auth.verify_password("owner", "pw")
                .expect("verify")
                .map(|i| i.name),
            Some("owner".to_string())
        );
    }
}
