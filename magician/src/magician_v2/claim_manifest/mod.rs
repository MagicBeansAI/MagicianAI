//! Which claims an artifact revision carries — Composable Work Modules,
//! Module B: *"produce a persuasive artifact from a claim set, with
//! provenance"*. This manifest is the provenance half: the single
//! authoritative copy of what a built artifact claims, on what evidence.
//!
//! # The boundary with the sent-record
//!
//! [`crate::magician_v2::evidence::outward_assertions`] answers what was
//! **sent** — that a claim was asserted, to these people, through this exact
//! payload. Its own motivation names *"the decks that show it, or the drafts
//! still carrying it"*, and a draft is exactly what a sent-record cannot find,
//! because nothing has been sent yet. This store answers what was **built**:
//! when a claim is corrected, the sent-record finds the emails that stated it,
//! and [`ClaimManifestStore::revisions_carrying`] finds the decks and drafts
//! that still embed it.
//!
//! Consumers *"consume; never own"*: a flow that wants to know what a revision
//! claims asks here rather than keeping a copy that can drift.
//!
//! # What this module refuses to hold, and why
//!
//! - **No audience.** Who an artifact is *for* is a fact about a send, and the
//!   sent-record owns it. No relationship is bound here, so no
//!   [`AudienceRef`](crate::magician_v2::audience::AudienceRef) appears — a
//!   manifest that named an audience would go stale the moment the artifact
//!   was shown to anyone else.
//! - **No claim or evidence content.** Refs only, supplied by the caller. This
//!   store reads no claim store, no evidence store and no artifact store, so
//!   any flow that produces an artifact can bind a manifest without inheriting
//!   those subsystems. Loose coupling is the point.
//! - **No unbind, supersede or delete.** A revision's claim set is immutable:
//!   revisions exist precisely so content cannot change under a reference.
//!   The correction path is a new revision whose manifest drops the claim —
//!   and [`ClaimManifestStore::latest_revision_carrying`] is how that fix
//!   becomes visible.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[cfg(test)]
mod tests;

const FIELD_SEP: char = '\u{1f}';

/// Scope for a store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimManifestScope {
    pub principal: String,
    pub workspace: String,
}

impl ClaimManifestScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// One claim an artifact revision carries, and the evidence it cites.
///
/// A binding with no evidence refs is refused at [`ClaimManifestStore::bind`]:
/// *"never answer from nothing"* applies to decks as much as forms — a claim
/// in an artifact that cannot cite evidence is an invented figure with a slide
/// layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimBinding {
    pub claim_ref: String,
    /// Stored canonical: trimmed, deduplicated, sorted. Canonical form is what
    /// makes a rebind order-insensitive — the same set always compares equal.
    pub evidence_refs: Vec<String>,
}

/// What one artifact revision claims: the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub manifest_id: String,
    pub artifact_ref: String,
    pub revision_ref: String,
    /// Stored canonical: sorted by claim ref, each binding's evidence refs
    /// deduplicated and sorted. One binding per claim — two bindings for the
    /// same claim would make "what evidence backs this claim here" ambiguous,
    /// so the write refuses them.
    pub claims: Vec<ClaimBinding>,
    /// Who vouched for this claim set. A principal or agent name, not an
    /// audience: no relationship is being bound, only authorship recorded.
    pub bound_by: String,
    pub bound_at: DateTime<Utc>,
}

impl Manifest {
    /// The binding for one claim, if this revision carries it.
    pub fn carrying(&self, claim_ref: &str) -> Option<&ClaimBinding> {
        self.claims
            .iter()
            .find(|binding| binding.claim_ref == claim_ref)
    }
}

/// One built revision still carrying a claim — a row of the
/// correction-propagation answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarryingRevision {
    pub artifact_ref: String,
    pub revision_ref: String,
    pub bound_at: DateTime<Utc>,
    /// What the carrying revision cited for the claim — so whoever propagates
    /// a correction can see whether the fix invalidates the citation too.
    pub evidence_refs: Vec<String>,
}

/// The built-artifact provenance record.
///
/// Append-only, one JSONL per artifact ref: *"the revision history of what it
/// claimed"* is one read rather than a scan. A reverse index per claim ref
/// serves the correction-propagation query the same way.
#[derive(Debug, Clone)]
pub struct ClaimManifestStore {
    workspace_layout: ArtifactV2Workspace,
}

impl ClaimManifestStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn root(&self, scope: &ClaimManifestScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("claim_manifest")
    }

    fn manifest_path(&self, scope: &ClaimManifestScope, artifact_ref: &str) -> PathBuf {
        self.root(scope)
            .join("artifacts")
            .join(format!("{}.jsonl", stable_id(artifact_ref)))
    }

    fn claim_index_path(&self, scope: &ClaimManifestScope, claim_ref: &str) -> PathBuf {
        self.root(scope)
            .join("index")
            .join("claim")
            .join(format!("{}.jsonl", stable_id(claim_ref)))
    }

    /// Bind a revision's claim set — the one write this store has.
    ///
    /// **A revision's claim set is immutable.** Rebinding the same
    /// `(artifact, revision)` with the identical claims — order-insensitive,
    /// because the set is canonicalised before comparing — is an idempotent
    /// resume: the original record stands, including who bound it and when, so
    /// a retried flow does not move the provenance it already wrote. Rebinding
    /// with a *different* claim set is refused: revisions exist precisely so
    /// content cannot change under a reference, and the fix is a new revision.
    ///
    /// Refused outright:
    /// - empty claims — a manifest that binds nothing is decoration;
    /// - a binding with no evidence refs — a claim in an artifact that cannot
    ///   cite evidence is an invented figure with a slide layout;
    /// - blank artifact/revision refs and an unnamed binder — provenance
    ///   nothing can look up, or that cannot answer who vouched, is not
    ///   provenance.
    pub fn bind(
        &self,
        scope: &ClaimManifestScope,
        artifact_ref: &str,
        revision_ref: &str,
        claims: Vec<ClaimBinding>,
        bound_by: &str,
        now: DateTime<Utc>,
    ) -> Result<Manifest> {
        let artifact_ref = artifact_ref.trim();
        let revision_ref = revision_ref.trim();
        let bound_by = bound_by.trim();
        if artifact_ref.is_empty() || revision_ref.is_empty() {
            anyhow::bail!(
                "a manifest must name the artifact and revision it describes: a blank \
                 reference is provenance nothing can ever look up, which defeats the record"
            );
        }
        if bound_by.is_empty() {
            anyhow::bail!(
                "a manifest must say who bound it: provenance that cannot answer who vouched \
                 for these claims is not provenance"
            );
        }
        let claims = canonicalise_claims(claims)?;

        if let Some(existing) = self.manifest_for(scope, artifact_ref, revision_ref)? {
            if existing.claims == claims {
                return Ok(existing);
            }
            anyhow::bail!(
                "revision `{revision_ref}` of `{artifact_ref}` already carries a different \
                 claim set; revisions exist precisely so content cannot change under a \
                 reference — rewriting this one would let a deck's provenance change after \
                 the deck was cited. Bind a new revision instead."
            );
        }

        let manifest = Manifest {
            manifest_id: derive_manifest_id(scope, artifact_ref, revision_ref),
            artifact_ref: artifact_ref.to_string(),
            revision_ref: revision_ref.to_string(),
            claims,
            bound_by: bound_by.to_string(),
            bound_at: now,
        };

        // Index BEFORE row: "the row exists" must imply "the index exists". A
        // manifest the correction query cannot find is a deck nobody fixes, so
        // a crash between the two writes must leave a dangling index entry —
        // harmless, it resolves to nothing on read — rather than an unindexed
        // manifest.
        for binding in &manifest.claims {
            self.append_claim_index(scope, &binding.claim_ref, artifact_ref)?;
        }
        self.append_manifest(scope, &manifest)?;
        Ok(manifest)
    }

    /// The manifest of one revision, if it was ever bound.
    pub fn manifest_for(
        &self,
        scope: &ClaimManifestScope,
        artifact_ref: &str,
        revision_ref: &str,
    ) -> Result<Option<Manifest>> {
        let manifest_id = derive_manifest_id(scope, artifact_ref.trim(), revision_ref.trim());
        Ok(self
            .manifests_for_artifact(scope, artifact_ref)?
            .into_iter()
            .find(|manifest| manifest.manifest_id == manifest_id))
    }

    /// Every manifest ever bound for an artifact, oldest first — the revision
    /// history of what it claimed.
    ///
    /// The log is append-only, so file order IS bind order; the fold only
    /// deduplicates. **First bind wins, defensively in the fold as well as at
    /// the write**: a duplicate line — a torn concurrent bind, a replay from an
    /// older binary — must not change what a revision claims.
    ///
    /// Read faults keep their two meanings apart, per
    /// [`crate::magician_v2::jsonl`]: a torn FINAL line is an append that
    /// never completed, so the manifest it recorded was never bound and the
    /// fold reads past it; an unreadable file or a torn INTERIOR line is an
    /// error, never an empty history — an invented blank history would let
    /// [`ClaimManifestStore::bind`]'s immutability refusal pass vacuously and
    /// the correction query answer "nothing carries it" from a disk fault.
    pub fn manifests_for_artifact(
        &self,
        scope: &ClaimManifestScope,
        artifact_ref: &str,
    ) -> Result<Vec<Manifest>> {
        let path = self.manifest_path(scope, artifact_ref.trim());
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for manifest in crate::magician_v2::jsonl::parse_log_lines::<Manifest>(&raw, &path)? {
            if seen.insert(manifest.manifest_id.clone()) {
                out.push(manifest);
            }
        }
        Ok(out)
    }

    /// Every built revision that carries a claim — THE correction-propagation
    /// query. When a claim is corrected, these are the decks and drafts still
    /// carrying it, which is exactly what the sent-record cannot find.
    ///
    /// Served by the per-claim reverse index, not a scan — a scan would be
    /// correct and would also be the thing nobody runs. Each indexed artifact's
    /// log is read once. A dangling index entry — a bind that crashed after the
    /// index write, before the row — resolves to nothing: index-before-row
    /// means the index may over-promise, never under-deliver.
    ///
    /// Sorted by `(artifact_ref, bound_at, revision_ref)` so the answer is
    /// deterministic whatever order things were bound or indexed in.
    pub fn revisions_carrying(
        &self,
        scope: &ClaimManifestScope,
        claim_ref: &str,
    ) -> Result<Vec<CarryingRevision>> {
        let claim_ref = claim_ref.trim();
        let mut out = Vec::new();
        for artifact_ref in self.indexed_artifacts(scope, claim_ref)? {
            for manifest in self.manifests_for_artifact(scope, &artifact_ref)? {
                if let Some(binding) = manifest.carrying(claim_ref) {
                    out.push(CarryingRevision {
                        artifact_ref: manifest.artifact_ref.clone(),
                        revision_ref: manifest.revision_ref.clone(),
                        bound_at: manifest.bound_at,
                        evidence_refs: binding.evidence_refs.clone(),
                    });
                }
            }
        }
        out.sort_by(|left, right| {
            left.artifact_ref
                .cmp(&right.artifact_ref)
                .then_with(|| left.bound_at.cmp(&right.bound_at))
                .then_with(|| left.revision_ref.cmp(&right.revision_ref))
        });
        Ok(out)
    }

    /// The artifacts whose LATEST bound revision still carries a claim, sorted.
    ///
    /// *"Still carrying it now"* and *"carried it once"* are different
    /// questions: a corrected deck whose new revision dropped the claim must
    /// NOT be flagged — that is the fixed case, and flagging it would teach
    /// people the list is noise. An artifact whose newest revision gained the
    /// claim IS flagged, however its history started.
    ///
    /// An indexed artifact with no manifest rows (a bind torn between index and
    /// row) contributes nothing: nothing was authoritatively built, so there is
    /// nothing to fix — the retried bind will complete the record.
    ///
    /// **"Latest" is the last-APPENDED manifest — bind order, not revision
    /// order.** Revision refs are opaque, so the store cannot order them and
    /// trusts that revisions are bound in the order they are built.
    /// Backfilling an older revision's manifest AFTER a newer one was bound
    /// makes the backfill read as the artifact's current claim set here, even
    /// though [`ClaimManifestStore::revisions_carrying`] orders the same rows
    /// by `bound_at`.
    pub fn latest_revision_carrying(
        &self,
        scope: &ClaimManifestScope,
        claim_ref: &str,
    ) -> Result<Vec<String>> {
        let claim_ref = claim_ref.trim();
        let mut out = Vec::new();
        for artifact_ref in self.indexed_artifacts(scope, claim_ref)? {
            let manifests = self.manifests_for_artifact(scope, &artifact_ref)?;
            // Oldest first, so the last manifest is the artifact's current
            // claim set.
            let Some(latest) = manifests.last() else {
                continue;
            };
            if latest.carrying(claim_ref).is_some() {
                out.push(latest.artifact_ref.clone());
            }
        }
        // Already unique — the index read deduplicates artifacts — so sorting
        // alone makes the output deterministic.
        out.sort();
        Ok(out)
    }

    // ── Internals ───────────────────────────────────────────────────────────

    /// The artifacts a claim's index names, deduplicated preserving FIRST-SEEN
    /// order.
    ///
    /// `Vec::dedup` removes only CONSECUTIVE duplicates, and this file
    /// interleaves artifacts freely — one artifact's later revision lands after
    /// other artifacts' entries — so a set is the only correct dedupe.
    ///
    /// An unparseable INTERIOR entry FAILS the read rather than being
    /// skipped: a silently skipped entry is a deck the correction query
    /// misses without anyone knowing, which is the exact failure the index
    /// exists to prevent. The one exception is an unparseable FINAL line:
    /// `append_path_sync` writes each entry in a single call, so a torn tail
    /// is an index append that never completed — and index-before-row means
    /// the manifest it would have named was never written, so skipping it
    /// drops nothing findable.
    fn indexed_artifacts(
        &self,
        scope: &ClaimManifestScope,
        claim_ref: &str,
    ) -> Result<Vec<String>> {
        let path = self.claim_index_path(scope, claim_ref);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for artifact_ref in crate::magician_v2::jsonl::parse_log_lines::<String>(&raw, &path)? {
            if seen.insert(artifact_ref.clone()) {
                out.push(artifact_ref);
            }
        }
        Ok(out)
    }

    /// One index line: the artifact ref, JSON-encoded rather than raw so a ref
    /// containing a newline cannot shear the line format.
    fn append_claim_index(
        &self,
        scope: &ClaimManifestScope,
        claim_ref: &str,
        artifact_ref: &str,
    ) -> Result<()> {
        let path = self.claim_index_path(scope, claim_ref);
        let mut line = serde_json::to_vec(artifact_ref)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending index {}", path.display()))?;
        Ok(())
    }

    fn append_manifest(&self, scope: &ClaimManifestScope, manifest: &Manifest) -> Result<()> {
        let path = self.manifest_path(scope, &manifest.artifact_ref);
        let mut line = serde_json::to_vec(manifest)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    /// Read a log, distinguishing "absent" from "unreadable".
    ///
    /// Delegates to [`crate::magician_v2::jsonl::read_log_if_present`]: only
    /// a missing file reads as an empty store, and every other failure —
    /// EACCES, EIO, invalid UTF-8 from a torn write — propagates. The first
    /// cut mapped EVERY error to `None`, which failed OPEN: an unreadable log
    /// made bind's immutability refusal vacuous and turned the correction
    /// query's answer into a confident "no decks carry it".
    fn read_if_present(&self, path: &Path) -> Result<Option<String>> {
        crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// The id for one revision's manifest.
///
/// Derived, never allocated, so a retried bind RESUMES the record it already
/// wrote instead of duplicating it. The tuple:
///
/// - `scope.principal`, `scope.workspace` — two tenants binding the same
///   artifact ref must not resume each other's manifests;
/// - `artifact_ref` — which artifact this is the provenance of;
/// - `revision_ref` — which revision, because a claim set is a fact about ONE
///   immutable revision, not about the artifact over time.
///
/// The claims are deliberately NOT in the tuple: this manifest is the single
/// authoritative copy per revision, and hashing the claims in would let a
/// content-different rebind arrive as a quiet sibling record instead of being
/// refused — the exact mutation the immutability rule exists to prevent.
fn derive_manifest_id(
    scope: &ClaimManifestScope,
    artifact_ref: &str,
    revision_ref: &str,
) -> String {
    format!(
        "cmf-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal, scope.workspace, artifact_ref, revision_ref
        ))
    )
}

/// The canonical form of a claim set: bindings sorted by claim ref, each
/// binding's evidence refs trimmed, deduplicated and sorted. Canonical form is
/// what makes the immutability comparison order-insensitive — the same set
/// handed back in any order compares equal, so a retry resumes.
///
/// Refusals, each of which protects the record's meaning:
///
/// - **Empty claim set** — a manifest that binds nothing is decoration, and
///   accepting it would make "this revision has a manifest" stop meaning "its
///   claims are accounted for".
/// - **A binding with no evidence** — checked AFTER the per-ref loop, so a
///   list of blank refs cannot pass vacuously: "every evidence ref is valid"
///   over an empty or all-blank list must not count as citing evidence.
/// - **A blank claim or evidence ref** — a ref nothing can resolve grants the
///   look of provenance without the substance.
/// - **The same claim bound twice** — two bindings for one claim would make
///   "what evidence backs this claim here" ambiguous, and the ambiguity would
///   surface exactly when a correction is being propagated.
fn canonicalise_claims(claims: Vec<ClaimBinding>) -> Result<Vec<ClaimBinding>> {
    if claims.is_empty() {
        anyhow::bail!(
            "a manifest that binds nothing is decoration: refusing it keeps `this revision \
             has a manifest` meaning `its claims are accounted for`"
        );
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(claims.len());
    for binding in claims {
        let claim_ref = binding.claim_ref.trim().to_string();
        if claim_ref.is_empty() {
            anyhow::bail!(
                "a binding must name its claim: a blank claim ref is provenance that points \
                 at nothing, so a correction to the claim could never find this artifact"
            );
        }
        let mut evidence = BTreeSet::new();
        for evidence_ref in &binding.evidence_refs {
            let evidence_ref = evidence_ref.trim();
            if evidence_ref.is_empty() {
                anyhow::bail!(
                    "claim `{claim_ref}` cites a blank evidence ref: a citation that resolves \
                     to nothing is no citation, and letting it stand would let an unbacked \
                     claim pass as backed"
                );
            }
            evidence.insert(evidence_ref.to_string());
        }
        if evidence.is_empty() {
            anyhow::bail!(
                "claim `{claim_ref}` cites no evidence: a claim in an artifact that cannot \
                 cite evidence is an invented figure with a slide layout — never answer from \
                 nothing applies to decks as much as forms"
            );
        }
        if !seen.insert(claim_ref.clone()) {
            anyhow::bail!(
                "claim `{claim_ref}` is bound twice in one manifest: two bindings for one \
                 claim make `what evidence backs this claim here` ambiguous, exactly when a \
                 correction is being propagated"
            );
        }
        out.push(ClaimBinding {
            claim_ref,
            evidence_refs: evidence.into_iter().collect(),
        });
    }
    out.sort_by(|left, right| left.claim_ref.cmp(&right.claim_ref));
    Ok(out)
}
