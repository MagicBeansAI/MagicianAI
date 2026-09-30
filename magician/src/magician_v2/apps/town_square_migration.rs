//! One-shot migration of the first-party Town Square corpus into the package's
//! app entity store.
//!
//! Queue item 6, slice 3. This is the increment's only bridge: after the engine
//! retires there is no fallback reader, so a defect here is not a degraded read
//! — it is the corpus.
//!
//! # The three properties, and what makes each of them true
//!
//! **It proves what it moved.** Every table carries a count and a digest on
//! both sides. The source digest is computed from the projected payloads; the
//! store digest is computed by reading the rows back out of the entity store
//! and projecting them the same way, through the store's own canonical-JSON
//! digest so the comparison cannot depend on map key order. A run that cannot
//! show the two are equal fails rather than reports success.
//!
//! The proof is a SUBSET claim — every source row present and byte-identical —
//! not an equal-row-count claim, because the package writes rows of its own the
//! moment anyone posts and a proof that broke then could never be re-run.
//!
//! **Identity is content-derived, so the corpus cannot double.** Every migrated
//! row is created with an explicit `record_id` derived from its entity and its
//! natural key. A second create for the same logical row is refused by the
//! store rather than minting a second record. This is the property everything
//! else rests on: without it, "did this row already migrate?" is answerable
//! only from receipt bookkeeping, and any change to how rows are batched turns
//! a replay into a duplicate. It is possible only because `Create` takes a
//! caller-chosen record id.
//!
//! **The plan does not depend on the clock.** Timestamps the host never had are
//! taken from the corpus itself, never from `now`. An earlier cut used the run
//! clock for `synced_at`, which made every run produce a different plan — so
//! the "already complete" check could never fire, the batch digest never
//! matched on a re-run, and a partial migration wedged permanently on
//! `IdempotencyConflict` with no clean recovery. A plan over an unchanged
//! corpus must be byte-identical, run to run, or none of this works.
//!
//! # Calling it
//!
//! The source store is resolved from the same scope as the authenticated
//! destination, so a caller cannot migrate one principal's private square into
//! another's installation.
//!
//! Resumption is the same path as a first run: the write phase creates only
//! what a read-back says is absent and corrects only what it says is different,
//! so a run that died half way is finished by running it again. Batch identity
//! is derived from batch content rather than position, which is what makes a
//! genuine replay replay and anything else a distinct mutation.
//!
//! Note that `now` is one instant threaded through the whole run, so a
//! system-worker scope's ten-minute window is checked against the value it had
//! at the start and never expires mid-run. Long runs complete; they do not
//! half-complete on a clock.
//!
//! **Run it before the package is used.** Rows the package DELETES after a
//! migration read as "not yet migrated" on a later run — nothing can tell a
//! user's deletion from a row that never landed — so a re-run after live use
//! will report the table unfaithful.
//!
//! The migration performs no writes against the social store. Note the narrower
//! claim: merely *opening* a `SocialStore` runs its bootstrap DDL and schema
//! migrations, so "the source is untouched" is false in general even though
//! nothing here writes to it.
//!
//! There is deliberately no entry point wired here yet. Invocation belongs with
//! the engine retirement (slice 4), which is the change that decides when the
//! host store stops being authoritative; adding a second caller now would mean
//! removing it then.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

use super::authority::AuthenticatedAppScope;
use super::boundary::{
    authorize_app_owner_store_mutation, authorize_app_owner_store_query, AppBoundaryError,
    AppCurrentStoreEvidence,
};
use super::entity_mutation::AppEntityMutationError;
use super::entity_store::{AppEntityStoreError, AppEntityStoreService};
use super::models::{
    AppContractError, AppDigest, AppFieldPath, AppInstallationId, AppMutationAtomicity,
    AppMutationCommand, AppMutationOperation, AppName, AppProtocolVersion, AppQueryRequest,
    AppRecordId, AppReference,
};
use super::records::AppMutationOrigin;
use crate::magician_v2::auth::ScopeRef;
use crate::magician_v2::social::store::{SocialCorpusExport, SocialStore, SocialStoreRegistry};

/// Identity of this migration. Fixed, not generated: it is what makes a second
/// invocation a continuation of the first rather than a second corpus.
pub const TOWN_SQUARE_MIGRATION_RUN_ID: &str = "app-migration:town-square-corpus-v1";

/// Prefix for record ids this migration derives. Distinct from the store's own
/// `rec_` namespace, which a caller may not spell.
const MIGRATED_RECORD_ID_PREFIX: &str = "ts_";

/// The two singletons the package addresses by name. `turn_cursor` MUST carry
/// this id: the `ambient_turn` behavior resolves its input by record id, and a
/// host-minted one is unnameable from the manifest.
const SINGLETON_RECORD_ID: &str = "singleton";

/// Stamp used only when the corpus carries no timestamp anywhere — an empty
/// square. Any constant works; what matters is that it is a constant.
const EMPTY_CORPUS_STAMP: &str = "1970-01-01T00:00:00+00:00";

/// Defaults for policy fields the host table never had.
///
/// Their real source is the console (`surfaces/square.js` `POLICY_DEFAULTS`,
/// `surfaces/square.html`) and the package fixtures. `set-policy.md` requires
/// all four fields from its input and declares no defaults of its own, so a
/// migrated square and a freshly seeded one start identical only because these
/// three values agree with the console's.
const DEFAULT_COOLDOWN_SECONDS: i64 = 1_800;
const DEFAULT_MAX_POST_CHARS: i64 = 600;
const DEFAULT_MAX_AUTONOMOUS_REPLIES: i64 = 4;

/// Separator used to derive a composite natural key. A source component that
/// contains it would make the derivation ambiguous, so such a row is refused
/// rather than silently merged with another.
const COMPOSITE_KEY_SEPARATOR: char = ':';

/// Rows one proof read can return. The app store refuses an unindexed snapshot
/// above this, so it is also the largest table this migration will accept —
/// writing a corpus it cannot verify is the one outcome worth refusing early.
const MIGRATION_MAX_PROVABLE_ROWS: usize = super::entity_store::MAX_QUERY_SNAPSHOT_ROWS;

/// Payload bytes one proof read can scan.
///
/// The store accumulates decoded bytes across every row of the entity on the
/// first page and refuses above its own ceiling. Held below that with headroom,
/// because the store counts its own encoding rather than ours.
const MIGRATION_MAX_PROVABLE_BYTES: usize = super::entity_store::MAX_QUERY_SCAN_BYTES / 4 * 3;

/// Rows per page when reading back for the proof. The contract caps a page at
/// `AppContractLimits::default().max_page_rows`, and a request above it is
/// refused at the boundary before it reaches the store.
const MIGRATION_PROOF_PAGE_ROWS: u32 = 100;

/// Rows per mutation command, and the byte budget that really governs it.
///
/// The contract sums every create payload in a command and refuses above
/// `max_value_bytes` (256 KiB). A host post body may be 10,000 characters, so a
/// flat hundred-row chunk can exceed that on a square with a few long posts —
/// which is why chunking counts bytes as well as rows.
const MIGRATION_BATCH_MAX_ROWS: usize = 100;
const MIGRATION_BATCH_MAX_PAYLOAD_BYTES: usize = 192 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum TownSquareMigrationError {
    #[error("town square migration could not read the source corpus: {0}")]
    Source(#[from] anyhow::Error),
    #[error("town square migration found no installation to migrate into")]
    MissingInstallation,
    #[error("town square migration store read failed: {0}")]
    Store(#[from] AppEntityStoreError),
    #[error("town square migration write failed: {0}")]
    Mutation(#[from] AppEntityMutationError),
    #[error("town square migration could not authorize its own write: {0}")]
    Boundary(#[from] AppBoundaryError),
    #[error("town square migration could not build a valid mutation: {0}")]
    Contract(#[from] AppContractError),
    #[error("town square migration payload could not be encoded: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error(
        "town square source row is outside the package contract: {table} row `{key}` has \
         {field} = `{value}`"
    )]
    UnmappableSourceRow {
        table: &'static str,
        key: String,
        field: &'static str,
        value: String,
    },
    #[error(
        "town square source row `{key}` in {table} has a `{field}` containing `{separator}`, \
         which is the separator this migration derives composite identity with; two distinct \
         source rows could collapse into one migrated row"
    )]
    AmbiguousCompositeKey {
        table: &'static str,
        key: String,
        field: &'static str,
        separator: char,
    },
    #[error(
        "town square {table} holds {rows} source rows, above the {limit} this migration can read \
         back and therefore prove; it will not write a corpus it cannot verify"
    )]
    TableTooLargeToProve {
        table: &'static str,
        rows: usize,
        limit: usize,
    },
    #[error(
        "town square entity `{entity}` already holds more than one record under the natural key \
         `{key}`; the corpus is duplicated and this migration will not write on top of it"
    )]
    DuplicateNaturalKey { entity: &'static str, key: String },
    #[error(
        "town square entity `{entity}` holds record `{record_id}` with no `{field}`, so it cannot \
         be matched against the source corpus"
    )]
    MalformedStoreRecord {
        entity: &'static str,
        record_id: String,
        field: &'static str,
    },
    #[error("town square migration failed writing {entity} batch {batch} ({rows} rows): {source}")]
    BatchWrite {
        entity: &'static str,
        batch: u64,
        rows: usize,
        #[source]
        source: AppEntityMutationError,
    },
    #[error(
        "town square {table} would need {bytes} bytes of payload, above the {limit} one proof \
         read can scan; it will not move a corpus it cannot verify"
    )]
    TableTooLargeToScan {
        table: &'static str,
        bytes: usize,
        limit: usize,
    },
    #[error("town square migration produced a batch digest it could not read back")]
    CorruptBatchDigest,
    #[error(
        "town square migration did not land faithfully: {table} has {source_rows} source rows \
         digesting {source_digest}, but only {matched_rows} were found in the store, digesting \
         {store_digest}"
    )]
    ProofFailed {
        table: &'static str,
        source_rows: u64,
        matched_rows: u64,
        source_digest: String,
        store_digest: String,
    },
}

/// Count-and-digest proof for one source table.
///
/// The proof is a SUBSET claim, deliberately: every source row is present in
/// the store and byte-identical to what the plan projected. It is not "the two
/// sides have the same number of rows", because the package writes rows of its
/// own the moment it is used, and a proof that broke as soon as someone posted
/// would be a proof nobody could ever re-run. `store_rows` is reported for the
/// operator's eyes; `matched_rows` is what the claim rests on.
///
/// A duplicated corpus cannot hide behind that leniency: a natural key holding
/// two records is a hard error before any of this is computed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TownSquareTableProof {
    pub source_table: &'static str,
    pub entity: &'static str,
    pub scope: TownSquarePlanScope,
    pub source_rows: u64,
    /// Every live row of this entity in the store, migrated or not.
    pub store_rows: u64,
    /// Source rows found in the store under the same natural key.
    pub matched_rows: u64,
    /// Matched rows whose stored content differs from the source projection.
    ///
    /// Zero for a `Migrated` table that passed, by definition. For a `Seeded`
    /// one it is the number that matters: `policy` is satisfied by existing, so
    /// a host `autonomous_enabled` that disagrees with an already-seeded
    /// `autonomy_state` would otherwise be invisible — the one bit this
    /// migration actually carries for that table, silently dropped.
    pub divergent_rows: u64,
    pub source_digest: String,
    /// Digest of the matched store rows, taken in source order, so an equal
    /// digest means equal content and not merely an equal count.
    pub store_digest: String,
}

impl TownSquareTableProof {
    pub fn is_faithful(&self) -> bool {
        if self.matched_rows != self.source_rows {
            return false;
        }
        match self.scope {
            TownSquarePlanScope::Migrated => self.source_digest == self.store_digest,
            // A seeded row that already exists is satisfied by existing. See
            // `TownSquarePlanScope::Seeded`.
            TownSquarePlanScope::Seeded => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TownSquareMigrationReport {
    pub run_id: String,
    pub installation_id: String,
    /// True when the corpus already matched before this run wrote anything.
    pub already_complete: bool,
    /// Rows this run created. Distinct from `rows_already_present`, because an
    /// operator deciding whether to retire the engine needs to know what this
    /// invocation did rather than what the store happens to contain.
    pub rows_written: u64,
    /// Rows this run found present but different, and corrected in place.
    pub rows_updated: u64,
    /// Source rows already in the store when this run started.
    pub rows_already_present: u64,
    /// Mutation commands this run issued, including any that replayed a
    /// receipt rather than writing.
    pub batches_issued: u64,
    pub tables: Vec<TownSquareTableProof>,
}

impl TownSquareMigrationReport {
    pub fn is_faithful(&self) -> bool {
        self.tables.iter().all(TownSquareTableProof::is_faithful)
    }

    /// Every source row now accounted for in the store, whether this run put it
    /// there or a previous one did.
    pub fn rows_accounted_for(&self) -> u64 {
        self.tables.iter().map(|table| table.matched_rows).sum()
    }
}

/// How strongly a planned entity is claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TownSquarePlanScope {
    /// Moved from the host corpus. Every row must be present AND byte-identical.
    Migrated,
    /// Seeded rather than moved: `turn_cursor` has no host source at all, and
    /// `policy` is one host bit plus three invented defaults. The package
    /// creates both itself — `sync_roster` seeds the cursor, `set_policy`
    /// writes the policy — so one may already exist, and it may legitimately
    /// differ from what this migration would have written. The claim is
    /// therefore presence, not equality: the migration will not clobber an
    /// operator's policy with a reconstruction of it.
    Seeded,
}

/// One entity's worth of work: the rows to write, in a stable order.
struct PlannedEntity {
    source_table: &'static str,
    entity: &'static str,
    scope: TownSquarePlanScope,
    /// Field names read back for the proof. Exactly the entity's own fields,
    /// with the natural key first.
    fields: &'static [&'static str],
    /// `(natural key, record id, payload)`, ordered by natural key.
    rows: Vec<(String, AppRecordId, Value)>,
}

impl PlannedEntity {
    fn source_digest(&self) -> Result<String, TownSquareMigrationError> {
        Ok(digest_rows(
            self.rows
                .iter()
                .map(|(key, _, payload)| (key.clone(), payload.clone())),
        )?)
    }
}

/// Digest a table's rows, in the order given.
///
/// Uses the store's own canonical-JSON digest rather than `to_string`, so the
/// result does not depend on map key order. That matters more than it looks:
/// with serde_json's `preserve_order` feature a payload's key order is its
/// insertion order, and the two sides of this proof build their payloads in
/// different places. A canonical digest makes them comparable by content alone.
fn digest_rows(rows: impl Iterator<Item = (String, Value)>) -> Result<String, serde_json::Error> {
    let manifest = rows
        .map(|(key, payload)| json!({"key": key, "payload": payload}))
        .collect::<Vec<_>>();
    Ok(AppDigest::blake3_canonical_json(&json!(manifest))?
        .as_str()
        .to_owned())
}

/// The record id a migrated row is created under.
///
/// Content-derived, so it is the same on every run over the same corpus, and
/// so a second create for the same logical row is refused by the store rather
/// than minting a second record. Hashed rather than used verbatim because a
/// natural key is not necessarily a legal record id: `AppRecordId` bars `:`,
/// `@` and `+`, all of which appear in real member ids and composite keys.
fn migrated_record_id(
    entity: &str,
    natural_key: &str,
) -> Result<AppRecordId, TownSquareMigrationError> {
    let digest = AppDigest::blake3(format!("{entity}\u{0}{natural_key}").as_bytes());
    let hex = digest.as_str().trim_start_matches("blake3:");
    Ok(AppRecordId::parse(format!(
        "{MIGRATED_RECORD_ID_PREFIX}{}",
        &hex[..32]
    ))?)
}

/// Join the parts of a composite natural key, refusing any part that contains
/// the separator.
///
/// Without this, the host rows `(post, "a:b", "c")` and `(post, "a", "b:c")` —
/// distinct under the source's composite PRIMARY KEY — would derive the same
/// package id and silently become one row. Member ids may contain `:`
/// (`validate_identifier` permits it) and so may a Slack-style `:+1:` emoji,
/// so this is reachable rather than theoretical.
fn composite_key(
    table: &'static str,
    fields: &[(&'static str, &str)],
) -> Result<String, TownSquareMigrationError> {
    for (field, value) in fields {
        if value.contains(COMPOSITE_KEY_SEPARATOR) {
            return Err(TownSquareMigrationError::AmbiguousCompositeKey {
                table,
                key: fields
                    .iter()
                    .map(|(_, value)| *value)
                    .collect::<Vec<_>>()
                    .join("|"),
                field,
                separator: COMPOSITE_KEY_SEPARATOR,
            });
        }
    }
    Ok(fields
        .iter()
        .map(|(_, value)| *value)
        .collect::<Vec<_>>()
        .join(&COMPOSITE_KEY_SEPARATOR.to_string()))
}

fn timestamp_or(value: &str, fallback: &str) -> String {
    // The host `mentions.created_at` column carries `DEFAULT ''`, so rows
    // written before it existed hold the empty string. The package's field is a
    // required timestamp, and inventing a fresh timestamp for a years-old
    // delivery would be worse than using the post it belongs to.
    if value.trim().is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

fn require_enum(
    table: &'static str,
    key: &str,
    field: &'static str,
    value: &str,
    allowed: &[&str],
) -> Result<String, TownSquareMigrationError> {
    if allowed.contains(&value) {
        return Ok(value.to_owned());
    }
    Err(TownSquareMigrationError::UnmappableSourceRow {
        table,
        key: key.to_owned(),
        field,
        value: value.to_owned(),
    })
}

/// The host mention vocabulary is wider than the package's and does not map
/// one-to-one. Recorded rather than silently flattened:
///
/// - `pending`  -> `pending`
/// - `replied`  -> `handled`, keeping `response_post_id`
/// - `passed`   -> `dropped` (an explicit decline)
/// - `seen`     -> `dropped` (a reply notification acknowledged, and never
///   actionable in the first place)
///
/// The loss is the `passed`/`seen` distinction. Both mean "settled without a
/// reply", the package has one word for that, and nothing in the engine ever
/// read the two apart.
fn map_mention_status(key: &str, status: &str) -> Result<&'static str, TownSquareMigrationError> {
    match status {
        "pending" => Ok("pending"),
        "replied" => Ok("handled"),
        "passed" | "seen" => Ok("dropped"),
        other => Err(TownSquareMigrationError::UnmappableSourceRow {
            table: "mentions",
            key: key.to_owned(),
            field: "status",
            value: other.to_owned(),
        }),
    }
}

/// A timestamp for fields the host never had, derived from the corpus rather
/// than the clock.
///
/// This is load-bearing, not tidiness. An earlier cut used `now`, which made
/// the plan a function of wall-clock time: every run produced different payload
/// bytes, so the "already complete" check could never fire, and a re-run's
/// batch digest never matched its stored receipt — wedging a partial migration
/// on `IdempotencyConflict` with no way to finish it. The plan over an
/// unchanged corpus has to be byte-identical, run to run.
///
/// The newest timestamp anywhere in the corpus is a defensible choice: it is
/// "as of the state this migration read", it is stable, and it never claims the
/// corpus is fresher than it is.
fn corpus_stamp(stamps: impl Iterator<Item = String>) -> String {
    stamps
        .filter(|value| !value.trim().is_empty())
        .max()
        .unwrap_or_else(|| EMPTY_CORPUS_STAMP.to_owned())
}

/// Project the whole source corpus into the package's entities.
fn plan_corpus(social: &SocialStore) -> Result<Vec<PlannedEntity>, TownSquareMigrationError> {
    // One read, one transaction. Eight separate reads against a live engine
    // give a torn corpus, and "every source row present and byte-identical"
    // would then be a claim about a state that never existed at any instant.
    let SocialCorpusExport {
        members,
        self_states,
        groups,
        memberships,
        posts,
        mentions,
        reactions,
        operator_policy: policy,
    } = social.export_corpus()?;

    let stamp = corpus_stamp(
        members
            .iter()
            .map(|row| row.created_at.clone())
            .chain(self_states.iter().map(|row| row.updated_at.clone()))
            .chain(groups.iter().map(|row| row.created_at.clone()))
            .chain(posts.iter().map(|row| row.created_at.clone()))
            .chain(mentions.iter().map(|row| row.created_at.clone()))
            .chain(reactions.iter().map(|row| row.created_at.clone()))
            .chain(policy.iter().map(|row| row.updated_at.clone())),
    );

    let post_created_at = posts
        .iter()
        .map(|post| (post.post_id.clone(), post.created_at.clone()))
        .collect::<BTreeMap<_, _>>();

    let mut planned = Vec::new();

    let mut rows = Vec::with_capacity(members.len());
    for member in &members {
        let kind = require_enum(
            "members",
            &member.member_id,
            "kind",
            &member.kind,
            &["agent", "operator"],
        )?;
        rows.push((
            member.member_id.clone(),
            migrated_record_id("member", &member.member_id)?,
            json!({
                "member_id": member.member_id,
                "kind": kind,
                "display_name": member.display_name,
                "introversion": member.introversion,
                "opted_out": member.opted_out,
                // The host has no column for this. Everything in `members` is
                // someone the square knows about, so the faithful reading is
                // enrolled; `sync_roster` reconciles it afterwards against the
                // live agent roster, which is the only thing that actually
                // knows. Note this does NOT resurrect a retired agent into
                // posting: the ambient turn also requires the live roster to
                // return it as enabled and not opted out, and `opted_out` is
                // carried across faithfully.
                "enrolled": true,
                "created_at": member.created_at,
                // ROW-LOCAL, and that is the point. An earlier cut used a
                // corpus-wide stamp, so one new post moved `synced_at` for
                // every member — a correct incremental run then created the new
                // row and still reported failure, because the plan claimed a
                // stamp the untouched rows did not carry. A row's payload must
                // depend on nothing outside that row. `sync_roster` overwrites
                // this on its next reconcile anyway.
                "synced_at": member.created_at,
            }),
        ));
    }
    planned.push(PlannedEntity {
        source_table: "members",
        entity: "member",
        scope: TownSquarePlanScope::Migrated,
        fields: &[
            "member_id",
            "kind",
            "display_name",
            "introversion",
            "opted_out",
            "enrolled",
            "created_at",
            "synced_at",
        ],
        rows,
    });

    let mut rows = Vec::with_capacity(self_states.len());
    for state in &self_states {
        rows.push((
            state.member_id.clone(),
            migrated_record_id("self_state", &state.member_id)?,
            json!({
                "member_id": state.member_id,
                "valence": state.valence,
                "energy": state.energy,
                "baseline_valence": state.baseline_valence,
                "baseline_energy": state.baseline_energy,
                "note": state.note,
                "updated_at": state.updated_at,
            }),
        ));
    }
    planned.push(PlannedEntity {
        source_table: "self_state",
        entity: "self_state",
        scope: TownSquarePlanScope::Migrated,
        fields: &[
            "member_id",
            "valence",
            "energy",
            "baseline_valence",
            "baseline_energy",
            "note",
            "updated_at",
        ],
        rows,
    });

    let mut rows = Vec::with_capacity(groups.len());
    for group in &groups {
        rows.push((
            group.group_id.clone(),
            migrated_record_id("group", &group.group_id)?,
            json!({
                "group_id": group.group_id,
                "name": group.name,
                "created_by": group.created_by,
                "created_at": group.created_at,
            }),
        ));
    }
    planned.push(PlannedEntity {
        source_table: "groups",
        entity: "group",
        scope: TownSquarePlanScope::Migrated,
        fields: &["group_id", "name", "created_by", "created_at"],
        rows,
    });

    let mut rows = Vec::with_capacity(memberships.len());
    for row in &memberships {
        // The host join has no surrogate key. This derivation is the one
        // `create-group.md` already uses, so a migrated membership and a newly
        // created one share an identity.
        let membership_id = composite_key(
            "group_members",
            &[("group_id", &row.group_id), ("member_id", &row.member_id)],
        )?;
        rows.push((
            membership_id.clone(),
            migrated_record_id("group_membership", &membership_id)?,
            json!({
                "membership_id": membership_id,
                "group_id": row.group_id,
                "member_id": row.member_id,
            }),
        ));
    }
    planned.push(PlannedEntity {
        source_table: "group_members",
        entity: "group_membership",
        scope: TownSquarePlanScope::Migrated,
        fields: &["membership_id", "group_id", "member_id"],
        rows,
    });

    let mut rows = Vec::with_capacity(posts.len());
    for post in &posts {
        let surface = require_enum(
            "posts",
            &post.post_id,
            "surface",
            &post.surface,
            &["feed", "group"],
        )?;
        let post_type = require_enum(
            "posts",
            &post.post_id,
            "post_type",
            &post.post_type,
            &["thought", "reply", "question", "link"],
        )?;
        rows.push((
            post.post_id.clone(),
            migrated_record_id("post", &post.post_id)?,
            json!({
                "post_id": post.post_id,
                "author_id": post.author_id,
                "surface": surface,
                "group_id": post.group_id,
                "post_type": post_type,
                "body": post.body,
                "parent_id": post.parent_id,
                "created_at": post.created_at,
                // Row-local; see the note on `member.synced_at`.
                "synced_at": post.created_at,
            }),
        ));
    }
    planned.push(PlannedEntity {
        source_table: "posts",
        entity: "post",
        scope: TownSquarePlanScope::Migrated,
        fields: &[
            "post_id",
            "author_id",
            "surface",
            "group_id",
            "post_type",
            "body",
            "parent_id",
            "created_at",
            "synced_at",
        ],
        rows,
    });

    let mut rows = Vec::with_capacity(mentions.len());
    for mention in &mentions {
        let mention_id = composite_key(
            "mentions",
            &[
                ("post_id", &mention.post_id),
                ("mentioned_member_id", &mention.mentioned_member_id),
            ],
        )?;
        let delivery_kind = require_enum(
            "mentions",
            &mention_id,
            "delivery_kind",
            &mention.delivery_kind,
            &["explicit_mention", "reply_notification"],
        )?;
        let status = map_mention_status(&mention_id, &mention.status)?;
        let created_at = timestamp_or(
            &mention.created_at,
            post_created_at
                .get(&mention.post_id)
                .map(String::as_str)
                .unwrap_or(&stamp),
        );
        rows.push((
            mention_id.clone(),
            migrated_record_id("mention", &mention_id)?,
            json!({
                "mention_id": mention_id,
                "post_id": mention.post_id,
                "mentioned_member_id": mention.mentioned_member_id,
                "delivery_kind": delivery_kind,
                "status": status,
                "created_at": created_at,
                "handled_at": mention.handled_at,
                "response_post_id": mention.response_post_id,
            }),
        ));
    }
    planned.push(PlannedEntity {
        source_table: "mentions",
        entity: "mention",
        scope: TownSquarePlanScope::Migrated,
        fields: &[
            "mention_id",
            "post_id",
            "mentioned_member_id",
            "delivery_kind",
            "status",
            "created_at",
            "handled_at",
            "response_post_id",
        ],
        rows,
    });

    let mut rows = Vec::with_capacity(reactions.len());
    for reaction in &reactions {
        // Same derivation the console uses, so a migrated reaction and a
        // clicked one are the same reaction rather than two.
        let reaction_id = composite_key(
            "reactions",
            &[
                ("post_id", &reaction.post_id),
                ("member_id", &reaction.member_id),
                ("emoji", &reaction.emoji),
            ],
        )?;
        rows.push((
            reaction_id.clone(),
            migrated_record_id("reaction", &reaction_id)?,
            json!({
                "reaction_id": reaction_id,
                "post_id": reaction.post_id,
                "member_id": reaction.member_id,
                "emoji": reaction.emoji,
                "created_at": reaction.created_at,
            }),
        ));
    }
    planned.push(PlannedEntity {
        source_table: "reactions",
        entity: "reaction",
        scope: TownSquarePlanScope::Migrated,
        fields: &["reaction_id", "post_id", "member_id", "emoji", "created_at"],
        rows,
    });

    // The policy singleton. The host row carries one bit and a timestamp; the
    // three bounds it never had take the console's defaults. Seeded rather than
    // migrated: `set_policy` may already have written one, and a reconstruction
    // must not clobber an operator's actual configuration.
    let (autonomy_state, policy_updated_at) = match &policy {
        Some(row) => (
            if row.autonomous_enabled { "on" } else { "off" },
            timestamp_or(&row.updated_at, &stamp),
        ),
        // No row means the operator never touched autonomy. Off is both the
        // faithful reading and the safe one.
        None => ("off", stamp.clone()),
    };
    planned.push(PlannedEntity {
        source_table: "operator_policy",
        entity: "policy",
        scope: TownSquarePlanScope::Seeded,
        fields: &[
            "policy_id",
            "autonomy_state",
            "cooldown_seconds",
            "max_post_chars",
            "max_autonomous_replies",
            "updated_at",
        ],
        rows: vec![(
            SINGLETON_RECORD_ID.to_owned(),
            AppRecordId::parse(SINGLETON_RECORD_ID)?,
            json!({
                "policy_id": SINGLETON_RECORD_ID,
                "autonomy_state": autonomy_state,
                "cooldown_seconds": DEFAULT_COOLDOWN_SECONDS,
                "max_post_chars": DEFAULT_MAX_POST_CHARS,
                "max_autonomous_replies": DEFAULT_MAX_AUTONOMOUS_REPLIES,
                "updated_at": policy_updated_at,
            }),
        )],
    });

    // The rotation cursor has no host source at all: the worker kept its offset
    // in memory, so it did not survive its own process, let alone a migration.
    // It starts fresh, it is named because the behavior that reads it resolves
    // its input by record id, and `sync_roster` seeds the same row — so this is
    // an ensure-exists rather than a claim about the host corpus.
    planned.push(PlannedEntity {
        source_table: "(none)",
        entity: "turn_cursor",
        scope: TownSquarePlanScope::Seeded,
        fields: &["cursor_id", "last_member_id", "turns_taken", "updated_at"],
        rows: vec![(
            SINGLETON_RECORD_ID.to_owned(),
            AppRecordId::parse(SINGLETON_RECORD_ID)?,
            json!({
                "cursor_id": SINGLETON_RECORD_ID,
                "last_member_id": Value::Null,
                "turns_taken": 0,
                "updated_at": stamp,
            }),
        )],
    });

    // Two bounds, because the read-back has two. Rows is the obvious one; bytes
    // is the one that bites, since a host post body may be 10,000 CHARACTERS
    // and the store's snapshot read refuses on accumulated bytes long before
    // 10,000 posts. Both are checked before anything is written, because a
    // corpus that is written and then found unverifiable is the single worst
    // outcome available to a one-shot irreversible move.
    for entity in &planned {
        if entity.rows.len() > MIGRATION_MAX_PROVABLE_ROWS {
            return Err(TownSquareMigrationError::TableTooLargeToProve {
                table: entity.source_table,
                rows: entity.rows.len(),
                limit: MIGRATION_MAX_PROVABLE_ROWS,
            });
        }
        let bytes = entity
            .rows
            .iter()
            .map(|(_, _, payload)| serde_json::to_vec(payload).map(|encoded| encoded.len()))
            .sum::<Result<usize, _>>()?;
        if bytes > MIGRATION_MAX_PROVABLE_BYTES {
            return Err(TownSquareMigrationError::TableTooLargeToScan {
                table: entity.source_table,
                bytes,
                limit: MIGRATION_MAX_PROVABLE_BYTES,
            });
        }
    }

    Ok(planned)
}

/// Read one entity back out of the store, keyed by natural key.
///
/// Refuses a natural key that appears twice. That is the check that stops a
/// duplicated corpus from being certified: without it, two records carrying the
/// same `member_id` collapse into one map entry, `matched_rows` counts the key
/// once, the digests agree, and `is_faithful()` returns true over a doubled
/// square.
async fn read_back(
    store: &AppEntityStoreService,
    authenticated_scope: &AuthenticatedAppScope,
    installation_id: &AppInstallationId,
    planned: &PlannedEntity,
    now: DateTime<Utc>,
) -> Result<(u64, BTreeMap<String, (AppRecordId, Value)>), TownSquareMigrationError> {
    let entity = AppName::parse(planned.entity)?;
    let select = planned
        .fields
        .iter()
        .map(|field| AppFieldPath::parse(*field))
        .collect::<Result<Vec<_>, _>>()?;
    let natural_key = planned.fields[0];

    let active = store
        .active_schema(authenticated_scope, installation_id, now)
        .await?
        .ok_or(TownSquareMigrationError::MissingInstallation)?;

    // The record id rides along because a source row that is PRESENT BUT
    // DIFFERENT has to be updated in place, and the store addresses an update
    // by record id.
    let mut by_key: BTreeMap<String, (AppRecordId, Value)> = BTreeMap::new();
    let mut store_rows = 0u64;
    let mut cursor = None;
    loop {
        let request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: installation_id.clone(),
            entity: entity.clone(),
            select: select.clone(),
            predicate: None,
            order: Vec::new(),
            cursor: cursor.clone(),
            limit: MIGRATION_PROOF_PAGE_ROWS,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("town_square_migration_proof")?,
        };
        let evidence = AppCurrentStoreEvidence::from_trusted_store(
            authenticated_scope,
            active.installation_id().clone(),
            active.installation_generation(),
            active.package_revision_ref().clone(),
            active.grant_revision(),
            active.schema_revision(),
            now,
        )?;
        let fence = authorize_app_owner_store_query(evidence, &request)?;
        let page = store
            .query(authenticated_scope, fence, request, now)
            .await?;
        for projection in &page.envelope.value {
            store_rows += 1;
            let mut payload = Map::new();
            for field in planned.fields {
                let path = AppFieldPath::parse(*field)?;
                payload.insert(
                    (*field).to_owned(),
                    projection.fields.get(&path).cloned().unwrap_or(Value::Null),
                );
            }
            // A row whose natural key is absent or not a string cannot be
            // matched against the plan at all. Defaulting it to `""` would put
            // every such row on one map entry and hide them behind each other.
            let key = payload
                .get(natural_key)
                .and_then(Value::as_str)
                .ok_or_else(|| TownSquareMigrationError::MalformedStoreRecord {
                    entity: planned.entity,
                    record_id: projection.record_id.to_string(),
                    field: natural_key,
                })?
                .to_owned();
            if by_key
                .insert(
                    key.clone(),
                    (projection.record_id.clone(), Value::Object(payload)),
                )
                .is_some()
            {
                return Err(TownSquareMigrationError::DuplicateNaturalKey {
                    entity: planned.entity,
                    key,
                });
            }
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok((store_rows, by_key))
}

/// Build one table's proof from a read-back.
fn table_proof(
    planned: &PlannedEntity,
    store_rows: u64,
    by_key: &BTreeMap<String, (AppRecordId, Value)>,
) -> Result<TownSquareTableProof, TownSquareMigrationError> {
    // Walk the SOURCE order and pull each row out of the store by its natural
    // key. A source row the store does not hold contributes nothing, which
    // makes both the count and the digest disagree — one missing row cannot
    // hide behind another that happens to be present.
    let mut matched = Vec::with_capacity(planned.rows.len());
    let mut divergent = 0u64;
    for (key, _, source_payload) in &planned.rows {
        if let Some((_, stored_payload)) = by_key.get(key) {
            if stored_payload != source_payload {
                divergent += 1;
            }
            matched.push((key.clone(), stored_payload.clone()));
        }
    }

    Ok(TownSquareTableProof {
        source_table: planned.source_table,
        entity: planned.entity,
        scope: planned.scope,
        source_rows: planned.rows.len() as u64,
        store_rows,
        matched_rows: matched.len() as u64,
        divergent_rows: divergent,
        source_digest: planned.source_digest()?,
        store_digest: digest_rows(matched.into_iter())?,
    })
}

/// What a pending row needs done to it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MigrationWrite {
    Create,
    Update(AppRecordId),
}

impl MigrationWrite {
    /// Part of the batch identity seed, so a batch that creates a row and one
    /// that updates the same row are never the same mutation.
    fn tag(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update(_) => "update",
        }
    }
}

type PlannedRow = (String, AppRecordId, Value);

/// Split pending writes into commands bounded by BOTH a row count and an
/// encoded-byte budget, because the contract refuses a command whose payloads
/// sum above `max_value_bytes` and a single host post body can be 10,000
/// characters.
fn batch_writes<'a>(
    pending: &[(MigrationWrite, &'a PlannedRow)],
) -> Result<Vec<Vec<(MigrationWrite, &'a PlannedRow)>>, TownSquareMigrationError> {
    let mut batches = Vec::new();
    let mut current: Vec<(MigrationWrite, &PlannedRow)> = Vec::new();
    let mut current_bytes = 0usize;
    for (write, row) in pending {
        let bytes = serde_json::to_vec(&row.2)?.len();
        let would_overflow = !current.is_empty()
            && (current.len() >= MIGRATION_BATCH_MAX_ROWS
                || current_bytes + bytes > MIGRATION_BATCH_MAX_PAYLOAD_BYTES);
        if would_overflow {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current.push((write.clone(), row));
        current_bytes += bytes;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    Ok(batches)
}

/// Migrate the first-party Town Square corpus into the package's entity store.
///
/// The source store is resolved from the same scope as the authenticated
/// destination, so this cannot move one principal's square into another's
/// installation. Reads the social store; performs no writes against it.
///
/// Safe to call repeatedly: a corpus that already matches is left alone, and a
/// partial run is finished rather than duplicated.
pub async fn migrate_town_square_corpus(
    social_registry: &SocialStoreRegistry,
    store: &AppEntityStoreService,
    authenticated_scope: &AuthenticatedAppScope,
    installation_id: &AppInstallationId,
    now: DateTime<Utc>,
) -> Result<TownSquareMigrationReport, TownSquareMigrationError> {
    migrate_town_square_corpus_with_options(
        social_registry,
        store,
        authenticated_scope,
        installation_id,
        now,
        false,
    )
    .await
}

/// Restore history after live use without rewriting membership, moods, policy,
/// or the behavior cursor. The same scoped mutation owner and read-back proof
/// apply to every selected historical entity.
pub async fn migrate_town_square_corpus_with_options(
    social_registry: &SocialStoreRegistry,
    store: &AppEntityStoreService,
    authenticated_scope: &AuthenticatedAppScope,
    installation_id: &AppInstallationId,
    now: DateTime<Utc>,
    history_only: bool,
) -> Result<TownSquareMigrationReport, TownSquareMigrationError> {
    // Source and destination share one scope by construction rather than by
    // caller convention. A one-shot irreversible move of a private corpus is
    // not somewhere to trust that two arguments agree.
    let scope = authenticated_scope.scope();
    let scope_ref = ScopeRef::system_internal_unauthenticated(
        scope.principal.as_str(),
        scope.workspace.as_str(),
    );
    // `store_for_scope`, not `existing_store_for_scope`: the latter skips
    // `migrate_legacy_default_store_if_needed`, so on the `anonymous/default`
    // scope it reports "no store" while the corpus is still sitting at the
    // legacy path. An operator reading that as "nothing to migrate" would
    // retire the engine over a live square. Materialising an empty scoped store
    // for a genuinely empty square is by far the cheaper mistake.
    let social = social_registry.store_for_scope(&scope_ref)?;

    let mut planned = plan_corpus(social.as_ref())?;
    if history_only {
        planned.retain(|entity| {
            matches!(
                entity.entity,
                "post" | "reaction" | "group" | "group_membership" | "mention"
            )
        });
    }

    // Check before writing. This is both the "refuse to run twice" and the
    // proof, which is why there is no separate has-run flag to drift.
    let mut before = Vec::with_capacity(planned.len());
    for entity in &planned {
        let (store_rows, by_key) =
            read_back(store, authenticated_scope, installation_id, entity, now).await?;
        before.push((table_proof(entity, store_rows, &by_key)?, by_key));
    }
    let rows_already_present = before
        .iter()
        .map(|(proof, _)| proof.matched_rows)
        .sum::<u64>();
    if before.iter().all(|(proof, _)| proof.is_faithful()) {
        return Ok(TownSquareMigrationReport {
            run_id: TOWN_SQUARE_MIGRATION_RUN_ID.to_owned(),
            installation_id: installation_id.as_str().to_owned(),
            already_complete: true,
            rows_written: 0,
            rows_updated: 0,
            rows_already_present,
            batches_issued: 0,
            tables: before.into_iter().map(|(proof, _)| proof).collect(),
        });
    }

    // One schema revision for the whole run. Re-reading it per batch would
    // silently ADAPT to a package update mid-run — writing the first batches
    // against one schema and the rest against another — which is the opposite
    // of failing closed. Captured once, a mid-run update makes the next batch's
    // fence mismatch and the run stops.
    let active = store
        .active_schema(authenticated_scope, installation_id, now)
        .await?
        .ok_or(TownSquareMigrationError::MissingInstallation)?;
    let schema_revision = active.schema_revision();

    let mut rows_written = 0u64;
    let mut rows_updated = 0u64;
    let mut batches_issued = 0u64;
    for (entity, (_, by_key)) in planned.iter().zip(before.iter()) {
        let entity_name = AppName::parse(entity.entity)?;
        // What still has to happen for this entity, and nothing else.
        //
        // A row absent from the store is created. A row PRESENT BUT DIFFERENT
        // is updated in place, which is the case an earlier cut had no answer
        // for: the package bootstraps `member` and `self_state` rows itself
        // (`sync_roster` seeds the operator with `introversion: 0.0` where the
        // host says `0.5`), so on any real install those tables were present,
        // divergent, and permanently unfixable — create-only cannot repair a
        // row that already exists, so the run reported `ProofFailed` forever.
        //
        // A seeded entity is exempt: `policy` and `turn_cursor` are the
        // package's to own once it has written them, and overwriting an
        // operator's actual policy with a reconstruction of it would be worse
        // than the divergence, which the proof now reports instead.
        let mut pending = Vec::new();
        for row in &entity.rows {
            match by_key.get(&row.0) {
                None => pending.push((MigrationWrite::Create, row)),
                Some((record_id, stored)) if stored != &row.2 => {
                    if entity.scope == TownSquarePlanScope::Migrated {
                        pending.push((MigrationWrite::Update(record_id.clone()), row));
                    }
                },
                Some(_) => {},
            }
        }
        if pending.is_empty() {
            continue;
        }

        for batch in batch_writes(&pending)? {
            // Batch identity is CONTENT-derived, never positional. An earlier
            // cut numbered batches by their index in the pending list, which
            // shrinks on every resumed run: batch 1 of run 2 carried different
            // rows than batch 1 of run 1, collided with run 1's stored receipt,
            // and returned `IdempotencyConflict` — permanently, because the
            // only way to reproduce run 1's bytes was for its rows to be
            // missing again, and they were not. Deriving both the key and the
            // batch number from what the batch actually writes makes a replay a
            // replay, and anything else a distinct mutation.
            let seed = batch
                .iter()
                .map(|(write, row)| format!("{}:{}", write.tag(), row.1))
                .collect::<Vec<_>>()
                .join(",");
            let digest = AppDigest::blake3(format!("{}\u{0}{seed}", entity.entity).as_bytes());
            let hex = digest.as_str().trim_start_matches("blake3:");
            // `| 1` because the contract refuses a zero batch number, and one
            // digest in 2^64 would otherwise be rejected for no reason.
            let migration_batch = u64::from_str_radix(&hex[..16], 16)
                .map_err(|_| TownSquareMigrationError::CorruptBatchDigest)?
                | 1;
            let idempotency_key = AppReference::parse(format!(
                "{TOWN_SQUARE_MIGRATION_RUN_ID}:{}:{}",
                entity.entity,
                &hex[..32]
            ))?;
            let operations = batch
                .iter()
                .enumerate()
                .map(|(position, (write, row))| match write {
                    MigrationWrite::Create => Ok(AppMutationOperation::Create {
                        entity: entity_name.clone(),
                        // Unique within the command and derived from position,
                        // because a natural key is not necessarily a valid
                        // `AppName`. The real identity is the record id.
                        temporary_id: AppName::parse(format!("row_{position}"))?,
                        record_id: Some(row.1.clone()),
                        payload: row.2.clone(),
                    }),
                    MigrationWrite::Update(record_id) => Ok(AppMutationOperation::Update {
                        entity: entity_name.clone(),
                        record_id: record_id.clone(),
                        patch: row.2.clone(),
                    }),
                })
                .collect::<Result<Vec<_>, TownSquareMigrationError>>()?;
            let created = batch
                .iter()
                .filter(|(write, _)| matches!(write, MigrationWrite::Create))
                .count() as u64;
            let updated = operations.len() as u64 - created;

            let command = AppMutationCommand {
                protocol_version: AppProtocolVersion::V1,
                idempotency_key,
                atomicity: AppMutationAtomicity::AllOrNothing,
                expected_schema_revision: schema_revision,
                operations,
                expected_record_revisions: Vec::new(),
            };
            let evidence = AppCurrentStoreEvidence::from_trusted_store(
                authenticated_scope,
                active.installation_id().clone(),
                active.installation_generation(),
                active.package_revision_ref().clone(),
                active.grant_revision(),
                schema_revision,
                now,
            )?;
            let origin = AppMutationOrigin::Migration {
                migration_run_id: AppReference::parse(TOWN_SQUARE_MIGRATION_RUN_ID)?,
                migration_batch,
            };
            let fence = authorize_app_owner_store_mutation(evidence, &command, origin)?;
            let rows = created + updated;
            store
                .mutate(authenticated_scope, fence, command, now)
                .await
                .map_err(|source| TownSquareMigrationError::BatchWrite {
                    entity: entity.entity,
                    batch: migration_batch,
                    rows: rows as usize,
                    source,
                })?;
            rows_written += created;
            rows_updated += updated;
            batches_issued += 1;
        }
    }

    // Prove again, against what actually landed. Every table is proved before
    // any failure is raised, because an operator deciding whether the corpus is
    // safe to retire needs the whole picture, not the first thing that broke.
    let mut tables = Vec::with_capacity(planned.len());
    let mut failure = None;
    for entity in &planned {
        let (store_rows, by_key) =
            read_back(store, authenticated_scope, installation_id, entity, now).await?;
        let proof = table_proof(entity, store_rows, &by_key)?;
        if !proof.is_faithful() && failure.is_none() {
            failure = Some(TownSquareMigrationError::ProofFailed {
                table: proof.source_table,
                source_rows: proof.source_rows,
                matched_rows: proof.matched_rows,
                source_digest: proof.source_digest.clone(),
                store_digest: proof.store_digest.clone(),
            });
        }
        tables.push(proof);
    }
    if let Some(error) = failure {
        return Err(error);
    }

    Ok(TownSquareMigrationReport {
        run_id: TOWN_SQUARE_MIGRATION_RUN_ID.to_owned(),
        installation_id: installation_id.as_str().to_owned(),
        already_complete: false,
        rows_written,
        rows_updated,
        rows_already_present,
        batches_issued,
        tables,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_mention_vocabulary_maps_onto_the_package_enum() {
        // Every status the engine can write must land on one the package
        // declares, or a migrated row is refused by its own schema.
        assert_eq!(map_mention_status("k", "pending").unwrap(), "pending");
        assert_eq!(map_mention_status("k", "replied").unwrap(), "handled");
        assert_eq!(map_mention_status("k", "passed").unwrap(), "dropped");
        assert_eq!(map_mention_status("k", "seen").unwrap(), "dropped");
        // And anything else stops the migration rather than being guessed at.
        assert!(matches!(
            map_mention_status("k", "invented"),
            Err(TownSquareMigrationError::UnmappableSourceRow {
                field: "status",
                ..
            })
        ));
    }

    #[test]
    fn an_empty_host_timestamp_falls_back_rather_than_inventing_now() {
        assert_eq!(
            timestamp_or("2026-01-01T00:00:00Z", "fb"),
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(timestamp_or("", "fb"), "fb");
        assert_eq!(timestamp_or("   ", "fb"), "fb");
    }

    #[test]
    fn a_row_outside_the_package_contract_stops_the_migration() {
        let error = require_enum(
            "posts",
            "p1",
            "surface",
            "carrier_pigeon",
            &["feed", "group"],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            TownSquareMigrationError::UnmappableSourceRow {
                table: "posts",
                field: "surface",
                ..
            }
        ));
        assert!(require_enum("posts", "p1", "surface", "feed", &["feed", "group"]).is_ok());
    }

    #[test]
    fn the_plan_does_not_move_when_the_clock_does() {
        // The property the whole design rests on. An earlier cut stamped
        // payloads with `now`, which made every run produce different bytes —
        // so "already complete" could never fire and a resumed run wedged on
        // IdempotencyConflict with no way to finish.
        let stamps = || {
            [
                "2026-01-01T00:00:00Z".to_owned(),
                "2026-03-04T05:06:07Z".to_owned(),
                "2026-02-02T00:00:00Z".to_owned(),
            ]
            .into_iter()
        };
        assert_eq!(corpus_stamp(stamps()), "2026-03-04T05:06:07Z");
        assert_eq!(corpus_stamp(stamps()), corpus_stamp(stamps()));
        // Blank stamps never win, and an empty corpus still yields a constant.
        assert_eq!(
            corpus_stamp(["".to_owned(), "  ".to_owned()].into_iter()),
            EMPTY_CORPUS_STAMP
        );
        assert_eq!(corpus_stamp(std::iter::empty()), EMPTY_CORPUS_STAMP);
    }

    #[test]
    fn a_migrated_record_id_is_stable_content_derived_and_legal() {
        let first = migrated_record_id("post", "post-3f1a").unwrap();
        assert_eq!(first, migrated_record_id("post", "post-3f1a").unwrap());
        // Entity is part of the identity, so two entities' rows cannot collide.
        assert_ne!(first, migrated_record_id("mention", "post-3f1a").unwrap());
        assert_ne!(first, migrated_record_id("post", "post-3f1b").unwrap());
        assert!(first.as_str().starts_with(MIGRATED_RECORD_ID_PREFIX));
        // Never inside the store's own minting namespace, which a caller may
        // not spell.
        assert!(!first.as_str().starts_with("rec_"));
        // And a natural key that is NOT a legal record id still yields one.
        assert!(migrated_record_id("member", "assistant@example.com").is_ok());
        assert!(migrated_record_id("reaction", "p:m:+1").is_ok());
    }

    #[test]
    fn a_composite_key_refuses_a_component_that_would_make_it_ambiguous() {
        assert_eq!(
            composite_key("mentions", &[("post_id", "p1"), ("member_id", "m1")]).unwrap(),
            "p1:m1"
        );
        // `(p, "a:b", "c")` and `(p, "a", "b:c")` are distinct rows under the
        // host's composite PRIMARY KEY and would derive the same package id.
        // Member ids may contain `:` and so may a Slack-style `:+1:` emoji.
        let error = composite_key(
            "reactions",
            &[("post_id", "p"), ("member_id", "a:b"), ("emoji", "c")],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            TownSquareMigrationError::AmbiguousCompositeKey {
                table: "reactions",
                field: "member_id",
                ..
            }
        ));
    }

    #[test]
    fn the_digest_is_order_sensitive_and_content_sensitive() {
        let rows = |second: Value| {
            [
                ("k1".to_owned(), json!({"v": 1})),
                ("k2".to_owned(), second),
            ]
            .into_iter()
        };
        let a = digest_rows(rows(json!({"v": 2}))).unwrap();
        assert_eq!(a, digest_rows(rows(json!({"v": 2}))).unwrap());
        assert_ne!(
            a,
            digest_rows(rows(json!({"v": 3}))).unwrap(),
            "a changed value must change the digest"
        );
        let reordered = digest_rows(
            [
                ("k2".to_owned(), json!({"v": 2})),
                ("k1".to_owned(), json!({"v": 1})),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_ne!(a, reordered, "order is part of the proof");
        assert!(a.starts_with("blake3:"));
    }

    fn proof(
        scope: TownSquarePlanScope,
        source_rows: u64,
        store_rows: u64,
        matched_rows: u64,
    ) -> TownSquareTableProof {
        TownSquareTableProof {
            source_table: "posts",
            entity: "post",
            scope,
            source_rows,
            store_rows,
            matched_rows,
            divergent_rows: 0,
            source_digest: "blake3:aa".to_owned(),
            store_digest: "blake3:aa".to_owned(),
        }
    }

    fn planned_row(key: &str, body: &str) -> PlannedRow {
        (
            key.to_owned(),
            migrated_record_id("post", key).unwrap(),
            json!({ "body": body }),
        )
    }

    #[test]
    fn a_migrated_proof_needs_every_source_row_present_and_equal() {
        assert!(proof(TownSquarePlanScope::Migrated, 3, 3, 3).is_faithful());

        let mut missing = proof(TownSquarePlanScope::Migrated, 3, 2, 2);
        assert!(
            !missing.is_faithful(),
            "a source row that did not land is not faithful"
        );
        missing.matched_rows = 3;
        missing.store_digest = "blake3:bb".to_owned();
        assert!(
            !missing.is_faithful(),
            "equal counts with different content is exactly what a digest is for"
        );
    }

    #[test]
    fn a_seeded_proof_is_satisfied_by_presence_alone() {
        // `policy` and `turn_cursor` are seeded, not moved: the package creates
        // both itself, so one may already exist and legitimately differ from
        // what this migration would have written. Clobbering an operator's
        // actual policy with a reconstruction of it would be worse than the
        // digest disagreeing.
        let mut seeded = proof(TownSquarePlanScope::Seeded, 1, 1, 1);
        seeded.store_digest = "blake3:something-else".to_owned();
        assert!(seeded.is_faithful());
        seeded.matched_rows = 0;
        assert!(!seeded.is_faithful(), "but it does have to exist");
    }

    #[test]
    fn the_proof_survives_the_package_writing_rows_of_its_own() {
        // The store legitimately grows past the source the moment someone
        // posts. A proof that read that as failure could never be re-run, and
        // re-running is how a partial migration gets finished. Duplication is
        // caught by `read_back`'s natural-key check instead, which is a
        // stronger guard than a row count.
        let live = proof(TownSquarePlanScope::Migrated, 3, 57, 3);
        assert!(live.is_faithful());
        assert!(live.store_rows > live.source_rows);
    }

    #[test]
    fn batches_are_bounded_by_bytes_as_well_as_rows() {
        // A host post body may be 10,000 characters, so a flat hundred-row
        // chunk can exceed the contract's aggregate payload budget.
        let fat = (0..40)
            .map(|index| planned_row(&format!("k{index}"), &"x".repeat(10_000)))
            .collect::<Vec<_>>();
        let pending = fat
            .iter()
            .map(|row| (MigrationWrite::Create, row))
            .collect::<Vec<_>>();
        let batches = batch_writes(&pending).unwrap();
        assert!(
            batches.len() > 1,
            "40 fat rows must not ride in one command"
        );
        for batch in &batches {
            let bytes = batch
                .iter()
                .map(|(_, row)| serde_json::to_vec(&row.2).unwrap().len())
                .sum::<usize>();
            assert!(bytes <= MIGRATION_BATCH_MAX_PAYLOAD_BYTES || batch.len() == 1);
            assert!(batch.len() <= MIGRATION_BATCH_MAX_ROWS);
        }
        assert_eq!(
            batches.iter().map(Vec::len).sum::<usize>(),
            fat.len(),
            "every row must appear exactly once"
        );

        // Thin rows still batch by count.
        let thin = (0..250)
            .map(|index| planned_row(&format!("k{index}"), "small"))
            .collect::<Vec<_>>();
        let pending = thin
            .iter()
            .map(|row| (MigrationWrite::Create, row))
            .collect::<Vec<_>>();
        let batches = batch_writes(&pending).unwrap();
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), MIGRATION_BATCH_MAX_ROWS);
        assert_eq!(batches.iter().map(Vec::len).sum::<usize>(), thin.len());
    }

    #[test]
    fn batch_identity_follows_content_not_position() {
        // The defect this replaces: batch numbers were the index in a `missing`
        // list that shrinks on every resumed run, so batch 1 of run 2 carried
        // different rows than batch 1 of run 1, collided with run 1's stored
        // receipt, and wedged on `IdempotencyConflict` forever.
        //
        // The seed a batch derives its key from must therefore depend only on
        // what that batch writes.
        let rows = (0..3)
            .map(|index| planned_row(&format!("k{index}"), "body"))
            .collect::<Vec<_>>();
        let seed = |pending: &[(MigrationWrite, &PlannedRow)]| {
            pending
                .iter()
                .map(|(write, row)| format!("{}:{}", write.tag(), row.1))
                .collect::<Vec<_>>()
                .join(",")
        };

        let full = rows
            .iter()
            .map(|row| (MigrationWrite::Create, row))
            .collect::<Vec<_>>();
        // The same rows, reached on a later run after the first two landed.
        let remainder = rows[2..]
            .iter()
            .map(|row| (MigrationWrite::Create, row))
            .collect::<Vec<_>>();
        assert_ne!(
            seed(&full),
            seed(&remainder),
            "a different set of rows must be a different mutation"
        );
        assert_eq!(
            seed(&remainder),
            seed(&remainder),
            "the same set of rows must be the same mutation, so a replay replays"
        );

        // Creating a row and updating it are not the same mutation either.
        let updating = rows[..1]
            .iter()
            .map(|row| (MigrationWrite::Update(row.1.clone()), row))
            .collect::<Vec<_>>();
        let creating = rows[..1]
            .iter()
            .map(|row| (MigrationWrite::Create, row))
            .collect::<Vec<_>>();
        assert_ne!(seed(&updating), seed(&creating));
    }

    #[test]
    fn the_proof_page_size_is_admissible_as_a_query() {
        // A page above `AppContractLimits::max_page_rows` is refused at the
        // boundary before it ever reaches the store, which would make this
        // module dead on arrival — an earlier cut asked for 500 and failed on
        // the first proof of the first table, every time, without writing
        // anything. The limit's field is private, so pin it through the
        // validator that actually enforces it.
        use super::super::models::ValidateAppContract;
        let request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("post").unwrap(),
            select: vec![AppFieldPath::parse("post_id").unwrap()],
            predicate: None,
            order: Vec::new(),
            cursor: None,
            limit: MIGRATION_PROOF_PAGE_ROWS,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("town_square_migration_proof").unwrap(),
        };
        assert!(request
            .validate_app_contract(&super::super::models::AppContractLimits::default())
            .is_ok());
    }
}
