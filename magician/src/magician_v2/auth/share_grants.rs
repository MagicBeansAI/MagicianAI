//! Share grants — the §5b P1 sharing layer (`scopes/<principal>/<workspace>/share_grants.yaml`).
//!
//! Design: `docs/archive/plans/2026-08-23-magician-auth-identity-workspace-design.md` §5b.
//! Terminology is deliberate: these are **share grants**; `plt_` grants own the
//! word "grant" in HTTP-land (see `sessions::TerminalGrant`).
//!
//! # The two axes
//!
//! §5b's axes table is binding here:
//!
//! | axis | example | sharing means | P1 status |
//! | --- | --- | --- | --- |
//! | workspace (same principal) | `me/personal` → `me/company` | convenience — focus, attribution, blast radius | **this module** |
//! | principal (different people) | `me/default` → `partner/default` | **consent** | **P2, out of scope here** |
//!
//! `federated_sources` therefore enumerates sibling workspaces of the SAME
//! principal only. Cross-PRINCIPAL grant files (`scopes/<other>/…` granting to
//! the reader) are structurally invisible to P1 lookup — they are never read.
//! P2 adds them behind credentials-mode enforcement, refusing with
//! `scope_auth_required` until that enforcement flips (§5b rules).
//!
//! # Rules (all enforced here, none delegated to callers)
//!
//! * **Deny by default** — no `share_grants.yaml` means today's behavior
//!   exactly: zero federation, no error.
//! * **Per class, never blanket** — a grant names individual classes.
//! * **Read never confers write** — this module is read-only; it exposes no
//!   write path, and nothing it returns may be routed back into a write into
//!   the granting scope.
//! * **Nothing is copied** — grants are federated at query time; revocation
//!   (delete the file, or drop the class/grantee row) takes effect on the
//!   next lookup because no state was replicated.
//! * **Non-grantable classes fail loudly at load** — `memory_index` (derived,
//!   rebuildable), `secrets`, and `auth` may never appear in a grant file;
//!   listing one is [`ShareGrantsError::NonGrantableClass`], never a silent
//!   drop. `auth` is additionally structurally unreachable (§2.1 keeps
//!   credentials at `system/auth/`, outside every scope).
//! * **Fail closed on corruption** — an unreadable or corrupt grant file
//!   surfaces as an error, never as "no grants": silently skipping a source
//!   would turn a truncation into an unintended disclosure boundary change.
//!   The caller decides how to report it.
//!
//! # Federation results
//!
//! Federation helpers return ONLY the granted sources' records. The caller
//! unions those with its own-scope results itself — own-scope reads are not
//! "federated" and never pass through this module.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{validate_principal_name, ScopeRef};

/// File name of the per-scope grant registry, inside the scope directory.
pub const SHARE_GRANTS_FILE: &str = "share_grants.yaml";
/// The only `schema_version` this code understands.
pub const SHARE_GRANTS_SCHEMA_VERSION: u32 = 1;

/// A data class that can appear in a share grant. The non-grantable variants
/// exist so a file listing them can be rejected **by name** at load — being
/// unrepresentable would make them silently droppable, the exact failure §5b
/// forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ShareClass {
    /// `memory/users` — the actual facts (the grantable memory surface).
    MemoryUsers,
    /// `memory/index` — derived and rebuildable; **non-grantable**.
    MemoryIndex,
    /// Notes.
    Notes,
    /// **Non-grantable.**
    Secrets,
    /// **Non-grantable** (and structurally unreachable — §2.1).
    Auth,
}

impl ShareClass {
    /// Wire name as it appears in `share_grants.yaml`.
    pub fn as_str(self) -> &'static str {
        match self {
            ShareClass::MemoryUsers => "memory_users",
            ShareClass::MemoryIndex => "memory_index",
            ShareClass::Notes => "notes",
            ShareClass::Secrets => "secrets",
            ShareClass::Auth => "auth",
        }
    }

    /// Parse a wire name; unknown names are an error (typo protection), not a
    /// silent skip.
    pub fn parse(raw: &str) -> Result<Self, ShareGrantsError> {
        match raw.trim() {
            "memory_users" => Ok(ShareClass::MemoryUsers),
            "memory_index" => Ok(ShareClass::MemoryIndex),
            "notes" => Ok(ShareClass::Notes),
            "secrets" => Ok(ShareClass::Secrets),
            "auth" => Ok(ShareClass::Auth),
            other => Err(ShareGrantsError::UnknownClass(other.to_string())),
        }
    }

    /// Whether a grant file may list this class at all.
    pub fn is_grantable(self) -> bool {
        !matches!(
            self,
            ShareClass::MemoryIndex | ShareClass::Secrets | ShareClass::Auth
        )
    }
}

/// Everything share grants refuse, in one place. Loud by design: a share
/// registry that degrades quietly changes a disclosure boundary without
/// anyone noticing.
#[derive(Debug, thiserror::Error)]
pub enum ShareGrantsError {
    #[error("cannot read share grant file {path:?}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("share grant file {path:?} is corrupt: {message}")]
    Corrupt { path: PathBuf, message: String },
    #[error("share grant file schema version {0} is unsupported (expected {SHARE_GRANTS_SCHEMA_VERSION})")]
    UnsupportedSchemaVersion(u32),
    #[error("unknown share class {0:?} (known: memory_users, memory_index, notes, secrets, auth)")]
    UnknownClass(String),
    #[error("share class {class:?} is not grantable — memory_index, secrets, and auth can never be granted")]
    NonGrantableClass { class: ShareClass },
    #[error("share grant grantee {grantee:?} in {path:?} is not a valid principal name")]
    InvalidGrantee { grantee: String, path: PathBuf },
}

/// One grant row in `share_grants.yaml`, as written (classes still wire
/// strings — parsing/validation is [`ShareGrants`]'s job so typos fail with
/// this module's errors rather than serde's).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShareGrantEntry {
    /// Principal name allowed to read — validated by
    /// [`validate_principal_name`] (same pattern as directory names).
    pub grantee: String,
    pub classes: Vec<String>,
}

/// The on-disk file shape:
///
/// ```yaml
/// schema_version: 1
/// grants:
///   - grantee: "company-agent"
///     classes: [memory_users, notes]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShareGrantsFile {
    pub schema_version: u32,
    #[serde(default)]
    pub grants: Vec<ShareGrantEntry>,
}

/// One validated grant row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareGrant {
    pub grantee: String,
    pub classes: Vec<ShareClass>,
}

/// A loaded-and-validated grant file. Building one is the load path below —
/// every invariant (valid grantee names, known classes, grantable classes)
/// has already been checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShareGrants {
    grants: Vec<ShareGrant>,
}

impl ShareGrants {
    /// All classes this file grants to `grantee`, in declaration order,
    /// deduplicated.
    pub fn classes_for(&self, grantee: &str) -> Vec<ShareClass> {
        let mut classes = Vec::new();
        for grant in &self.grants {
            if grant.grantee == grantee {
                for class in &grant.classes {
                    if !classes.contains(class) {
                        classes.push(*class);
                    }
                }
            }
        }
        classes
    }

    /// Does this file grant `class` to `grantee`?
    pub fn grants_class_to(&self, grantee: &str, class: ShareClass) -> bool {
        self.grants
            .iter()
            .any(|grant| grant.grantee == grantee && grant.classes.contains(&class))
    }
}

/// `scopes/<principal>/<workspace>/share_grants.yaml` for a scope.
pub fn share_grants_path(scopes_root: &Path, scope: &ScopeRef) -> PathBuf {
    scopes_root
        .join(scope.principal())
        .join(scope.workspace())
        .join(SHARE_GRANTS_FILE)
}

/// Load and validate one scope's grant file.
///
/// * No file → `Ok(None)` (deny by default — today's behavior, no error).
/// * An empty file → `Ok(None)` (grants nothing; deny by default holds).
/// * Anything unreadable, unparseable, or invalid → `Err` (fail closed).
///
/// Pure and synchronous: plain file reads under `scopes_root`, no store lock.
pub fn load_for_scope(
    scopes_root: &Path,
    scope: &ScopeRef,
) -> Result<Option<ShareGrants>, ShareGrantsError> {
    let path = share_grants_path(scopes_root, scope);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ShareGrantsError::Io { path, source });
        },
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    let file: ShareGrantsFile =
        serde_yaml::from_slice(&bytes).map_err(|error| ShareGrantsError::Corrupt {
            path: path.clone(),
            message: error.to_string(),
        })?;
    if file.schema_version != SHARE_GRANTS_SCHEMA_VERSION {
        return Err(ShareGrantsError::UnsupportedSchemaVersion(
            file.schema_version,
        ));
    }
    let mut grants = Vec::with_capacity(file.grants.len());
    for entry in file.grants {
        if validate_principal_name(&entry.grantee).is_err() {
            return Err(ShareGrantsError::InvalidGrantee {
                grantee: entry.grantee,
                path,
            });
        }
        let mut classes = Vec::with_capacity(entry.classes.len());
        for raw in entry.classes {
            let class = ShareClass::parse(&raw)?;
            if !class.is_grantable() {
                return Err(ShareGrantsError::NonGrantableClass { class });
            }
            if !classes.contains(&class) {
                classes.push(class);
            }
        }
        grants.push(ShareGrant {
            grantee: entry.grantee,
            classes,
        });
    }
    Ok(Some(ShareGrants { grants }))
}

/// A scope whose grant file federates to a reader, plus everything that file
/// grants that reader (not just the queried class — callers sizing a UI or a
/// cache want the full surface).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FederatedSource {
    pub scope: ScopeRef,
    pub classes: Vec<ShareClass>,
}

/// Enumerate the scopes federated to `reader` for `class` — sibling
/// workspaces of the **same principal** only (§5b's workspace axis).
///
/// * Siblings are the directories under `scopes/<reader.principal()>/` other
///   than the reader's own workspace (own scope is not "federated" — the
///   caller already reads it). Non-directory entries such as
///   `workspaces.json` and names failing the principal pattern are skipped.
/// * A sibling federates when its grant file grants `class` to
///   `grantee == reader.principal()`.
/// * No principal directory at all → `Ok(vec![])`, not an error.
/// * Any sibling's grant file that exists but is unreadable/corrupt/invalid
///   → `Err` (fail closed — the caller decides, this never silently skips).
/// * Cross-principal grant files are never even read (P1 scope; see the
///   module header).
///
/// Deterministic order: workspace name ascending.
pub fn federated_sources(
    scopes_root: &Path,
    reader: &ScopeRef,
    class: ShareClass,
) -> Result<Vec<FederatedSource>, ShareGrantsError> {
    let principal_dir = scopes_root.join(reader.principal());
    let entries = match std::fs::read_dir(&principal_dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ShareGrantsError::Io {
                path: principal_dir,
                source,
            });
        },
    };
    let mut workspaces: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name != reader.workspace() && validate_principal_name(name).is_ok())
        .collect();
    workspaces.sort();

    let mut sources = Vec::new();
    for workspace in workspaces {
        // The principal is inherited from the trusted reader scope and the
        // workspace is a pattern-validated directory name from our own
        // scopes tree — disk-derived, not client-supplied — so the system
        // constructor is legitimate here. Nothing from an HTTP request
        // reaches this call.
        let scope = ScopeRef::system_internal_unauthenticated(reader.principal(), &workspace);
        if let Some(grants) = load_for_scope(scopes_root, &scope)? {
            if grants.grants_class_to(reader.principal(), class) {
                sources.push(FederatedSource {
                    scope,
                    classes: grants.classes_for(reader.principal()),
                });
            }
        }
    }
    Ok(sources)
}

/// A record read from a federated source, stamped with the scope it came
/// from. §5b: "every returned record keeps its source scope stamp so agents
/// know whose knowledge they act on and provenance survives."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FederatedRecord<T> {
    pub source: ScopeRef,
    pub value: T,
}

/// Errors from [`federated_read`]: the grant lookup itself, or a per-source
/// read raised by the caller's closure.
#[derive(Debug, thiserror::Error)]
pub enum FederatedReadError<E> {
    #[error("share grant lookup failed: {0}")]
    Sources(#[from] ShareGrantsError),
    #[error("federated read from a granted source failed: {0}")]
    Read(E),
}

/// Read records from every scope federated to `reader` for `class`, using the
/// caller's `read_one` closure per granted source.
///
/// Returns **only the granted sources' records** — own-scope results are the
/// caller's to add; federation adds granted sources, it never replaces the
/// union. This is the less-surprising contract: a caller that federates notes
/// keeps its own notes by construction, not by a helper's union policy.
///
/// Read failures fail the whole call (fail closed, same policy as
/// [`federated_sources`]); a source that cannot be read is not silently
/// omitted. Synchronous and store-lock-free, like the rest of this module.
pub fn federated_read<T, E>(
    scopes_root: &Path,
    reader: &ScopeRef,
    class: ShareClass,
    read_one: impl Fn(&ScopeRef) -> Result<Vec<T>, E>,
) -> Result<Vec<FederatedRecord<T>>, FederatedReadError<E>> {
    let sources = federated_sources(scopes_root, reader, class)?;
    let mut records = Vec::new();
    for source in sources {
        let values = read_one(&source.scope).map_err(FederatedReadError::Read)?;
        records.extend(values.into_iter().map(|value| FederatedRecord {
            source: source.scope.clone(),
            value,
        }));
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scope(principal: &str, workspace: &str) -> ScopeRef {
        ScopeRef::system_internal_unauthenticated(principal, workspace)
    }

    fn write_grants(scopes_root: &Path, principal: &str, workspace: &str, yaml: &str) {
        let dir = scopes_root.join(principal).join(workspace);
        fs::create_dir_all(&dir).expect("create scope dir");
        fs::write(dir.join(SHARE_GRANTS_FILE), yaml).expect("write grant file");
    }

    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let scopes = dir.path().join("scopes");
        (dir, scopes)
    }

    #[test]
    fn no_file_is_deny_by_default_and_not_an_error() {
        let (_dir, scopes) = setup();
        let reader = scope("owner", "default");
        // No principal directory at all.
        assert!(load_for_scope(&scopes, &reader).expect("load").is_none());
        assert!(federated_sources(&scopes, &reader, ShareClass::MemoryUsers)
            .expect("sources")
            .is_empty());
        // Principal directory with workspaces, but no grant files anywhere.
        fs::create_dir_all(scopes.join("owner").join("company")).expect("dirs");
        assert!(load_for_scope(&scopes, &reader).expect("load").is_none());
        assert!(federated_sources(&scopes, &reader, ShareClass::MemoryUsers)
            .expect("sources")
            .is_empty());
    }

    #[test]
    fn same_principal_other_workspace_grant_federates() {
        let (_dir, scopes) = setup();
        write_grants(
            &scopes,
            "owner",
            "company",
            "schema_version: 1\ngrants:\n  - grantee: owner\n    classes: [memory_users, notes]\n",
        );
        let reader = scope("owner", "default");
        let sources =
            federated_sources(&scopes, &reader, ShareClass::MemoryUsers).expect("sources");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].scope.workspace(), "company");
        assert_eq!(sources[0].scope.principal(), "owner");
        assert_eq!(
            sources[0].classes,
            vec![ShareClass::MemoryUsers, ShareClass::Notes],
            "classes carries the full grant, not just the queried class"
        );
        // The other granted class finds the same source.
        assert_eq!(
            federated_sources(&scopes, &reader, ShareClass::Notes)
                .expect("sources")
                .len(),
            1
        );
        // A class NOT granted finds nothing — per-class, never blanket. (A
        // non-grantable class can never be granted by a loadable file, so
        // querying it simply yields no sources.)
        assert!(federated_sources(&scopes, &reader, ShareClass::MemoryIndex)
            .expect("sources")
            .is_empty());
    }

    #[test]
    fn own_workspace_is_excluded_even_if_it_grants_to_itself() {
        let (_dir, scopes) = setup();
        write_grants(
            &scopes,
            "owner",
            "default",
            "schema_version: 1\ngrants:\n  - grantee: owner\n    classes: [memory_users]\n",
        );
        fs::create_dir_all(scopes.join("owner").join("company")).expect("dirs");
        let reader = scope("owner", "default");
        let sources =
            federated_sources(&scopes, &reader, ShareClass::MemoryUsers).expect("sources");
        assert!(
            sources.is_empty(),
            "own scope is not federated; the caller reads it directly"
        );
    }

    #[test]
    fn cross_principal_grants_are_invisible_to_p1_lookup() {
        let (_dir, scopes) = setup();
        // A different principal's scope grants to the reader by name.
        write_grants(
            &scopes,
            "partner",
            "default",
            "schema_version: 1\ngrants:\n  - grantee: owner\n    classes: [memory_users]\n",
        );
        let reader = scope("owner", "default");
        assert!(
            federated_sources(&scopes, &reader, ShareClass::MemoryUsers)
                .expect("sources")
                .is_empty(),
            "P1 federates the workspace axis only; cross-principal is P2"
        );
    }

    #[test]
    fn non_grantable_class_fails_loudly_at_load() {
        for yaml in [
            "schema_version: 1\ngrants:\n  - grantee: owner\n    classes: [secrets]\n",
            "schema_version: 1\ngrants:\n  - grantee: owner\n    classes: [memory_index]\n",
            "schema_version: 1\ngrants:\n  - grantee: owner\n    classes: [notes, auth]\n",
        ] {
            let (_dir, scopes) = setup();
            write_grants(&scopes, "owner", "company", yaml);
            let granting = scope("owner", "company");
            let outcome = load_for_scope(&scopes, &granting);
            assert!(
                matches!(outcome, Err(ShareGrantsError::NonGrantableClass { .. })),
                "expected NonGrantableClass for {yaml:?}, got {outcome:?}"
            );
        }
    }

    #[test]
    fn unknown_class_fails_loudly() {
        let (_dir, scopes) = setup();
        write_grants(
            &scopes,
            "owner",
            "company",
            "schema_version: 1\ngrants:\n  - grantee: owner\n    classes: [memory_users, note]\n",
        );
        let outcome = load_for_scope(&scopes, &scope("owner", "company"));
        assert!(
            matches!(outcome, Err(ShareGrantsError::UnknownClass(ref class)) if class == "note"),
            "expected UnknownClass(\"note\"), got {outcome:?}"
        );
    }

    #[test]
    fn invalid_grantee_fails_loudly() {
        let (_dir, scopes) = setup();
        write_grants(
            &scopes,
            "owner",
            "company",
            "schema_version: 1\ngrants:\n  - grantee: \"../etc\"\n    classes: [notes]\n",
        );
        let outcome = load_for_scope(&scopes, &scope("owner", "company"));
        assert!(
            matches!(
                outcome,
                Err(ShareGrantsError::InvalidGrantee { ref grantee, .. }) if grantee == "../etc"
            ),
            "expected InvalidGrantee, got {outcome:?}"
        );
    }

    #[test]
    fn corrupt_yaml_is_an_error_never_a_skip() {
        let (_dir, scopes) = setup();
        write_grants(&scopes, "owner", "company", "grants: [ {");
        let reader = scope("owner", "default");
        let outcome = federated_sources(&scopes, &reader, ShareClass::Notes);
        assert!(
            matches!(outcome, Err(ShareGrantsError::Corrupt { .. })),
            "corrupt files fail closed, got {outcome:?}"
        );
        // Wrong schema version is equally loud (checked on a clean tree so
        // the corrupt file above is not what this half observes).
        let (_dir2, scopes2) = setup();
        write_grants(
            &scopes2,
            "owner",
            "company",
            "schema_version: 2\ngrants: []\n",
        );
        let outcome = load_for_scope(&scopes2, &scope("owner", "company"));
        assert!(
            matches!(outcome, Err(ShareGrantsError::UnsupportedSchemaVersion(2))),
            "expected UnsupportedSchemaVersion, got {outcome:?}"
        );
    }

    #[test]
    fn revocation_is_immediate_on_file_deletion() {
        let (_dir, scopes) = setup();
        write_grants(
            &scopes,
            "owner",
            "company",
            "schema_version: 1\ngrants:\n  - grantee: owner\n    classes: [memory_users]\n",
        );
        let reader = scope("owner", "default");
        assert_eq!(
            federated_sources(&scopes, &reader, ShareClass::MemoryUsers)
                .expect("sources")
                .len(),
            1
        );
        fs::remove_file(share_grants_path(&scopes, &scope("owner", "company"))).expect("revoke");
        assert!(
            federated_sources(&scopes, &reader, ShareClass::MemoryUsers)
                .expect("sources")
                .is_empty(),
            "nothing was copied, so deletion is the revocation"
        );
    }

    #[test]
    fn federated_read_stamps_sources_and_skips_own_scope() {
        let (_dir, scopes) = setup();
        // Two sibling workspaces; only company grants notes.
        write_grants(
            &scopes,
            "owner",
            "company",
            "schema_version: 1\ngrants:\n  - grantee: owner\n    classes: [notes]\n",
        );
        fs::create_dir_all(scopes.join("owner").join("scratch")).expect("dirs");
        for (workspace, marker) in [
            ("default", "own"),
            ("company", "granted"),
            ("scratch", "none"),
        ] {
            let dir = scopes.join("owner").join(workspace);
            fs::create_dir_all(&dir).expect("dirs");
            fs::write(dir.join("marker.txt"), marker).expect("marker");
        }
        let reader = scope("owner", "default");
        let records = federated_read(
            &scopes,
            &reader,
            ShareClass::Notes,
            |source| -> Result<Vec<String>, String> {
                let marker = fs::read_to_string(scopes_root_marker(&scopes, source))
                    .map_err(|error| error.to_string())?;
                Ok(vec![marker])
            },
        )
        .expect("federated read");
        assert_eq!(records.len(), 1, "only granted sources contribute");
        assert_eq!(records[0].value, "granted");
        assert_eq!(records[0].source.workspace(), "company");
        assert_eq!(records[0].source.principal(), "owner");
    }

    /// Test helper: the per-scope "record" the federated_read test reads.
    fn scopes_root_marker(scopes_root: &Path, source: &ScopeRef) -> PathBuf {
        scopes_root
            .join(source.principal())
            .join(source.workspace())
            .join("marker.txt")
    }
}
