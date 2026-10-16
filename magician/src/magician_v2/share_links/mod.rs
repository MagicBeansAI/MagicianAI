//! Capability grants for audience members — Deal Close plan phase 2, generalised.
//!
//! The plan's contract: *"One identity, one link, expiring, revocable, bound to
//! the engagement"* — with the binding widened the way the rest of the set was:
//! to an [`AudienceRef`] plus a caller-supplied `resource_ref`. A data room
//! today; any shared resource tomorrow. Nothing here knows or asks what the
//! resource is.
//!
//! # The secret is never stored
//!
//! The caller supplies the secret at issue time — entropy is the runtime's job,
//! not this module's — and only its blake3 hash is written. A store that holds
//! plaintext capabilities is a breach amplifier: one read of the log would mint
//! a working credential for every grant it governs. A hash can verify a
//! presented secret and can do nothing else with it.
//!
//! # Expiry is required
//!
//! [`ShareLink::expires_at`] is a plain timestamp, not an `Option`. A
//! possession-based grant that never lapses is the blanket-yes failure — the
//! URL outlives the relationship that justified it — so the type cannot express
//! "no expiry", and issuance refuses one that is not ahead of `now`. Expiry is
//! inclusive, like every expiry in this codebase: expiring at noon means
//! expired at noon. Whether a link is expired is derived from the clock at read
//! time, never stored — a link must read as dead even if nothing ran.
//!
//! # Possession, never proof of identity
//!
//! A presented secret proves possession of the link, and links get forwarded.
//! So a successful presentation reports the identity the link was **issued to**
//! and a **presentation number**: [`Presentation::issued_to`] feeds the access
//! log's `token_issued_to` unchanged, and [`Presentation::sequence`] numbers
//! this grant slot's presentations — first, second, nth. It is **not** the
//! access log's visit number: the reader surface presents the credential on
//! every request, because presenting it is the authorisation, and derives
//! `AccessEvent::sequence` from the access lane instead. A reader who opens a
//! room once and reads four documents spends five presentations here and is
//! one visit there.
//!
//! # Revocation is forward-only
//!
//! Revoking prevents future presentation. It does not recall anything already
//! fetched, and nothing here is described as if it could: the presentations a
//! link accumulated before it was killed stay on the record.
//!
//! # What this module refuses to do
//!
//! - **Generate secrets.** A module that both minted and verified credentials
//!   would concentrate exactly what should stay split across layers.
//! - **Read anything else.** The [`Audience`] is supplied at presentation time
//!   by whoever owns the relationship; there is no dependency on engagement
//!   stores, rosters, calendars or config, so any flow can gate any resource.
//! - **Express an open grant.** An audience is enumerable by construction —
//!   there is no public link and no "anyone with the URL", and the type keeps
//!   it that way.
//! - **Keep the access log.** It returns what the log needs — the issued-to
//!   identity and the presentation number — and the log owns everything else.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::{Audience, AudienceRef};

#[cfg(test)]
mod tests;

const FIELD_SEP: char = '\u{1f}';

/// Scope for a store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareLinkScope {
    pub principal: String,
    pub workspace: String,
}

impl ShareLinkScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// Where a link stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareLinkState {
    /// Unrevoked and not yet expired: the only state that presents.
    Live,
    /// The clock's verdict, derived at read time and never stored.
    Expired,
    /// The owner's verdict. It outranks the clock: an explicit kill is the
    /// fact worth surfacing over a deadline that happened to pass as well.
    Revoked,
}

impl ShareLinkState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }
}

/// One grant: a hashed credential that opens `resource_ref` for `issued_to`,
/// for as long as the expiry and the audience relationship both hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareLink {
    pub link_id: String,
    /// What the link opens. Caller-supplied and opaque here.
    pub resource_ref: String,
    /// The relationship the grant derives from. Access ends with it.
    pub audience: AudienceRef,
    /// The identity the link was **issued to** — never a claim about who
    /// presents it. Feeds the access log's `token_issued_to` unchanged.
    pub issued_to: String,
    /// blake3 of the secret. The plaintext never touches this store — see the
    /// module note on breach amplification.
    pub secret_hash: String,
    pub issued_at: DateTime<Utc>,
    /// Required, not optional: a possession-based grant that never lapses is
    /// the blanket-yes failure. Inclusive — expiring at noon means expired at
    /// noon.
    pub expires_at: DateTime<Utc>,
    /// When the owner killed it, if they did. Forward-only: it prevents future
    /// presentation and recalls nothing already fetched.
    pub revoked_at: Option<DateTime<Utc>>,
    /// The `secret_hash` of the credential this one superseded, present
    /// exactly when the credential arrived via [`ShareLinkStore::rotate`] —
    /// the explicit rotation evidence the completed-rotation replay resume
    /// requires. Rotation used to be inferred from a timestamp coincidence
    /// (predecessor `revoked_at` equal to successor `issued_at`), which a
    /// legitimate same-clock revoke-plus-issue forges. Serde-defaulted so
    /// records written before the field existed parse as non-rotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_of: Option<String>,
    /// Distinct presentations, folded from the log — the highest presentation
    /// number reached. Survives rotation, because the count belongs to the
    /// identity's grant slot, not to the secret in their hands.
    pub presentations: u32,
}

impl ShareLink {
    /// Where this stands at `now`.
    ///
    /// Derived from the clock at read time, never stored: an expired link must
    /// read as expired even if nothing ran. Revocation outranks expiry — the
    /// owner's explicit act is the fact worth surfacing over the clock's.
    pub fn state(&self, now: DateTime<Utc>) -> ShareLinkState {
        if self.revoked_at.is_some() {
            ShareLinkState::Revoked
        } else if now >= self.expires_at {
            ShareLinkState::Expired
        } else {
            ShareLinkState::Live
        }
    }

    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.state(now) == ShareLinkState::Live
    }
}

/// A successful presentation, shaped to feed the access log.
///
/// `issued_to` is the log's `token_issued_to` — possession, never proof of
/// identity. `sequence` numbers this grant slot's presentations — first or
/// nth — and is **not** the access log's visit number: the reader surface
/// presents the credential on every request and numbers the visit from the
/// access lane, so several presentations counted here can be one visit there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presentation {
    pub issued_to: String,
    pub sequence: u32,
}

/// Why a presentation was refused.
///
/// Every gate has its own reason because the **audit log** needs the honest
/// one: a credential the owner killed yesterday and a string that never was a
/// credential call for different responses. How much of the reason to reveal
/// to the presenter is the transport layer's decision — this module reports
/// the truth inward and takes no position on what goes outward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationRefused {
    /// No current credential on the resource carries this hash. A rotated-away
    /// secret lands here too: the credential it named has been superseded.
    UnknownSecret,
    /// The credential exists and the owner killed it.
    Revoked,
    /// Past its expiry — inclusively.
    Expired,
    /// The presented audience is not the relationship the grant was bound to.
    /// The same audience id under a different kind is a different relationship.
    AudienceMismatch,
    /// The relationship the grant derives from has ended. Access ends with it,
    /// even for an identity the roster still lists.
    AudienceEnded,
    /// The audience no longer names the identity the link was issued to. An
    /// audience naming nobody admits nobody — the empty roster fails closed
    /// rather than vacuously passing.
    IdentityNotAdmitted,
}

impl PresentationRefused {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownSecret => "unknown_secret",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::AudienceMismatch => "audience_mismatch",
            Self::AudienceEnded => "audience_ended",
            Self::IdentityNotAdmitted => "identity_not_admitted",
        }
    }
}

/// What a caller supplies to issue — or rotate in — a link. `link_id`, the
/// stored hash, the rotation lineage and the folded counters are the store's,
/// so a caller cannot hand in a pre-revoked or pre-presented grant, or forge
/// rotation evidence.
#[derive(Debug, Clone)]
pub struct IssueShareLink {
    pub resource_ref: String,
    pub audience: AudienceRef,
    pub issued_to: String,
    /// The plaintext secret — supplied, never generated here, and never
    /// stored: only its hash survives the call.
    pub secret: String,
    pub expires_at: DateTime<Utc>,
}

/// One line in a resource's grant log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum ShareLinkRecord {
    Issued(ShareLink),
    /// Names the credential it kills by hash, so a replayed revocation of a
    /// rotated-away secret can never kill its successor in the same slot.
    Revoked {
        link_id: String,
        secret_hash: String,
        at: DateTime<Utc>,
    },
    /// One presentation. `sequence` is stored so a log line reads on its
    /// own; the fold takes the maximum rather than counting lines, which
    /// makes a replayed line the same presentation instead of a phantom
    /// extra one.
    Presented {
        link_id: String,
        at: DateTime<Utc>,
        sequence: u32,
    },
}

/// Capability grants, append-only JSONL, one log per resource.
///
/// Current state is the fold of the log: a re-issue in the same grant slot
/// supersedes the credential in place, a revocation marks it, a presentation
/// raises its presentation count. Nothing is ever rewritten.
#[derive(Debug, Clone)]
pub struct ShareLinkStore {
    workspace_layout: ArtifactV2Workspace,
}

impl ShareLinkStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn root(&self, scope: &ShareLinkScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("share_links")
    }

    fn log_path(&self, scope: &ShareLinkScope, resource_ref: &str) -> PathBuf {
        self.root(scope)
            .join(format!("{}.jsonl", stable_id(resource_ref)))
    }

    /// Issue a link: one identity, one link.
    ///
    /// Refuses while the identity already holds a live (unrevoked, unexpired)
    /// link on the resource — under whatever audience — so revocation always
    /// has exactly one target. Replaying the identical issuance (same slot,
    /// same secret, same expiry) returns the existing link instead: a retried
    /// call resumes rather than duplicates.
    ///
    /// Also refuses a secret that already names another credential on the
    /// resource: if two grants shared a hash, a presentation could not be
    /// attributed honestly.
    ///
    /// And a killed credential may never live again — resource-wide, not
    /// just in its own slot: the fold accumulates every secret hash ever
    /// revoked or superseded, and issuing any of them refuses, however many
    /// generations back it died — a live record under such a hash would
    /// resurrect a killed URL. A secret whose credential merely lapsed in
    /// its own slot refuses too: re-arming it would silently extend it past
    /// the no-extend rule rotation enforces.
    pub fn issue(
        &self,
        scope: &ShareLinkScope,
        request: &IssueShareLink,
        now: DateTime<Utc>,
    ) -> Result<ShareLink> {
        validate(request, now)?;
        let (held, dead_hashes) = self.fold_full(scope, &request.resource_ref)?;
        self.issue_into(scope, &held, &dead_hashes, request, None, now)
    }

    /// Replace an identity's live credential: revoke the old, issue the new —
    /// one call, two appended records.
    ///
    /// Refuses when there is nothing live to rotate, because a caller who
    /// believes an old credential was just invalidated must find out none
    /// existed; refuses a replacement secret equal to the one being rotated
    /// away, because that would leave the credential this call claims to
    /// invalidate still presentable; and refuses a request bound to a
    /// different audience than the live link's — a rebind is a different
    /// operation, done as an explicit revoke and issue, because rotating
    /// across relationships would open a fresh grant slot and reset the
    /// presentation count that belongs to the identity.
    ///
    /// Replaying a rotation that already completed — the response was lost
    /// after both records landed — returns the live link instead of erroring:
    /// the live credential's explicit [`ShareLink::rotation_of`] mark, which
    /// only a rotate writes, tells a finished rotation apart from a
    /// first-time rotate to the standing secret, so the retry resumes the way
    /// a retried [`issue`](Self::issue) does.
    ///
    /// A replacement secret that was ever revoked or rotated away on the
    /// resource refuses before anything is appended: a killed credential may
    /// never live again, and refusing early keeps the live credential
    /// untouched.
    ///
    /// There is deliberately no "extend": a longer life means a fresh secret,
    /// so a leaked URL can never be silently made longer-lived.
    pub fn rotate(
        &self,
        scope: &ShareLinkScope,
        request: &IssueShareLink,
        now: DateTime<Utc>,
    ) -> Result<ShareLink> {
        validate(request, now)?;
        let (mut held, mut dead_hashes) = self.fold_full(scope, &request.resource_ref)?;
        let Some(live) = held
            .iter()
            .find(|link| link.issued_to == request.issued_to && link.is_live(now))
            .cloned()
        else {
            anyhow::bail!(
                "`{}` holds no live link to `{}`, so there is nothing to rotate; refusing rather \
                 than quietly issuing, because a caller who believes an old credential was just \
                 invalidated must find out none existed",
                request.issued_to,
                request.resource_ref
            );
        };
        if dead_hashes.contains(&hash_secret(&request.secret)) {
            anyhow::bail!(
                "this secret named a credential on `{}` that was revoked or rotated away; a \
                 killed credential may never live again, so rotation requires a fresh secret — \
                 refused before the live credential was touched",
                request.resource_ref
            );
        }
        if live.link_id == derive_link_id(scope, request)
            && live.secret_hash == hash_secret(&request.secret)
            && live.expires_at == request.expires_at
            && live.rotation_of.is_some()
        {
            // The identical rotation noticed twice — a retried call whose
            // response was lost after both records landed. The credential the
            // call means to invalidate is already dead and its replacement is
            // already the slot's live one, so the retry resumes instead of
            // failing the same-secret refusal below, which in this
            // interleaving would assert a falsehood. `rotation_of` is what
            // makes resuming safe: only a rotate writes it, so a same-clock
            // revoke-plus-issue can no longer forge the evidence the way the
            // deleted timestamp-coincidence inference let it — a first-time
            // rotate to the standing secret still refuses below.
            return Ok(live);
        }
        if live.audience != request.audience {
            anyhow::bail!(
                "`{}`'s live link to `{}` is bound to `{}`, not `{}`; rotation replaces the \
                 credential inside one grant slot, and a rebind is a different operation — \
                 revoke and issue under the new audience explicitly — because rotating across \
                 relationships would open a fresh slot whose reset presentation count restarts \
                 the record of how often this identity's link has been used",
                request.issued_to,
                request.resource_ref,
                live.audience.as_key(),
                request.audience.as_key(),
            );
        }
        if live.secret_hash == hash_secret(&request.secret) {
            anyhow::bail!(
                "the replacement secret is the one being rotated away; rotating to the same \
                 secret would leave the credential this call claims to invalidate still \
                 presentable"
            );
        }

        self.append(
            &self.log_path(scope, &request.resource_ref),
            &ShareLinkRecord::Revoked {
                link_id: live.link_id.clone(),
                secret_hash: live.secret_hash.clone(),
                at: now,
            },
        )?;
        // Mirror the revocation in the already-folded state so the issue half
        // sees it without a second read of the log — and in the dead-hash
        // set, so the credential this call just killed is dead there too.
        for link in held.iter_mut() {
            if link.link_id == live.link_id && link.revoked_at.is_none() {
                link.revoked_at = Some(now);
            }
        }
        dead_hashes.insert(live.secret_hash.clone());
        self.issue_into(
            scope,
            &held,
            &dead_hashes,
            request,
            Some(live.secret_hash.clone()),
            now,
        )
    }

    /// Kill every unrevoked link an identity holds on a resource.
    ///
    /// **Forward-only**: revocation prevents future presentation; it does not
    /// recall anything already fetched, and the presentations accumulated
    /// before the kill stay on the record — erasing them would falsify the
    /// audit log.
    ///
    /// **Idempotent**: the first revocation is the revocation. A later call
    /// finds nothing unrevoked and appends nothing, so the recorded time of
    /// the kill never moves. Revoking an identity that holds nothing is a
    /// no-op success: this is a removal, so fail-closed cuts the other way —
    /// the end-state the caller asked for already holds.
    ///
    /// Returns the identity's links on the resource after the call, in a
    /// stable order.
    pub fn revoke(
        &self,
        scope: &ShareLinkScope,
        resource_ref: &str,
        issued_to: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<ShareLink>> {
        let path = self.log_path(scope, resource_ref);
        let mut theirs: Vec<ShareLink> = self
            .fold(scope, resource_ref)?
            .into_iter()
            .filter(|link| link.issued_to == issued_to)
            .collect();
        for link in &mut theirs {
            // First revocation wins, at the write: an already-revoked link
            // keeps its recorded time of death and gets no second record.
            if link.revoked_at.is_none() {
                self.append(
                    &path,
                    &ShareLinkRecord::Revoked {
                        link_id: link.link_id.clone(),
                        secret_hash: link.secret_hash.clone(),
                        at: now,
                    },
                )?;
                link.revoked_at = Some(now);
            }
        }
        theirs.sort_by(|left, right| left.link_id.cmp(&right.link_id));
        Ok(theirs)
    }

    /// Present a secret against a resource.
    ///
    /// The outer `Result` is storage — whether the log could be read and
    /// appended. The inner one is the decision. They are kept apart because a
    /// disk error is not a refusal and must never be recorded as one: an audit
    /// trail where an I/O blip reads as "revoked" lies about what the owner
    /// did.
    ///
    /// The gates run in a fixed order and the refusal names the first that
    /// failed: the secret must name a current credential; the credential must
    /// be unrevoked and unexpired (inclusive); the supplied audience must be
    /// the relationship the grant was bound to; that relationship must still
    /// be current; and it must still admit the identity the link was issued
    /// to. The `audience` is supplied by whoever owns the relationship and is
    /// checked live, so access ends the moment the relationship does.
    ///
    /// On success the presentation is appended and numbered: `sequence` is
    /// prior presentations plus one. It counts this grant slot's
    /// presentations and is not the access log's visit number — see
    /// [`Presentation`].
    pub fn present(
        &self,
        scope: &ShareLinkScope,
        resource_ref: &str,
        secret: &str,
        audience: &Audience,
        now: DateTime<Utc>,
    ) -> Result<Result<Presentation, PresentationRefused>> {
        let held = self.fold(scope, resource_ref)?;
        let presented_hash = hash_secret(secret);
        let Some(link) = held.iter().find(|link| link.secret_hash == presented_hash) else {
            return Ok(Err(PresentationRefused::UnknownSecret));
        };
        if link.revoked_at.is_some() {
            return Ok(Err(PresentationRefused::Revoked));
        }
        if now >= link.expires_at {
            return Ok(Err(PresentationRefused::Expired));
        }
        if audience.reference != link.audience {
            return Ok(Err(PresentationRefused::AudienceMismatch));
        }
        if !audience.is_current(now) {
            return Ok(Err(PresentationRefused::AudienceEnded));
        }
        // `admits` is an `any` over the roster, so an audience naming nobody
        // admits nobody: the empty roster fails closed, never vacuously open.
        if !audience.admits(&link.issued_to, now) {
            return Ok(Err(PresentationRefused::IdentityNotAdmitted));
        }

        let sequence = link.presentations + 1;
        self.append(
            &self.log_path(scope, resource_ref),
            &ShareLinkRecord::Presented {
                link_id: link.link_id.clone(),
                at: now,
                sequence,
            },
        )?;
        Ok(Ok(Presentation {
            issued_to: link.issued_to.clone(),
            sequence,
        }))
    }

    /// Every grant on one resource, dead ones included, in a stable order.
    ///
    /// Revoked and expired links stay readable because the audit question —
    /// who could ever have opened this, and how often did they — is about the
    /// whole history, not the survivors.
    pub fn for_resource(
        &self,
        scope: &ShareLinkScope,
        resource_ref: &str,
    ) -> Result<Vec<ShareLink>> {
        let mut out = self.fold(scope, resource_ref)?;
        out.sort_by(|left, right| {
            left.issued_to
                .cmp(&right.issued_to)
                .then_with(|| left.link_id.cmp(&right.link_id))
        });
        Ok(out)
    }

    /// The identity's live grant on a resource, if any — at most one, by
    /// construction.
    pub fn live_link(
        &self,
        scope: &ShareLinkScope,
        resource_ref: &str,
        issued_to: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<ShareLink>> {
        Ok(self
            .fold(scope, resource_ref)?
            .into_iter()
            .find(|link| link.issued_to == issued_to && link.is_live(now)))
    }

    /// The issue half shared by [`issue`](Self::issue) and
    /// [`rotate`](Self::rotate): both have already folded the log once, so
    /// this works from that fold — the links plus the resource's dead-hash
    /// set — rather than reading the file again. `rotation_of` is `Some`
    /// only on the rotate path, and is what stamps the successor with its
    /// explicit rotation evidence.
    fn issue_into(
        &self,
        scope: &ShareLinkScope,
        held: &[ShareLink],
        dead_hashes: &HashSet<String>,
        request: &IssueShareLink,
        rotation_of: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<ShareLink> {
        let secret_hash = hash_secret(&request.secret);
        let link_id = derive_link_id(scope, request);

        // A killed credential may never live again — resource-wide. The
        // per-slot guard further down remembers only the slot's CURRENT
        // credential, so without this set an OLDER revoked secret — rotated
        // away before the kill, or from an earlier slot generation — could
        // be re-armed by a replayed issue, resurrecting a URL the owner
        // already retired. The identical-issuance resume below only ever
        // matches a LIVE credential, so no legitimate retry lands here.
        if dead_hashes.contains(&secret_hash) {
            anyhow::bail!(
                "this secret named a credential on `{}` that was revoked or rotated away; a \
                 killed credential may never live again, so a re-issue requires a fresh secret",
                request.resource_ref
            );
        }

        if let Some(live) = held
            .iter()
            .find(|link| link.issued_to == request.issued_to && link.is_live(now))
        {
            if live.link_id == link_id
                && live.secret_hash == secret_hash
                && live.expires_at == request.expires_at
            {
                // The same issuance noticed twice — a retried call whose
                // response was lost — is one grant. Returning it makes the
                // retry resume instead of duplicate.
                return Ok(live.clone());
            }
            anyhow::bail!(
                "`{}` already holds a live link to `{}`; one identity holds one link so \
                 revocation has exactly one target — rotate the existing link instead of \
                 stacking a second",
                request.issued_to,
                request.resource_ref
            );
        }
        if held
            .iter()
            .any(|link| link.secret_hash == secret_hash && link.link_id != link_id)
        {
            anyhow::bail!(
                "this secret already names another credential on `{}`; a secret must name \
                 exactly one credential, or a presentation could not be attributed honestly",
                request.resource_ref
            );
        }
        // The lapse half of the dead-slot rule. A revoked or superseded
        // hash was already refused by the dead-hash set above, but expiry is
        // the clock's verdict, never a log record, so a lapsed-yet-unrevoked
        // credential is not in the set — and without this check a re-issue
        // of its secret after the lapse would silently extend it, the exact
        // extend `rotate` refuses by design.
        if let Some(slot) = held.iter().find(|link| link.link_id == link_id) {
            if slot.secret_hash == secret_hash && !slot.is_live(now) {
                anyhow::bail!(
                    "this secret already named a credential on `{}` that is now {}; a killed or \
                     lapsed credential is never re-armed, so a re-issue requires a fresh secret",
                    request.resource_ref,
                    slot.state(now).as_str(),
                );
            }
        }

        // The presentation count survives a re-issue in the same slot: it
        // belongs to the identity, not to the secret in their hands, so the
        // slot's presentation numbering does not restart because a
        // credential was rotated.
        let carried = held
            .iter()
            .find(|link| link.link_id == link_id)
            .map(|link| link.presentations)
            .unwrap_or(0);

        let link = ShareLink {
            link_id,
            resource_ref: request.resource_ref.clone(),
            audience: request.audience.clone(),
            issued_to: request.issued_to.clone(),
            secret_hash,
            issued_at: now,
            expires_at: request.expires_at,
            revoked_at: None,
            rotation_of,
            // Stored as zero; the fold carries the slot's prior count forward.
            presentations: 0,
        };
        self.append(
            &self.log_path(scope, &request.resource_ref),
            &ShareLinkRecord::Issued(link.clone()),
        )?;
        let mut issued = link;
        issued.presentations = carried;
        Ok(issued)
    }

    /// Current state as the fold of the log, in one linear pass.
    ///
    /// Reading and parsing go through [`crate::magician_v2::jsonl`]: only an
    /// absent file reads as an empty store — an unreadable one is an error,
    /// never an absence — and only a torn FINAL line is tolerated, as the one
    /// artifact a crashed append can leave.
    fn fold(&self, scope: &ShareLinkScope, resource_ref: &str) -> Result<Vec<ShareLink>> {
        Ok(self.fold_full(scope, resource_ref)?.0)
    }

    /// The fold plus the resource's dead-hash set: alongside the links,
    /// every `secret_hash` that was ever revoked or superseded on the
    /// resource. [`issue`](Self::issue) and [`rotate`](Self::rotate) refuse
    /// any request whose secret hashes into the set — a killed credential
    /// may never live again, whatever slot it once occupied and however many
    /// generations back it died. A guard that remembered only each slot's
    /// CURRENT credential let a replayed issue re-arm any OLDER revoked
    /// secret, which is exactly the amnesia this set closes.
    ///
    /// A position map keys each grant slot to its place in the output, so a
    /// later `Issued` for the same id supersedes **in place** — that is a
    /// rotation, or a fresh grant in a slot whose credential died — instead of
    /// duplicating the slot the way a consecutive-only dedupe would let it.
    fn fold_full(
        &self,
        scope: &ShareLinkScope,
        resource_ref: &str,
    ) -> Result<(Vec<ShareLink>, HashSet<String>)> {
        let path = self.log_path(scope, resource_ref);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok((Vec::new(), HashSet::new()));
        };

        let records: Vec<ShareLinkRecord> =
            crate::magician_v2::jsonl::parse_log_lines(&raw, &path)?;
        let mut positions: HashMap<String, usize> = HashMap::new();
        let mut out: Vec<ShareLink> = Vec::new();
        let mut dead_hashes: HashSet<String> = HashSet::new();
        for record in records {
            match record {
                ShareLinkRecord::Issued(link) => match positions.get(&link.link_id) {
                    Some(&held) => {
                        // A superseded credential is dead for good: its hash
                        // joins the resource-wide dead set so no later issue
                        // or rotate can ever re-arm it, however many
                        // generations later. Whether the successor arrived by
                        // rotation is the record's own explicit `rotation_of`
                        // mark — never inferred from timestamps, which a
                        // same-clock revoke-plus-issue forges.
                        dead_hashes.insert(out[held].secret_hash.clone());
                        // Supersede in place, carrying the presentation
                        // count: the count belongs to the identity, not the
                        // credential.
                        let carried = out[held].presentations;
                        out[held] = link;
                        out[held].presentations = out[held].presentations.max(carried);
                    },
                    None => {
                        positions.insert(link.link_id.clone(), out.len());
                        out.push(link);
                    },
                },
                ShareLinkRecord::Revoked {
                    link_id,
                    secret_hash,
                    at,
                } => {
                    // Every hash a revocation ever named is dead for good,
                    // current credential or long superseded.
                    dead_hashes.insert(secret_hash.clone());
                    if let Some(&held) = positions.get(&link_id) {
                        // The revocation must name the credential it kills, so
                        // a replayed revocation of a rotated-away secret cannot
                        // kill its successor. And the first revocation wins,
                        // defensively here as well as at the write: a duplicate
                        // line must not move the recorded time of death.
                        if out[held].secret_hash == secret_hash && out[held].revoked_at.is_none() {
                            out[held].revoked_at = Some(at);
                        }
                    }
                },
                ShareLinkRecord::Presented {
                    link_id, sequence, ..
                } => {
                    if let Some(&held) = positions.get(&link_id) {
                        // Maximum, not a per-line increment: a replayed line is
                        // the same presentation, and max keeps the count
                        // idempotent under duplication.
                        out[held].presentations = out[held].presentations.max(sequence);
                    }
                },
            }
        }
        Ok((out, dead_hashes))
    }

    fn append(&self, path: &PathBuf, record: &ShareLinkRecord) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    /// Absent is the only error that means "empty": delegates to
    /// [`crate::magician_v2::jsonl::read_log_if_present`], which maps ONLY
    /// `NotFound` to `None` and propagates everything else. Folding an
    /// unreadable log to an empty store would fail OPEN — a revocation would
    /// report its documented no-op success while revoking nothing, an
    /// issuance would pass the one-identity-one-link guard vacuously, and a
    /// presentation would record a disk fault as a refusal, the exact lie
    /// [`present`](Self::present)'s audit contract forbids.
    fn read_if_present(&self, path: &PathBuf) -> Result<Option<String>> {
        crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }
}

/// What every issuance must establish before anything is written. Absent or
/// blank material refuses — a grant that names no resource, no relationship,
/// no identity or no secret grants nothing, because a permissive default here
/// would be a credential nobody meant to mint.
fn validate(request: &IssueShareLink, now: DateTime<Utc>) -> Result<()> {
    if request.resource_ref.trim().is_empty() {
        anyhow::bail!(
            "a share link must name the resource it opens; a grant to nothing cannot be audited \
             or revoked"
        );
    }
    if !request.audience.is_named() {
        anyhow::bail!(
            "a share link must be bound to a named audience; access has to have a relationship \
             it can end with"
        );
    }
    if request.issued_to.trim().is_empty() {
        anyhow::bail!(
            "a share link must name the identity it is issued to; one identity, one link needs \
             the identity"
        );
    }
    if request.secret.trim().is_empty() {
        anyhow::bail!(
            "refusing a blank secret: hashing it would mint a credential anyone could present"
        );
    }
    if now >= request.expires_at {
        anyhow::bail!(
            "a share link must expire ahead of issuance (expiry is inclusive); a grant born \
             lapsed hides a caller bug, and the required expiry exists because a \
             possession-based grant that never lapses is a standing yes to whoever holds the URL"
        );
    }
    Ok(())
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// The stored form of a secret: its blake3 hash, full width.
///
/// The plaintext never touches disk. A store that holds plaintext capabilities
/// is a breach amplifier — one read of the log would mint a working credential
/// for every grant it governs — so the log can verify a presented secret and
/// can do nothing else with it.
fn hash_secret(secret: &str) -> String {
    blake3::hash(secret.as_bytes()).to_hex().to_string()
}

/// The id for one grant slot. Derived, never allocated, from:
///
/// - `scope.principal`, `scope.workspace` — grants are tenant-scoped; two
///   workspaces naming the same resource must not share credential slots.
/// - `resource_ref` — a grant opens one resource; the same identity on two
///   resources holds two links.
/// - `audience.as_key()` — the relationship the grant derives from. `as_key`
///   carries the kind, so the same audience id under two kinds never merges
///   into one grant.
/// - `issued_to` — one identity, one link: the identity is the unit of
///   issuance.
///
/// The secret and the clock are deliberately **not** in the tuple: the id
/// names the grant *slot*, not the credential occupying it, so a rotation or a
/// re-issue after expiry resumes the slot — and its presentation numbering —
/// instead of duplicating it. Derived ids make retries resume instead of
/// duplicate.
fn derive_link_id(scope: &ShareLinkScope, request: &IssueShareLink) -> String {
    format!(
        "shl-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal,
            scope.workspace,
            request.resource_ref,
            request.audience.as_key(),
            request.issued_to,
        ))
    )
}
