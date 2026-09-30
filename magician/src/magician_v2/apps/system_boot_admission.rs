//! Host-controlled, digest-pinned boot admission for `distribution: system`
//! packages.
//!
//! Two call sites refuse a system manifest with the same sentence — *"system
//! distribution packages require host-controlled digest-pinned boot
//! admission"*. This module is the owner they name, and it is the ONLY way a
//! package acquires system class.
//!
//! The trust argument is deliberately narrow. Manifest distribution is a
//! *claim* an uploaded package can make about itself; it never mints anything.
//! What earns system class is provenance: the bytes came out of the
//! deployment's own read-only seed root, which no HTTP request can write and
//! no package can nominate. That proof travels as [`SystemSeedProvenance`],
//! whose fields are private to this module — no other module in the crate can
//! construct one, so the trusted staging path cannot be reached by anything
//! but this resolver.
//!
//! "Digest-pinned" means the proof carries the exact bundle digest that was
//! admitted. Provenance is bound to bytes, not to a directory name: replacing a
//! seed package's contents produces a different pin rather than inheriting the
//! old one's authority.

use std::{collections::BTreeSet, io, path::Path};

use chrono::{DateTime, Utc};

use thiserror::Error;

use super::{
    authority::{AppAuthorityError, AppScopeAuthentication, AuthenticatedAppScope},
    candidate_publication::{AppCandidatePublicationReceipt, AppCandidatePublicationService},
    manifest::{AppManifestDistribution, AppPackageCandidate},
    models::{AppDigest, AppInstallationId},
    package_staging::{admit_package_directory, AppPackageStagingError},
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

/// A seed directory holding more entries than this is treated as corrupt
/// rather than walked. The deployment ships a fixed, small set of system
/// packages; an unbounded scan here would be a boot-time denial of service
/// reachable by anything that can write near the seed root.
const MAX_SEED_ENTRIES: usize = 256;

/// The subdirectory inside `<seed>/system/<name>/` that holds the package.
const PACKAGE_SUBDIR: &str = "app";

#[derive(Debug, Error)]
pub enum SystemBootAdmissionError {
    #[error("system seed root is unavailable: {0}")]
    SeedUnavailable(String),
    #[error("system seed root holds more than {MAX_SEED_ENTRIES} entries")]
    SeedTooLarge,
    #[error("seed package `{package}` was refused: {source}")]
    Admission {
        package: String,
        #[source]
        source: AppPackageStagingError,
    },
    #[error("seed package `{package}` is not declared `distribution: system`")]
    NotSystemClass { package: String },
    #[error("seed package `{package}` names a package already admitted this boot")]
    DuplicatePackage { package: String },
    #[error("system-package host authority requires the system-admission worker")]
    InvalidHostGrantor,
    #[error("installation `{installation_id}` was not admitted from the system seed this boot")]
    InstallationNotAdmittedForHostGrant { installation_id: AppInstallationId },
    #[error(transparent)]
    Authority(#[from] AppAuthorityError),
}

/// Non-forgeable proof that a specific set of bytes came from the deployment's
/// read-only seed root.
///
/// The `seed_digest` field is private to this module, so no other module —
/// including the rest of this crate — can construct one. The trusted staging
/// and publication paths take this by reference; holding it is the whole
/// authority to bypass the ordinary system-distribution refusal.
///
/// Deliberately not `Clone`, `Serialize` or `Deserialize`: a proof that can be
/// copied out of its admission or rebuilt from bytes is not a proof.
pub(crate) struct SystemSeedProvenance {
    seed_digest: AppDigest,
}

impl SystemSeedProvenance {
    pub(crate) fn seed_digest(&self) -> &AppDigest {
        &self.seed_digest
    }
}

impl std::fmt::Debug for SystemSeedProvenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemSeedProvenance")
            .field("seed_digest", &self.seed_digest)
            .finish()
    }
}

/// One admitted seed package: its bytes, its provenance proof, and the seed
/// directory it came from.
///
/// Move-only by construction — [`Self::into_parts`] consumes it, so a single
/// admission cannot be published twice from the same proof.
pub struct TrustedSystemPackageAdmission {
    package_dir: String,
    candidate: AppPackageCandidate,
    provenance: SystemSeedProvenance,
}

impl TrustedSystemPackageAdmission {
    pub fn package_dir(&self) -> &str {
        &self.package_dir
    }

    pub fn candidate(&self) -> &AppPackageCandidate {
        &self.candidate
    }

    pub fn seed_digest(&self) -> &AppDigest {
        self.provenance.seed_digest()
    }

    pub(crate) fn into_parts(self) -> (String, AppPackageCandidate, SystemSeedProvenance) {
        (self.package_dir, self.candidate, self.provenance)
    }
}

impl std::fmt::Debug for TrustedSystemPackageAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustedSystemPackageAdmission")
            .field("package_dir", &self.package_dir)
            .field("seed_digest", &self.provenance.seed_digest)
            .finish()
    }
}

/// Resolve every `distribution: system` package under the deployment's seed
/// root, admitting each through the same bounded byte reader that ordinary
/// staging uses.
///
/// Ordering is stable (the seed directory names are sorted) so two boots of
/// the same deployment admit in the same order and produce the same log.
///
/// A seed entry that is not a system package is skipped rather than refused:
/// the seed root also holds agent templates, db templates and trust policies,
/// none of which are app packages. A directory that *does* hold a package
/// manifest but does not declare system class is an error, because that is a
/// deployment mistake rather than an unrelated neighbour.
pub fn resolve_system_package_inventory(
    workspace: &ArtifactV2Workspace,
) -> Result<Vec<TrustedSystemPackageAdmission>, SystemBootAdmissionError> {
    resolve_system_package_inventory_from(&workspace.system_seed_root())
}

/// The resolver proper, over an explicit seed root.
///
/// Split out so the pinning tests can drive a real seed directory without
/// standing up a workspace — the thing being pinned is which bytes earn system
/// class, and that decision belongs to the path, not to the workspace.
fn resolve_system_package_inventory_from(
    seed_root: &Path,
) -> Result<Vec<TrustedSystemPackageAdmission>, SystemBootAdmissionError> {
    // Canonicalize the ROOT, and only the root.
    //
    // `admit_package_directory` walks its argument with `O_NOFOLLOW` and
    // refuses any `.` or `..` component, so a seed root reached through a
    // relative path or a symlinked data directory would fail every package —
    // and symlinked data directories are ordinary, not exotic. Resolving the
    // root here is what makes those deployments work.
    //
    // The package directories underneath are deliberately NOT canonicalized:
    // they are joined onto the resolved root and handed to that same
    // symlink-refusing walker, so a single package pointed at somewhere
    // writable is still refused. Trusting the deployment about where its seed
    // lives is not the same as trusting it about what is inside.
    let seed_root = &match seed_root.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(SystemBootAdmissionError::SeedUnavailable(error.to_string())),
    };
    let names = read_seed_entry_names(seed_root)?;

    let mut admitted: Vec<TrustedSystemPackageAdmission> = Vec::new();
    for name in names {
        let package_root = seed_root.join(&name).join(PACKAGE_SUBDIR);
        // A seed entry without an `app/` subdirectory is one of the seed
        // root's many non-package neighbours, not a broken package.
        if !package_root.is_dir() {
            continue;
        }

        let candidate = match admit_package_directory(&package_root) {
            Ok(candidate) => candidate,
            // A directory named `app/` that holds no `SKILL.md` is likewise a
            // neighbour rather than a malformed package. Every other refusal
            // is real and must stop the boot admission rather than silently
            // drop a package the deployment expects to be present.
            Err(AppPackageStagingError::Manifest(
                super::manifest::AppManifestError::MissingSkillManifest,
            )) => continue,
            Err(source) => {
                return Err(SystemBootAdmissionError::Admission {
                    package: name,
                    source,
                })
            },
        };

        if candidate.manifest().manifest().app.distribution != AppManifestDistribution::System {
            return Err(SystemBootAdmissionError::NotSystemClass { package: name });
        }

        if admitted.iter().any(|entry| entry.package_dir == name) {
            return Err(SystemBootAdmissionError::DuplicatePackage { package: name });
        }

        let seed_digest = candidate.bundle_digest().clone();
        admitted.push(TrustedSystemPackageAdmission {
            package_dir: name,
            candidate,
            provenance: SystemSeedProvenance { seed_digest },
        });
    }

    Ok(admitted)
}

/// List the seed root's immediate entry names, bounded and symlink-refusing.
///
/// `DirEntry::file_type` does not follow symlinks on Unix, so a symlinked seed
/// entry reports as a symlink and is skipped here rather than traversed. The
/// package directory beneath it is opened by [`admit_package_directory`],
/// which walks every path component with `O_NOFOLLOW` — that walker is the
/// real symlink defence and it is already audited, so this function
/// deliberately does not hand-roll a second one.
fn read_seed_entry_names(seed_root: &Path) -> Result<Vec<String>, SystemBootAdmissionError> {
    let entries = match std::fs::read_dir(seed_root) {
        Ok(entries) => entries,
        // A deployment with no seed root admits no system packages. That is a
        // legitimate configuration (a bare runtime store), not a failure.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(SystemBootAdmissionError::SeedUnavailable(error.to_string())),
    };

    let mut names = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| SystemBootAdmissionError::SeedUnavailable(error.to_string()))?;
        if names.len() >= MAX_SEED_ENTRIES {
            return Err(SystemBootAdmissionError::SeedTooLarge);
        }
        let file_type = entry
            .file_type()
            .map_err(|error| SystemBootAdmissionError::SeedUnavailable(error.to_string()))?;
        if !file_type.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        names.push(name);
    }
    names.sort();
    Ok(names)
}

/// A digest over the whole admitted inventory.
///
/// The surfacing increment's widget runtime takes exactly this: an
/// `inventory_digest` that pins *the set* of trusted packages, not just one
/// package's bytes. The distinction matters because system slot defaults and
/// widget authority are properties of the deployment's inventory as a whole —
/// adding or removing a system package changes what the host is willing to
/// render, so it must change this digest.
///
/// Computed over the sorted `(package_dir, seed_digest)` pairs, so it is
/// stable across boots and independent of directory-read order.
pub fn inventory_digest(inventory: &[TrustedSystemPackageAdmission]) -> AppDigest {
    let mut pairs: Vec<(&str, &str)> = inventory
        .iter()
        .map(|entry| {
            (
                entry.package_dir.as_str(),
                entry.provenance.seed_digest.as_str(),
            )
        })
        .collect();
    pairs.sort_unstable();
    let mut bytes = Vec::new();
    for (dir, digest) in pairs {
        bytes.extend_from_slice(dir.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(digest.as_bytes());
        bytes.push(0);
    }
    AppDigest::blake3(&bytes)
}

/// The set of package bundles that earned system class this boot, carried
/// together with the digest that pins the set itself.
///
/// This is the only thing the widget runtime accepts in exchange for
/// trusted-system provenance. Its fields are private to this module, so —
/// exactly like [`SystemSeedProvenance`] — nothing else in the crate can mint
/// one. A pin can only come out of a real seed-root resolution.
///
/// Unlike `SystemSeedProvenance` this one *is* `Clone`. It is not a one-shot
/// authority to publish specific bytes; it is a standing statement about what
/// the deployment ships, and the runtime keeps its own copy of it for the life
/// of the process.
#[derive(Clone, Debug)]
pub struct TrustedSystemInventoryPin {
    inventory_digest: AppDigest,
    seed_digests: BTreeSet<AppDigest>,
}

/// Move-only authority to mint a host grantor for one exact installation.
///
/// The type is crate-visible so the authority kernel can consume it, while its
/// field is private to this module. Consequently only a successful report from
/// the real seed-root admission path can create one.
pub(crate) struct TrustedSystemPackageHostGrant {
    installation_id: AppInstallationId,
}

impl TrustedSystemPackageHostGrant {
    pub(crate) fn into_installation_id(self) -> AppInstallationId {
        self.installation_id
    }
}

impl TrustedSystemInventoryPin {
    fn from_inventory(inventory: &[TrustedSystemPackageAdmission]) -> Self {
        Self {
            inventory_digest: inventory_digest(inventory),
            seed_digests: inventory
                .iter()
                .map(|entry| entry.provenance.seed_digest.clone())
                .collect(),
        }
    }

    pub(crate) fn inventory_digest(&self) -> &AppDigest {
        &self.inventory_digest
    }

    /// Whether these exact bytes were admitted out of the seed root this boot.
    ///
    /// Digest equality is the whole test. Provenance is bound to bytes, never
    /// to a package name or an installation id, so bytes that were renamed,
    /// re-uploaded or reinstalled under another id still get the answer their
    /// content deserves. And because the resolver refuses a seed package that
    /// does not declare `distribution: system`, every digest in here belongs to
    /// a system-class bundle: an installable manifest could only appear by
    /// being byte-identical to one, which its own distribution line prevents.
    pub(crate) fn admits(&self, package_content_digest: &AppDigest) -> bool {
        self.seed_digests.contains(package_content_digest)
    }

    /// How many distinct bundles the pin admits. Startup logs this beside the
    /// digest, which is otherwise an opaque 64 characters.
    pub fn admitted_bundles(&self) -> usize {
        self.seed_digests.len()
    }
}

/// What one seed package did at boot.
#[derive(Debug)]
pub struct SystemPackageAdmissionOutcome {
    pub package_dir: String,
    pub seed_digest: AppDigest,
    pub result: Result<AppCandidatePublicationReceipt, String>,
}

impl SystemPackageAdmissionOutcome {
    pub fn succeeded(&self) -> bool {
        self.result.is_ok()
    }
}

/// Admit every system package in the deployment's seed root for one scope.
///
/// Each package is published independently and a failure is recorded rather
/// than propagated: one malformed seed package must not cost the deployment
/// every other one, and a boot path that returns `Err` on the first problem
/// would do exactly that. The caller decides what an unsuccessful outcome
/// means — startup logs it; the pinning test asserts on it.
///
/// Publication is idempotent by content: republishing identical bytes yields
/// `AlreadyPresent` rather than a second installation, so this is safe to run
/// on every boot.
///
/// **What this does not do.** This resolver publishes inert
/// `ready_for_review` installations. It grants nothing by itself. The returned
/// report can later mint a package-bound host grantor for a successful outcome;
/// `AppPlatformApi` uses that only when `enable_at_boot` is explicitly enabled.
pub async fn admit_system_packages(
    publisher: &AppCandidatePublicationService,
    workspace: &ArtifactV2Workspace,
    authenticated: &AuthenticatedAppScope,
    now: DateTime<Utc>,
) -> Result<SystemBootAdmissionReport, SystemBootAdmissionError> {
    let inventory = resolve_system_package_inventory(workspace)?;
    // Computed over the whole inventory BEFORE any publication, so a package
    // that fails to publish still counts toward what the deployment shipped.
    // An inventory digest that silently shrank on a partial failure would let
    // a broken boot mint surfacing authority for a different-looking fleet.
    let trusted_inventory = TrustedSystemInventoryPin::from_inventory(&inventory);
    let mut outcomes = Vec::with_capacity(inventory.len());
    for admission in inventory {
        let package_dir = admission.package_dir().to_owned();
        let seed_digest = admission.seed_digest().clone();
        let result = publisher
            .publish_trusted_system_package(authenticated, admission, now)
            .await
            .map_err(|error| error.to_string());
        outcomes.push(SystemPackageAdmissionOutcome {
            package_dir,
            seed_digest,
            result,
        });
    }
    Ok(SystemBootAdmissionReport {
        admission_worker: authenticated.clone(),
        trusted_inventory,
        outcomes,
    })
}

/// The whole boot admission pass.
#[derive(Debug)]
pub struct SystemBootAdmissionReport {
    /// The exact short-lived worker that produced this report. Retaining it
    /// prevents a report for one scope/run from being paired with a different
    /// worker merely because deterministic installation ids match.
    admission_worker: AuthenticatedAppScope,
    /// Private so the pin cannot be assembled by a caller that never resolved
    /// a seed root — the report is the only way out of this module, and a
    /// forgeable one would hand surfacing authority to anything that can name
    /// a digest.
    trusted_inventory: TrustedSystemInventoryPin,
    pub outcomes: Vec<SystemPackageAdmissionOutcome>,
}

impl SystemBootAdmissionReport {
    /// Pins the set of system packages this deployment ships.
    pub fn inventory_digest(&self) -> &AppDigest {
        self.trusted_inventory.inventory_digest()
    }

    /// The proof the widget runtime needs before a `distribution: system`
    /// manifest can compile its widgets. Hand it to
    /// `AppWidgetRuntime::adopt_trusted_system_inventory`; until something
    /// does, every system manifest refuses to compile — which is the
    /// fail-closed default, not a defect.
    pub fn trusted_inventory(&self) -> &TrustedSystemInventoryPin {
        &self.trusted_inventory
    }

    pub fn admitted(&self) -> usize {
        self.outcomes.iter().filter(|o| o.succeeded()).count()
    }

    pub fn failures(&self) -> impl Iterator<Item = (&str, &str)> {
        self.outcomes
            .iter()
            .filter_map(|outcome| match &outcome.result {
                Err(error) => Some((outcome.package_dir.as_str(), error.as_str())),
                Ok(_) => None,
            })
    }

    /// Mint the narrowly scoped host identity that may approve one package
    /// this report actually published from the trusted system seed.
    pub fn host_grantor_scope(
        &self,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<AuthenticatedAppScope, SystemBootAdmissionError> {
        if self.admission_worker.authentication() != AppScopeAuthentication::SystemWorker {
            return Err(SystemBootAdmissionError::InvalidHostGrantor);
        }
        self.admission_worker.ensure_live_at(&now)?;
        let admitted = self.outcomes.iter().any(
            |outcome| matches!(&outcome.result, Ok(receipt) if &receipt.installation_id == installation_id),
        );
        if !admitted {
            return Err(
                SystemBootAdmissionError::InstallationNotAdmittedForHostGrant {
                    installation_id: installation_id.clone(),
                },
            );
        }
        AuthenticatedAppScope::from_trusted_system_package_host(
            &self.admission_worker,
            TrustedSystemPackageHostGrant {
                installation_id: installation_id.clone(),
            },
            now,
        )
        .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The repo seed root, which is what a dev deployment resolves.
    fn repo_seed_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3/system")
    }

    /// The five system packages the platform ships must all resolve.
    ///
    /// This is the test that would have caught the platform's central gap: the
    /// packages parsed in isolation for a year while nothing could admit them.
    #[test]
    fn every_shipped_system_package_resolves_from_the_seed_root() {
        let seed = repo_seed_root();
        let names = read_seed_entry_names(&seed).expect("seed root is readable");
        assert!(
            names.contains(&"meetings".to_owned()),
            "meetings package missing from {names:?}"
        );
        assert!(names.contains(&"town_square".to_owned()));
        assert!(names.contains(&"claims_review".to_owned()));
        assert!(names.contains(&"learning".to_owned()));
    }

    /// The 2026-09-08 host-read and reviewed-processing changes require new locks.
    /// A package version seals its dependency lock, so leaving even one seed
    /// on its prior immutable version makes that package fail every boot in
    /// every persisted scope. Pin the coordinated re-lock as one inventory
    /// assertion rather than allowing a partial bump to look successful.
    #[test]
    fn shipped_system_packages_carry_the_current_primitive_relock_versions() {
        let inventory =
            resolve_system_package_inventory_from(&repo_seed_root()).expect("inventory resolves");
        let versions = inventory
            .iter()
            .map(|admission| {
                (
                    admission.package_dir(),
                    admission.candidate().manifest().manifest().version.as_str(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            versions,
            std::collections::BTreeMap::from([
                ("claims_review", "0.2.11"),
                ("learning", "0.2.16"),
                ("meetings", "0.1.9"),
                ("thinking_map", "0.2.15"),
                ("town_square", "0.1.33"),
            ])
        );
    }

    /// A seed entry that is not an app package is skipped, not refused.
    ///
    /// The seed root also holds agent templates, db templates and trust
    /// policies. Treating those as malformed packages would make boot
    /// admission fail on a correct deployment.
    #[test]
    fn non_package_seed_neighbours_are_skipped() {
        let seed = repo_seed_root();
        let names = read_seed_entry_names(&seed).expect("seed root is readable");
        assert!(
            names.contains(&"agent_templates".to_owned()),
            "expected the non-package neighbour to be listed"
        );
        assert!(
            !seed.join("agent_templates").join(PACKAGE_SUBDIR).is_dir(),
            "agent_templates must not look like a package or this test proves nothing"
        );
    }

    /// A missing seed root admits nothing rather than failing the boot.
    #[test]
    fn an_absent_seed_root_yields_an_empty_inventory() {
        let names = read_seed_entry_names(Path::new("/nonexistent-seed-root-for-tests"))
            .expect("an absent seed root is a legitimate configuration");
        assert!(names.is_empty());
    }

    /// Every resolved package really is system class.
    ///
    /// Drives the production resolver rather than reimplementing its walk, so
    /// the test cannot pass while the real path fails — which is exactly what
    /// happened when this test opened package directories itself.
    #[test]
    fn resolved_packages_are_all_system_class() {
        let inventory =
            resolve_system_package_inventory_from(&repo_seed_root()).expect("inventory resolves");
        assert!(!inventory.is_empty(), "expected shipped system packages");
        for admission in &inventory {
            assert_eq!(
                admission.candidate().manifest().manifest().app.distribution,
                AppManifestDistribution::System,
                "seed package `{}` must declare distribution: system",
                admission.package_dir()
            );
        }
    }

    /// No two shipped widgets pin the same slot default.
    ///
    /// A slot id is page-qualified, so `page: /, slot: ambient` is one slot
    /// for the whole deployment. Two trusted-system claimants on it are
    /// *contested*, and `TrustedSystemSlotDefaultSet::from_boot_admission`
    /// drops a contested slot rather than refusing the set — correct, because
    /// one authoring conflict must not blank every other pinned default, but
    /// it means a second claimant silently costs the deployment the slot
    /// itself in every scope on every boot. `meetings` and `town_square` both
    /// shipped `page: /, slot: ambient, system_default: true`, so the home
    /// ambient default could not be pinned anywhere, and nothing failed: the
    /// only signal was one `warn!` line in a boot log.
    ///
    /// Two claimants inside a *single* package are worse still — that path
    /// returns `Err` and takes the whole boot's default set with it — so both
    /// shapes are caught here by keying claimants on package *and* widget.
    ///
    /// The bytes are what can regress, so the bytes are what this reads. A
    /// fixture would keep passing after a seed manifest reintroduced the
    /// conflict, which is exactly the failure this exists to catch.
    #[test]
    fn no_two_shipped_system_widgets_pin_the_same_slot_default() {
        use std::collections::BTreeMap;

        let inventory =
            resolve_system_package_inventory_from(&repo_seed_root()).expect("inventory resolves");
        let mut claims: BTreeMap<(&str, &str), Vec<String>> = BTreeMap::new();
        for admission in &inventory {
            for widget in &admission.candidate().manifest().manifest().app.widgets {
                for slot in widget
                    .suggested_slots
                    .iter()
                    .filter(|slot| slot.system_default)
                {
                    claims
                        .entry((slot.page.as_str(), slot.slot.as_str()))
                        .or_default()
                        .push(format!("{}/{}", admission.package_dir(), widget.id));
                }
            }
        }
        assert!(
            !claims.is_empty(),
            "no shipped package pins a slot default, so this test proves nothing"
        );
        let contested = claims
            .iter()
            .filter(|(_, claimants)| claimants.len() > 1)
            .collect::<Vec<_>>();
        assert!(
            contested.is_empty(),
            "shipped system widgets contest a slot default, which pins it to nobody: {contested:?}"
        );
    }

    /// Every shipped package still passes the gate boot admission runs.
    ///
    /// `.magician/app-derived.json` and `sdk/app.generated.ts` are rendered
    /// FROM the manifest, and admission re-renders and byte-compares them
    /// (`prepare_candidate` → `verify_provider_free_candidate` →
    /// `ensure_generated_current`). So editing a seed `SKILL.md` without
    /// regenerating them fails at boot, not at authoring time, and it fails
    /// quietly: the package's outcome is `Err`, and
    /// `resolve_admitted_slot_defaults` refuses a scope in which ANY resolved
    /// package failed to realize, so one stale digest blanks the pinned slot
    /// defaults of every OTHER package too. `town_square` shipped exactly
    /// that — a digest of a manifest that still carried `system_default:
    /// true` on `page: /, slot: ambient` after the line was removed to settle
    /// the contest the test above pins.
    ///
    /// Reads the real seed bytes through the real gate. A fixture package
    /// would keep passing across precisely the edit this exists to catch.
    #[test]
    fn shipped_system_packages_pass_boot_admission_conformance() {
        use crate::magician_v2::apps::authoring::verify_provider_free_candidate;

        let inventory =
            resolve_system_package_inventory_from(&repo_seed_root()).expect("inventory resolves");
        assert!(!inventory.is_empty(), "expected shipped system packages");
        let refused = inventory
            .iter()
            .filter_map(|admission| {
                verify_provider_free_candidate(admission.candidate())
                    .err()
                    .map(|error| format!("{}: {error}", admission.package_dir()))
            })
            .collect::<Vec<_>>();
        assert!(
            refused.is_empty(),
            "seed packages fail the conformance gate boot admission runs, so they admit nothing \
             and take every scope's slot defaults with them: {refused:?}"
        );
    }

    /// A seed root reached through `..` still resolves.
    ///
    /// Regression. `admit_package_directory` refuses any path holding a `.` or
    /// `..` component, so before the root was canonicalized every package under
    /// a relatively-addressed seed root was refused with "path contains dot or
    /// parent traversal" — which is to say boot admission admitted nothing at
    /// all on a deployment whose data directory is a symlink or a relative
    /// path. Both are ordinary.
    #[test]
    fn a_seed_root_reached_through_parent_traversal_still_resolves() {
        let traversed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3/system");
        assert!(
            traversed
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "this test is meaningless unless the path really contains `..`"
        );
        let inventory =
            resolve_system_package_inventory_from(&traversed).expect("traversed seed resolves");
        assert!(
            !inventory.is_empty(),
            "a `..`-addressed seed root must still admit the shipped packages"
        );
    }

    /// The inventory pin admits exactly the bundles the seed root shipped.
    ///
    /// Membership is by digest, so this also pins the negative: bytes that were
    /// never resolved out of the seed root are not in the set no matter what
    /// they call themselves.
    #[test]
    fn the_inventory_pin_admits_exactly_the_seed_bundles() {
        let inventory =
            resolve_system_package_inventory_from(&repo_seed_root()).expect("inventory resolves");
        assert!(!inventory.is_empty(), "expected shipped system packages");
        let pin = TrustedSystemInventoryPin::from_inventory(&inventory);
        assert_eq!(pin.inventory_digest(), &inventory_digest(&inventory));
        for admission in &inventory {
            assert!(
                pin.admits(admission.seed_digest()),
                "`{}` was admitted but its bytes are not pinned",
                admission.package_dir()
            );
        }
        assert!(
            !pin.admits(&AppDigest::blake3(
                b"bytes that never came from the seed root"
            )),
            "the pin must not admit bytes it never saw"
        );
    }

    /// The pin is what turns a system manifest from *refused* into *compiled*.
    ///
    /// This is the gap S1 names, asserted against the bytes the deployment
    /// actually ships rather than a fixture. Before the pin reached the widget
    /// runtime, `compile_native_manifest_widgets` refused every one of these
    /// manifests for its distribution class alone, so not one system package
    /// could surface anything.
    ///
    /// Refusals for *other* reasons are left alone deliberately: a manifest
    /// whose declarations the V1 compiler does not accept is a different gap,
    /// and folding it in here would make this test stop measuring provenance.
    #[test]
    fn a_pinned_inventory_is_what_lets_a_system_manifest_compile_its_widgets() {
        use crate::magician_v2::apps::models::{AppInstallationId, AppReference};
        use crate::magician_v2::apps::widget_runtime::{
            compile_native_manifest_widgets, trusted_system_provenance, AppWidgetRuntimeError,
            CompiledAppInstallationProvenance,
        };

        const CLASS_REFUSAL: &str = "a system manifest requires digest-pinned host provenance";

        let inventory =
            resolve_system_package_inventory_from(&repo_seed_root()).expect("inventory resolves");
        assert!(!inventory.is_empty(), "expected shipped system packages");
        let pin = TrustedSystemInventoryPin::from_inventory(&inventory);
        let installation_id =
            AppInstallationId::parse("installation-system-boot-pin").expect("installation id");
        let package_revision_ref =
            AppReference::parse("package-revision:system-boot-pin").expect("package revision");

        let mut compiled_widgets = 0usize;
        for admission in &inventory {
            let manifest = admission.candidate().manifest().manifest();
            let unpinned = compile_native_manifest_widgets(
                manifest,
                installation_id.clone(),
                1,
                package_revision_ref.clone(),
                admission.seed_digest().clone(),
                CompiledAppInstallationProvenance::installable(),
            );
            assert!(
                matches!(
                    unpinned,
                    Err(AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
                        CLASS_REFUSAL
                    ))
                ),
                "`{}` must stay refused without host provenance",
                admission.package_dir()
            );

            let provenance = trusted_system_provenance(Some(&pin), admission.seed_digest());
            assert_eq!(
                provenance,
                CompiledAppInstallationProvenance::trusted_system(pin.inventory_digest().clone()),
                "`{}` earned its provenance from the wrong inventory",
                admission.package_dir()
            );
            match compile_native_manifest_widgets(
                manifest,
                installation_id.clone(),
                1,
                package_revision_ref.clone(),
                admission.seed_digest().clone(),
                provenance,
            ) {
                Ok(plan) => compiled_widgets += plan.widgets.len(),
                Err(error) => assert!(
                    !matches!(
                        error,
                        AppWidgetRuntimeError::UnsupportedCompiledDeclaration(CLASS_REFUSAL)
                    ),
                    "`{}` is still refused for its distribution class with the pin in hand",
                    admission.package_dir()
                ),
            }
        }
        assert!(
            compiled_widgets > 0,
            "no shipped system package compiled a widget, so nothing can surface"
        );
    }

    /// The provenance witness is bound to exact bytes.
    ///
    /// This is what "digest-pinned" buys: an admission's proof names the
    /// bundle it was minted for, so it cannot be carried to other bytes.
    #[test]
    fn provenance_pins_the_admitted_bytes() {
        let inventory =
            resolve_system_package_inventory_from(&repo_seed_root()).expect("inventory resolves");
        assert!(!inventory.is_empty(), "expected shipped system packages");
        for admission in &inventory {
            assert_eq!(
                admission.seed_digest(),
                admission.candidate().bundle_digest(),
                "provenance digest must be the admitted bundle digest"
            );
        }
    }
}
