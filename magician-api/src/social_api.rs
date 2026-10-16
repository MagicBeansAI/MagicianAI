//! The `/social/*` HTTP surface, re-pointed at the Town Square package's
//! entity store.
//!
//! Queue item 6, slice 4. The corpus moved into the package (slice 3), so these
//! routes can no longer read a first-party store that is no longer the
//! authority. The invariant this module exists to hold is narrow and absolute:
//! **the wire does not move even though everything under it does.**
//! `ui/unified-ui/.../town-square` is an unchanged consumer of unchanged JSON.
//!
//! Three consequences of that invariant shape the code:
//!
//! **Responses are built from the original structs.** `Post`, `Member`,
//! `SelfState`, `Group` and `Reaction` still come from `social::types`, and
//! every handler projects entity-store rows back into them rather than
//! hand-rolling JSON. Serialising the same types is the only way to be sure the
//! bytes match; a hand-written `json!` that merely looks right is how a field
//! silently changes name or order.
//!
//! **Writes go through the owner data plane, not the package's workflows.**
//! The package's `publish_post` and `create_group` are `auto`-runner LLM
//! workflows: routing an operator's HTTP POST through one would turn a
//! synchronous 201-with-a-post-id into an asynchronous run handle, which is
//! precisely the wire moving. The owner mutation path writes the same typed
//! rows directly, which is what it is for.
//!
//! **The worker's health fields are now the behavior's.** `GET /social/health`
//! retires with the engine, but `/social/policy` also reported worker state,
//! and that half of its response has to keep meaning something. It now comes
//! from the behavior scheduler: the scope pause control, and the `ambient_turn`
//! head's own state.
//!
//! It lives in `magician-api` rather than beside the retired engine because the
//! authenticated-app-scope kernel does, and there is exactly one of those.

use std::collections::HashMap;
use std::sync::{Arc, RwLock as StdRwLock};

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tracing::warn;
use uuid::Uuid;

use magician::magician_v2::apps::authority::AuthenticatedAppScope;
use magician::magician_v2::apps::background_behaviors::AppBehaviorScheduler;
use magician::magician_v2::apps::entity_adapter::AppEntityAdapterService;
use magician::magician_v2::apps::models::{
    AppComparisonOperator, AppFieldPath, AppInstallationId, AppMutationAtomicity,
    AppMutationCommand, AppMutationOperation, AppName, AppOrderDirection, AppPredicate,
    AppPredicateNode, AppProtocolVersion, AppQueryOrder, AppQueryRequest, AppRecordId,
    AppReference, AppRevision,
};
use magician::magician_v2::apps::registry::AppRegistryService;
use magician::magician_v2::social::store::{
    body_contains_exact_mention, extract_mention_handles, MAX_MENTIONS_PER_POST,
    MAX_SOCIAL_GROUP_MEMBERS,
};
use magician::magician_v2::social::types::{
    contains_secret_shaped_content, Group, Member, Post, Reaction, SelfState,
};

use crate::apps_api::authenticated_app_scope;

/// The package whose entity store now holds the corpus.
const TOWN_SQUARE_PACKAGE_ID: &str = "app:town-square";

/// The operator's own member row. `sync_roster` seeds it; every write on this
/// surface is attributed to it, exactly as the retired engine did.
const OPERATOR_MEMBER_ID: &str = "operator";

/// The behavior whose health answers the questions the worker used to.
const AMBIENT_BEHAVIOR_ID: &str = "ambient_turn";

/// Health pages walked looking for that behavior before giving up. Bounded so a
/// scope with a pathological number of heads cannot turn one policy read into
/// an unbounded scan.
const AMBIENT_HEALTH_MAX_PAGES: usize = 16;

/// The contract's hard page cap. A request above it is refused at the boundary,
/// so this is a ceiling rather than a tunable, and reads page under it.
const ENTITY_PAGE_ROWS: u32 = 200;

/// Values one `In` predicate may carry, bounded by `max_collection_items`.
/// Reads that filter on a collected id list chunk under it.
const IN_PREDICATE_VALUES: usize = 200;

/// How many rows a read may follow the cursor for, per collection.
///
/// The retired engine answered these with unbounded SQL, so any finite number
/// is a change; these are set well above what a personal square holds, and the
/// package's own `storage.max_records` bounds the corpus anyway.
const ROSTER_BUDGET: usize = 4_000;
const REACTION_BUDGET: usize = 4_000;
const MENTION_BUDGET: usize = 8_000;
const GROUP_BUDGET: usize = 4_000;

/// Defaults for the policy singleton when the operator has never saved one.
/// These are the console's and the migration's; a square that has not been
/// configured must behave the same however it got here.
const DEFAULT_COOLDOWN_SECONDS: i64 = 1_800;
const DEFAULT_MAX_POST_CHARS: i64 = 600;
const DEFAULT_MAX_AUTONOMOUS_REPLIES: i64 = 4;

#[derive(Clone)]
pub struct SocialApi {
    entities: AppEntityAdapterService,
    registry: AppRegistryService,
    /// The scheduler is optional and lock-guarded because it is registered
    /// after boot, and because `/social/policy` must answer before it is: an
    /// unregistered scheduler reads as "not configured", never as healthy.
    behaviors: Arc<StdRwLock<Option<AppBehaviorScheduler>>>,
    max_post_chars: usize,
}

/// What `/fleet/state` needs from the square.
pub struct SquareFleetProjection {
    pub posts: Vec<Post>,
    pub members: Vec<Member>,
    pub states: Vec<SelfState>,
}

/// Why a square projection could not be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SquareProjectionError {
    /// No enabled `app:town-square` installation in this scope. Distinct from
    /// unavailable because a deployment that never installed the package is
    /// not a degraded one.
    NotInstalled,
    Unavailable,
}

/// Everything a handler needs to touch the package's store, resolved once per
/// request.
struct TownSquare {
    authenticated: AuthenticatedAppScope,
    installation_id: AppInstallationId,
    schema_revision: AppRevision,
}

impl SocialApi {
    pub fn new(
        entities: AppEntityAdapterService,
        registry: AppRegistryService,
        behaviors: Arc<StdRwLock<Option<AppBehaviorScheduler>>>,
        max_post_chars: usize,
    ) -> Self {
        Self {
            entities,
            registry,
            behaviors,
            max_post_chars,
        }
    }

    /// Authenticate the request and find this scope's Town Square installation.
    ///
    /// The package is system-class, so exactly one enabled installation of it
    /// exists per scope. Resolving it by package id rather than caching a
    /// boot-time handle keeps this correct across a package update, which
    /// replaces the installation's revisions underneath.
    async fn town_square(&self, req: &HttpRequest) -> Result<TownSquare, HttpResponse> {
        let now = Utc::now();
        let authenticated = authenticated_app_scope(req, &now)?;
        self.town_square_for(authenticated, now).await
    }

    /// The same resolution for a caller that already holds an authenticated
    /// scope. `/fleet/state` projects the square through this rather than
    /// constructing a second reader over the same rows.
    async fn town_square_for(
        &self,
        authenticated: AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<TownSquare, HttpResponse> {
        // Every path below answers the owner with the same sentence, so none of
        // them may discard why. A registry `Overloaded` and an uncompiled
        // installation are the same message and neither logged, which sent an
        // investigation after a missing schema revision when the real cause was
        // the registry's 4-permit `try_acquire` losing a race.
        let installations = self
            .registry
            .enabled_installations_bounded(&authenticated, 256, now)
            .await
            .map_err(|error| {
                warn!(%error, "Town Square unavailable: enabled-installation read failed");
                town_square_unavailable()
            })?;
        // One read for every revision, not one per installation. Reading them
        // individually took one of the registry's four blocking slots each and
        // paid the read-admission wait each time, so during the boot burst a
        // workspace with a few apps timed out partway down its own list and
        // reported the square unavailable — an empty agent roster with a healthy
        // registry behind it.
        let revision_refs: Vec<_> = installations
            .iter()
            .map(|installation| installation.package_revision_ref.clone())
            .collect();
        let revisions = self
            .registry
            .package_revisions(&authenticated, &revision_refs, now)
            .await
            .map_err(|error| {
                warn!(%error, "Town Square unavailable: package-revision read failed");
                town_square_unavailable()
            })?;
        for installation in installations {
            // Absent means the revision row is gone, which is the same
            // "skip this installation" the per-item read expressed as `None`.
            let Some(revision) = revisions.get(&installation.package_revision_ref.to_string())
            else {
                continue;
            };
            if revision.package_id.as_str() != TOWN_SQUARE_PACKAGE_ID {
                continue;
            }
            // An installation with no active schema revision has been admitted
            // but not yet compiled; it cannot answer a read, and pretending
            // otherwise would surface an empty square as a healthy one.
            let Some(schema_revision) = installation.active_schema_revision else {
                warn!(
                    installation_id = %installation.installation_id,
                    package_version = %revision.semantic_version,
                    "Town Square unavailable: installation has no active schema revision \
                     (admitted but never compiled; approve it to compile)"
                );
                return Err(town_square_unavailable());
            };
            return Ok(TownSquare {
                authenticated,
                installation_id: installation.installation_id,
                schema_revision,
            });
        }
        warn!(
            "Town Square not installed: no enabled installation in this workspace names the \
             town-square package"
        );
        Err(town_square_not_installed())
    }

    /// The Town Square projection `/fleet/state` renders: recent feed posts,
    /// the roster, and each member's mood.
    ///
    /// This exists so the fleet view reads the package's entity store — the
    /// corpus's actual owner since the engine retirement — instead of the
    /// retired SQLite social store. Reading the old store did not fail; it
    /// returned the corpus frozen at the moment of retirement and reported it
    /// as `available`, which is worse than an error because nothing looks
    /// wrong.
    ///
    /// Errors are deliberately coarse. `/fleet/state` renders a section
    /// availability rather than an error body, so the only distinction it can
    /// act on is whether there is a square to read at all.
    pub async fn fleet_projection(
        &self,
        authenticated: AuthenticatedAppScope,
        feed_limit: usize,
    ) -> Result<SquareFleetProjection, SquareProjectionError> {
        let now = Utc::now();
        let square = self
            .town_square_for(authenticated, now)
            .await
            .map_err(|response| {
                if response.status() == actix_web::http::StatusCode::NOT_FOUND {
                    SquareProjectionError::NotInstalled
                } else {
                    SquareProjectionError::Unavailable
                }
            })?;

        // Same window-and-margin the feed route uses: `created_at` ordering is
        // applied by the store, exact keyset ordering in Rust.
        let predicate =
            equals("surface", json!("feed")).ok_or(SquareProjectionError::Unavailable)?;
        let rows = self
            .read(
                &square,
                "post",
                POST_FIELDS,
                Some(predicate),
                descending("created_at"),
                feed_limit.saturating_add(FEED_WINDOW_MARGIN),
            )
            .await
            .map_err(|_| SquareProjectionError::Unavailable)?;
        let mut posts = rows.iter().map(post_from).collect::<Vec<_>>();
        posts.sort_by(feed_order);
        posts.truncate(feed_limit);

        let members = self
            .read(
                &square,
                "member",
                MEMBER_FIELDS,
                None,
                Vec::new(),
                ROSTER_BUDGET,
            )
            .await
            .map_err(|_| SquareProjectionError::Unavailable)?
            .iter()
            .map(member_from)
            .collect::<Vec<_>>();

        let states = self
            .read(
                &square,
                "self_state",
                SELF_STATE_FIELDS,
                None,
                Vec::new(),
                ROSTER_BUDGET,
            )
            .await
            .map_err(|_| SquareProjectionError::Unavailable)?
            .iter()
            .map(self_state_from)
            .collect::<Vec<_>>();

        Ok(SquareFleetProjection {
            posts,
            members,
            states,
        })
    }

    /// Rows of an entity, following the store's cursor until the caller's
    /// budget is met or the data runs out.
    ///
    /// Following matters more than it looks. The contract caps ONE page at 200
    /// rows, and the retired engine answered these questions with unbounded
    /// SQL. A single unpaged read would silently drop the 201st member, the
    /// 201st reaction across a feed page, and the 201st pending mention — a
    /// wire change that shows up as data flickering in and out between polls
    /// rather than as an error.
    async fn read(
        &self,
        square: &TownSquare,
        entity: &str,
        select: &[&str],
        predicate: Option<AppPredicate>,
        order: Vec<AppQueryOrder>,
        budget: usize,
    ) -> Result<Vec<Map<String, Value>>, HttpResponse> {
        Ok(self
            .read_addressable(square, entity, select, predicate, order, budget)
            .await?
            .into_iter()
            .map(|(_, fields)| fields)
            .collect())
    }

    /// The same read, keeping each row's record id for the paths that then have
    /// to address the row (update, delete).
    async fn read_addressable(
        &self,
        square: &TownSquare,
        entity: &str,
        select: &[&str],
        predicate: Option<AppPredicate>,
        order: Vec<AppQueryOrder>,
        budget: usize,
    ) -> Result<Vec<(AppRecordId, Map<String, Value>)>, HttpResponse> {
        let now = Utc::now();
        let entity_name =
            AppName::parse(entity).map_err(|error| read_failed_with(entity, &error))?;
        let select = select
            .iter()
            .map(|field| AppFieldPath::parse(*field))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| read_failed_with(entity, &error))?;
        let purpose =
            AppName::parse("social_http").map_err(|error| read_failed_with(entity, &error))?;
        let budget = budget.max(1);

        // CONSTANT across every page of one read. `limit` has no
        // `skip_serializing_if`, so it is inside `query_identity_digest`, and a
        // follow-up page whose limit differs from the one that minted the
        // cursor is rejected as a stale/corrupt cursor. Shrinking the limit as
        // the budget filled looked like good manners and turned the second page
        // of any read whose budget was not a multiple of the page cap into a
        // 500 -- which was every feed request on a square with more than 200
        // posts.
        let page_rows = budget.min(ENTITY_PAGE_ROWS as usize) as u32;
        let mut rows = Vec::new();
        let mut cursor = None;
        loop {
            if rows.len() >= budget {
                break;
            }
            let request = AppQueryRequest {
                pagination: Default::default(),
                protocol_version: AppProtocolVersion::V1,
                source_installation_id: square.installation_id.clone(),
                entity: entity_name.clone(),
                select: select.clone(),
                predicate: predicate.clone(),
                order: order.clone(),
                cursor: cursor.clone(),
                limit: page_rows,
                relation_expansions: Vec::new(),
                purpose: purpose.clone(),
            };
            let page = self
                .entities
                .owner_query(&square.authenticated, request, now)
                .await
                .map_err(|error| read_failed_with(entity, &error))?;
            for projection in page.envelope.value {
                let fields = projection
                    .fields
                    .into_iter()
                    .map(|(path, value)| (path.as_str().to_owned(), value))
                    .collect::<Map<String, Value>>();
                rows.push((projection.record_id, fields));
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        // The budget bounds the read, not the page: trim anything the last page
        // carried past it.
        rows.truncate(budget);
        Ok(rows)
    }

    /// Apply one all-or-nothing mutation to the package's store.
    async fn write(
        &self,
        square: &TownSquare,
        area: &'static str,
        idempotency_key: String,
        operations: Vec<AppMutationOperation>,
    ) -> Result<(), HttpResponse> {
        let now = Utc::now();
        let command = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse(idempotency_key)
                .map_err(|error| write_failed_with(area, &error))?,
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: square.schema_revision,
            operations,
            expected_record_revisions: Vec::new(),
        };
        self.entities
            .owner_mutate(&square.authenticated, &square.installation_id, command, now)
            .await
            .map(|_| ())
            .map_err(|error| write_failed_with(area, &error))
    }
}

// ---------------------------------------------------------------------------
// Row projection
//
// Every response is built by filling the ORIGINAL `social::types` structs and
// serialising those. Reconstructing the JSON by hand would look right and drift
// silently the first time a field is renamed; going through the same types the
// retired engine used makes the wire identical by construction.
// ---------------------------------------------------------------------------

fn text(row: &Map<String, Value>, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn opt_text(row: &Map<String, Value>, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
}

fn decimal(row: &Map<String, Value>, key: &str) -> f64 {
    row.get(key).and_then(Value::as_f64).unwrap_or_default()
}

fn flag(row: &Map<String, Value>, key: &str) -> bool {
    row.get(key).and_then(Value::as_bool).unwrap_or_default()
}

const POST_FIELDS: &[&str] = &[
    "post_id",
    "author_id",
    "surface",
    "group_id",
    "post_type",
    "body",
    "parent_id",
    "created_at",
];

fn post_from(row: &Map<String, Value>) -> Post {
    Post {
        post_id: text(row, "post_id"),
        author_id: text(row, "author_id"),
        surface: text(row, "surface"),
        group_id: opt_text(row, "group_id"),
        post_type: text(row, "post_type"),
        body: text(row, "body"),
        parent_id: opt_text(row, "parent_id"),
        created_at: text(row, "created_at"),
    }
}

const MEMBER_FIELDS: &[&str] = &[
    "member_id",
    "kind",
    "display_name",
    "introversion",
    "opted_out",
    "created_at",
];

fn member_from(row: &Map<String, Value>) -> Member {
    Member {
        member_id: text(row, "member_id"),
        kind: text(row, "kind"),
        display_name: text(row, "display_name"),
        introversion: decimal(row, "introversion"),
        opted_out: flag(row, "opted_out"),
        created_at: text(row, "created_at"),
    }
}

const SELF_STATE_FIELDS: &[&str] = &[
    "member_id",
    "valence",
    "energy",
    "baseline_valence",
    "baseline_energy",
    "note",
    "updated_at",
];

fn self_state_from(row: &Map<String, Value>) -> SelfState {
    SelfState {
        member_id: text(row, "member_id"),
        valence: decimal(row, "valence"),
        energy: decimal(row, "energy"),
        baseline_valence: decimal(row, "baseline_valence"),
        baseline_energy: decimal(row, "baseline_energy"),
        note: opt_text(row, "note"),
        updated_at: text(row, "updated_at"),
    }
}

const GROUP_FIELDS: &[&str] = &["group_id", "name", "created_by", "created_at"];

fn group_from(row: &Map<String, Value>) -> Group {
    Group {
        group_id: text(row, "group_id"),
        name: text(row, "name"),
        created_by: text(row, "created_by"),
        created_at: text(row, "created_at"),
    }
}

const REACTION_FIELDS: &[&str] = &["post_id", "member_id", "emoji", "created_at"];

fn reaction_from(row: &Map<String, Value>) -> Reaction {
    Reaction {
        post_id: text(row, "post_id"),
        member_id: text(row, "member_id"),
        emoji: text(row, "emoji"),
        created_at: text(row, "created_at"),
    }
}

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

fn compare(field: &str, operator: AppComparisonOperator, value: Value) -> Option<AppPredicateNode> {
    Some(AppPredicateNode::Compare {
        field: AppFieldPath::parse(field).ok()?,
        operator,
        value,
    })
}

fn equals(field: &str, value: Value) -> Option<AppPredicate> {
    Some(AppPredicate {
        root: 0,
        nodes: vec![compare(field, AppComparisonOperator::Equal, value)?],
    })
}

fn ascending(field: &str) -> Vec<AppQueryOrder> {
    AppFieldPath::parse(field)
        .map(|field| {
            vec![AppQueryOrder {
                field,
                direction: AppOrderDirection::Ascending,
            }]
        })
        .unwrap_or_default()
}

fn descending(field: &str) -> Vec<AppQueryOrder> {
    AppFieldPath::parse(field)
        .map(|field| {
            vec![AppQueryOrder {
                field,
                direction: AppOrderDirection::Descending,
            }]
        })
        .unwrap_or_default()
}

/// `surface = 'feed'`, and no newer than the cursor's timestamp when one is
/// supplied.
///
/// The composite `(created_at, post_id)` keyset the retired store paged on
/// cannot be expressed as a predicate here: `validate_operator` admits the
/// ordering comparisons only for integer, decimal and timestamp fields, and
/// `post_id` is text — so `post_id < cursor` is an `OperatorTypeMismatch`. It
/// is refused whether or not the branch containing it is reachable, because the
/// validator walks the node arena FLAT rather than from the root. An earlier
/// cut spelled the keyset out that way and turned every `?before=` request into
/// a 503.
///
/// So the predicate narrows on the timestamp only — `created_at <= cursor`,
/// which is legal on a timestamp — and the exact keyset boundary is applied in
/// Rust over the returned window, where the full composite key is available.
fn feed_predicate(cursor: Option<&(String, String)>) -> Option<AppPredicate> {
    let feed = compare("surface", AppComparisonOperator::Equal, json!("feed"))?;
    let Some((created_at, _)) = cursor else {
        return Some(AppPredicate {
            root: 0,
            nodes: vec![feed],
        });
    };
    Some(AppPredicate {
        root: 0,
        nodes: vec![
            AppPredicateNode::All {
                children: vec![1, 2],
            },
            feed,
            compare(
                "created_at",
                AppComparisonOperator::LessThanOrEqual,
                json!(created_at),
            )?,
        ],
    })
}

/// Extra rows fetched beyond a feed page.
///
/// The store chooses its window with `(created_at DESC, record_id ASC)` — its
/// own tiebreak, an opaque host-minted id — while the response is ordered by
/// `(created_at DESC, post_id DESC)`. Those are different total orders, so a
/// window sized exactly to the page can truncate a post that the response order
/// would have placed inside it. Over-fetching by this margin and ordering the
/// superset closes the gap for any timestamp shared by fewer than this many
/// posts, which is every realistic square: the corpus timestamps to the
/// millisecond and this is a personal feed.
const FEED_WINDOW_MARGIN: usize = 200;

/// Order a page the way the retired engine's `ORDER BY created_at DESC,
/// post_id DESC` did.
fn feed_order(left: &Post, right: &Post) -> std::cmp::Ordering {
    right
        .created_at
        .cmp(&left.created_at)
        .then_with(|| right.post_id.cmp(&left.post_id))
}

fn any_of(field: &str, values: Vec<Value>) -> Option<AppPredicate> {
    Some(AppPredicate {
        root: 0,
        nodes: vec![AppPredicateNode::In {
            field: AppFieldPath::parse(field).ok()?,
            values,
        }],
    })
}

// ---------------------------------------------------------------------------
// Shared response shapes, unchanged from the retired engine
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct PostWithReactions {
    #[serde(flatten)]
    pub post: Post,
    pub reactions: Vec<Reaction>,
}

#[derive(Serialize)]
pub struct FeedResponse {
    pub posts: Vec<PostWithReactions>,
    // No `skip_serializing_if`, deliberately: the retired engine always emitted
    // this key, `null` on the last page. Omitting it would be a wire change
    // that reads as a tidy-up.
    pub next_before: Option<String>,
}

// `member` is NOT flattened and `post` above IS. That asymmetry is the retired
// engine's, and it is the whole point of copying these declarations rather than
// rewriting them from what the JSON looks like.
#[derive(Serialize)]
pub struct MemberWithMood {
    pub member: Member,
    pub mood: Option<SelfState>,
    pub pending_reply_notifications: usize,
}

/// The retired engine answered a failed read with 500 `social_read_failed` and
/// a failed write with 500 `social_write_failed`, naming the area. Collapsing
/// both into 503 would have been a wire change on every failure path, and the
/// UI prints `message` verbatim.
///
/// Both log. The retired handlers emitted `warn!(area, %error, ...)` on every
/// failure; a 500 that carries no diagnostic is a 500 nobody can chase.
fn read_failed_with(area: &str, error: &dyn std::fmt::Display) -> HttpResponse {
    warn!(area, %error, "Town Square read failed");
    read_failed(area)
}

fn write_failed_with(area: &str, error: &dyn std::fmt::Display) -> HttpResponse {
    warn!(area, %error, "Town Square write failed");
    write_failed(area)
}

fn read_failed(area: &str) -> HttpResponse {
    HttpResponse::InternalServerError().json(json!({
        "error": "social_read_failed",
        "message": format!("Town Square could not load {area}.")
    }))
}

fn write_failed(area: &str) -> HttpResponse {
    HttpResponse::InternalServerError().json(json!({
        "error": "social_write_failed",
        "message": format!("Town Square could not save this {area}.")
    }))
}

fn town_square_unavailable() -> HttpResponse {
    HttpResponse::ServiceUnavailable().json(json!({
        "error": "social_store_unavailable",
        "message": "Town Square storage is unavailable for this workspace."
    }))
}

fn town_square_not_installed() -> HttpResponse {
    HttpResponse::ServiceUnavailable().json(json!({
        "error": "social_store_unavailable",
        "message": "Town Square is not installed in this workspace."
    }))
}

fn invalid_feed_cursor() -> HttpResponse {
    HttpResponse::BadRequest().json(json!({
        "error": "invalid_feed_cursor",
        "message": "The Town Square page cursor is invalid. Refresh the feed and try again."
    }))
}

fn encode_feed_cursor(post: &Post) -> String {
    format!("{}|{}", post.created_at, post.post_id)
}

fn parse_feed_cursor(cursor: &str) -> Result<(String, String), HttpResponse> {
    let Some((created_at, post_id)) = cursor.rsplit_once('|') else {
        return Err(invalid_feed_cursor());
    };
    if cursor.len() > 512
        || created_at.is_empty()
        || post_id.is_empty()
        || chrono::DateTime::parse_from_rfc3339(created_at).is_err()
    {
        return Err(invalid_feed_cursor());
    }
    Ok((created_at.to_string(), post_id.to_string()))
}

/// Reactions for one page of posts, grouped by post.
async fn reactions_for(
    api: &SocialApi,
    square: &TownSquare,
    posts: &[Post],
) -> Result<HashMap<String, Vec<Reaction>>, HttpResponse> {
    if posts.is_empty() {
        return Ok(HashMap::new());
    }
    let mut grouped: HashMap<String, Vec<Reaction>> = HashMap::new();
    for chunk in posts.chunks(IN_PREDICATE_VALUES) {
        let Some(predicate) = any_of(
            "post_id",
            chunk.iter().map(|post| json!(post.post_id)).collect(),
        ) else {
            return Err(read_failed("reaction"));
        };
        let rows = api
            .read(
                square,
                "reaction",
                REACTION_FIELDS,
                Some(predicate),
                ascending("created_at"),
                REACTION_BUDGET,
            )
            .await?;
        for row in &rows {
            let reaction = reaction_from(row);
            grouped
                .entry(reaction.post_id.clone())
                .or_default()
                .push(reaction);
        }
    }
    Ok(grouped)
}

fn with_reactions(
    posts: Vec<Post>,
    mut reactions: HashMap<String, Vec<Reaction>>,
) -> Vec<PostWithReactions> {
    posts
        .into_iter()
        .map(|post| {
            let post_reactions = reactions.remove(&post.post_id).unwrap_or_default();
            PostWithReactions {
                post,
                reactions: post_reactions,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Reads
//
// Orderings are applied in Rust after the page comes back, not left to the
// store. The retired engine ordered with SQLite's collation over composite
// keys; reproducing that exactly matters because it is the array order the UI
// renders, and it is cheaper to sort a bounded page here than to assume two
// engines collate identically.
// ---------------------------------------------------------------------------

#[derive(Default, Deserialize)]
pub struct FeedQuery {
    pub limit: Option<usize>,
    pub before: Option<String>,
}

pub async fn get_feed(
    req: HttpRequest,
    api: web::Data<SocialApi>,
    query: web::Query<FeedQuery>,
) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let cursor = match query.before.as_deref().map(parse_feed_cursor).transpose() {
        Ok(cursor) => cursor,
        Err(response) => return response,
    };
    let Some(predicate) = feed_predicate(cursor.as_ref()) else {
        return town_square_unavailable();
    };
    let rows = match api
        .read(
            &square,
            "post",
            POST_FIELDS,
            Some(predicate),
            descending("created_at"),
            limit + 1 + FEED_WINDOW_MARGIN,
        )
        .await
    {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let mut posts = rows.iter().map(post_from).collect::<Vec<_>>();
    posts.sort_by(feed_order);
    // The predicate could only narrow to `created_at <= cursor`, so the window
    // still holds the cursor row itself and anything sharing its timestamp that
    // sorts before it. Applying the exact composite boundary here is what keeps
    // two posts written in the same millisecond from hiding each other across a
    // page edge.
    if let Some((cursor_created_at, cursor_post_id)) = cursor.as_ref() {
        posts
            .retain(|post| (&post.created_at, &post.post_id) < (cursor_created_at, cursor_post_id));
    }
    let has_more = posts.len() > limit;
    posts.truncate(limit);
    let next_before = has_more
        .then(|| posts.last().map(encode_feed_cursor))
        .flatten();
    let reactions = match reactions_for(&api, &square, &posts).await {
        Ok(reactions) => reactions,
        Err(response) => return response,
    };
    HttpResponse::Ok().json(FeedResponse {
        posts: with_reactions(posts, reactions),
        next_before,
    })
}

pub async fn get_groups(req: HttpRequest, api: web::Data<SocialApi>) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let groups = match operator_groups(&api, &square).await {
        Ok(groups) => groups,
        Err(response) => return response,
    };
    HttpResponse::Ok().json(json!({ "groups": groups }))
}

/// The groups the operator belongs to, in the retired engine's order
/// (`created_at DESC, group_id DESC`).
async fn operator_groups(api: &SocialApi, square: &TownSquare) -> Result<Vec<Group>, HttpResponse> {
    let Some(predicate) = equals("member_id", json!(OPERATOR_MEMBER_ID)) else {
        return Err(town_square_unavailable());
    };
    let memberships = api
        .read(
            square,
            "group_membership",
            &["membership_id", "group_id", "member_id"],
            Some(predicate),
            Vec::new(),
            GROUP_BUDGET,
        )
        .await?;
    let ids = memberships
        .iter()
        .map(|row| text(row, "group_id"))
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // Chunked, because `In` is bounded by `max_collection_items` and a scope
    // with more groups than that would turn every group route into a contract
    // error rather than a longer read. The retired engine did this as a SQL
    // join with no ceiling.
    let mut groups = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(IN_PREDICATE_VALUES) {
        let Some(predicate) = any_of(
            "group_id",
            chunk.iter().map(|id| json!(id)).collect::<Vec<_>>(),
        ) else {
            return Err(read_failed("group"));
        };
        let rows = api
            .read(
                square,
                "group",
                GROUP_FIELDS,
                Some(predicate),
                Vec::new(),
                GROUP_BUDGET,
            )
            .await?;
        groups.extend(rows.iter().map(group_from));
    }
    groups.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.group_id.cmp(&left.group_id))
    });
    Ok(groups)
}

async fn operator_is_group_member(
    api: &SocialApi,
    square: &TownSquare,
    group_id: &str,
) -> Result<bool, HttpResponse> {
    Ok(operator_groups(api, square)
        .await?
        .iter()
        .any(|group| group.group_id == group_id))
}

pub async fn get_group_posts(
    req: HttpRequest,
    api: web::Data<SocialApi>,
    path: web::Path<String>,
) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let group_id = path.into_inner();
    match operator_is_group_member(&api, &square, &group_id).await {
        Ok(true) => {},
        Ok(false) => {
            return HttpResponse::Forbidden().json(json!({
                "error": "not_a_group_member",
                "message": "This group is private to its members."
            }))
        },
        Err(response) => return response,
    }
    let Some(predicate) = equals("group_id", json!(group_id)) else {
        return town_square_unavailable();
    };
    let rows = match api
        .read(
            &square,
            "post",
            POST_FIELDS,
            Some(predicate),
            descending("created_at"),
            50 + FEED_WINDOW_MARGIN,
        )
        .await
    {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let mut posts = rows.iter().map(post_from).collect::<Vec<_>>();
    posts.sort_by(feed_order);
    posts.truncate(50);
    let reactions = match reactions_for(&api, &square, &posts).await {
        Ok(reactions) => reactions,
        Err(response) => return response,
    };
    HttpResponse::Ok().json(FeedResponse {
        posts: with_reactions(posts, reactions),
        next_before: None,
    })
}

pub async fn get_members(req: HttpRequest, api: web::Data<SocialApi>) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let rows = match api
        .read(
            &square,
            "member",
            MEMBER_FIELDS,
            None,
            Vec::new(),
            ROSTER_BUDGET,
        )
        .await
    {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    // The owner roster includes opted-out agents so visibility does not imply
    // participation. Mention delivery and ambient eligibility keep their own
    // enrolled/opt-out checks; listing an agent cannot opt it in.
    let mut members = rows.iter().map(member_from).collect::<Vec<_>>();
    members.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.display_name.cmp(&right.display_name))
            .then_with(|| left.member_id.cmp(&right.member_id))
    });

    let states = match api
        .read(
            &square,
            "self_state",
            SELF_STATE_FIELDS,
            None,
            Vec::new(),
            ROSTER_BUDGET,
        )
        .await
    {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let states = states
        .iter()
        .map(|row| {
            let state = self_state_from(row);
            (state.member_id.clone(), state)
        })
        .collect::<HashMap<_, _>>();

    let counts = match pending_reply_notification_counts(&api, &square).await {
        Ok(counts) => counts,
        Err(response) => return response,
    };

    HttpResponse::Ok().json(json!({
        "members": members
            .into_iter()
            .map(|member| {
                let mood = states.get(&member.member_id).cloned();
                let pending_reply_notifications =
                    counts.get(&member.member_id).copied().unwrap_or(0);
                MemberWithMood { member, mood, pending_reply_notifications }
            })
            .collect::<Vec<_>>()
    }))
}

/// `delivery_kind = 'reply_notification' AND status = 'pending'`, counted per
/// mentioned member.
async fn pending_reply_notification_counts(
    api: &SocialApi,
    square: &TownSquare,
) -> Result<HashMap<String, usize>, HttpResponse> {
    let predicate = AppPredicate {
        root: 0,
        nodes: vec![
            AppPredicateNode::All {
                children: vec![1, 2],
            },
            match compare(
                "delivery_kind",
                AppComparisonOperator::Equal,
                json!("reply_notification"),
            ) {
                Some(node) => node,
                None => return Err(town_square_unavailable()),
            },
            match compare("status", AppComparisonOperator::Equal, json!("pending")) {
                Some(node) => node,
                None => return Err(town_square_unavailable()),
            },
        ],
    };
    let rows = api
        .read(
            square,
            "mention",
            &[
                "mention_id",
                "mentioned_member_id",
                "delivery_kind",
                "status",
            ],
            Some(predicate),
            Vec::new(),
            MENTION_BUDGET,
        )
        .await?;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for row in &rows {
        *counts.entry(text(row, "mentioned_member_id")).or_default() += 1;
    }
    Ok(counts)
}

pub async fn get_member_self(
    req: HttpRequest,
    api: web::Data<SocialApi>,
    path: web::Path<String>,
) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let member_id = path.into_inner();
    let Some(predicate) = equals("member_id", json!(member_id)) else {
        return town_square_unavailable();
    };
    let rows = match api
        .read(
            &square,
            "self_state",
            SELF_STATE_FIELDS,
            Some(predicate),
            Vec::new(),
            1,
        )
        .await
    {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    match rows.first() {
        Some(row) => HttpResponse::Ok().json(json!({ "state": self_state_from(row) })),
        None => HttpResponse::NotFound().json(json!({ "error": "self_state_not_found" })),
    }
}

pub async fn get_square_ambient(req: HttpRequest, api: web::Data<SocialApi>) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let Some(predicate) = equals("surface", json!("feed")) else {
        return town_square_unavailable();
    };
    let rows = match api
        .read(
            &square,
            "post",
            POST_FIELDS,
            Some(predicate),
            descending("created_at"),
            10 + FEED_WINDOW_MARGIN,
        )
        .await
    {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let mut posts = rows.iter().map(post_from).collect::<Vec<_>>();
    posts.sort_by(feed_order);
    posts.truncate(10);

    let member_rows = match api
        .read(
            &square,
            "member",
            MEMBER_FIELDS,
            None,
            Vec::new(),
            ROSTER_BUDGET,
        )
        .await
    {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let members = member_rows
        .iter()
        .map(|row| {
            let member = member_from(row);
            (member.member_id.clone(), member)
        })
        .collect::<HashMap<_, _>>();

    let state_rows = match api
        .read(
            &square,
            "self_state",
            SELF_STATE_FIELDS,
            None,
            Vec::new(),
            ROSTER_BUDGET,
        )
        .await
    {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let states = state_rows
        .iter()
        .map(|row| {
            let state = self_state_from(row);
            (state.member_id.clone(), state.valence)
        })
        .collect::<HashMap<_, _>>();

    HttpResponse::Ok().json(json!({
        "ambient_signals": posts
            .into_iter()
            .map(|post| {
                let author_id = post.author_id;
                json!({
                    "author_id": author_id,
                    "display_name": members
                        .get(&author_id)
                        .map(|member| member.display_name.clone())
                        .unwrap_or_default(),
                    "mood": states.get(&author_id).copied().unwrap_or(0.0),
                    "recent_activity": post.body.chars().take(50).collect::<String>(),
                })
            })
            .collect::<Vec<_>>()
    }))
}

// ---------------------------------------------------------------------------
// Writes
//
// Through the owner data plane, not the package's workflows. `publish_post` and
// `create_group` are `auto`-runner LLM workflows: sending an operator's POST
// through one would turn a synchronous 201-with-an-id into an asynchronous run
// handle, and that is the wire moving. The workflows exist for the agent-driven
// path; this is the operator's.
// ---------------------------------------------------------------------------

fn create_op(entity: &str, temporary_id: &str, payload: Value) -> Option<AppMutationOperation> {
    Some(AppMutationOperation::Create {
        entity: AppName::parse(entity).ok()?,
        temporary_id: AppName::parse(temporary_id).ok()?,
        record_id: None,
        payload,
    })
}

#[derive(Deserialize)]
pub struct CreatePostRequest {
    pub surface: String,
    pub group_id: Option<String>,
    pub post_type: String,
    pub body: String,
    pub parent_id: Option<String>,
    #[serde(default)]
    pub mentioned_member_ids: Vec<String>,
}

pub async fn create_post(
    req: HttpRequest,
    api: web::Data<SocialApi>,
    body: web::Json<CreatePostRequest>,
) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    if body.mentioned_member_ids.len() > MAX_MENTIONS_PER_POST {
        return HttpResponse::BadRequest().json(json!({
            "error": "too_many_mention_targets",
            "message": format!(
                "A Town Square post may notify at most {MAX_MENTIONS_PER_POST} agents."
            )
        }));
    }
    let post_body = body.body.trim();
    if post_body.is_empty() || post_body.chars().count() > api.max_post_chars {
        return HttpResponse::BadRequest().json(json!({
            "error": "invalid_post",
            "message": format!("Post text must contain 1–{} characters.", api.max_post_chars)
        }));
    }
    // The secret boundary stays HOST machinery and keeps its place on the write
    // path. A package that owns the corpus must not be able to publish a
    // provider key, and this is an oracle rather than a redaction: a post that
    // trips it is refused, never silently rewritten.
    if contains_secret_shaped_content(post_body) {
        return HttpResponse::BadRequest().json(json!({
            "error": "unsafe_post",
            "message": "Town Square posts cannot contain credential-shaped content."
        }));
    }
    if !matches!(
        body.post_type.as_str(),
        "thought" | "reply" | "question" | "link"
    ) {
        return HttpResponse::BadRequest().json(json!({
            "error": "invalid_post_type",
            "message": "Post type must be thought, reply, question, or link."
        }));
    }
    match (body.post_type.as_str(), body.parent_id.as_deref()) {
        ("reply", None) => {
            return HttpResponse::BadRequest().json(json!({
                "error": "missing_reply_parent",
                "message": "A reply must reference its parent post."
            }))
        },
        ("reply", Some(_)) | (_, None) => {},
        (_, Some(_)) => {
            return HttpResponse::BadRequest().json(json!({
                "error": "unexpected_reply_parent",
                "message": "Only replies may reference a parent post."
            }))
        },
    }
    if body.surface == "group" {
        let Some(group_id) = body.group_id.as_deref() else {
            return HttpResponse::BadRequest().json(json!({"error": "missing_group"}));
        };
        match operator_is_group_member(&api, &square, group_id).await {
            Ok(true) => {},
            Ok(false) => {
                return HttpResponse::Forbidden().json(json!({"error": "not_a_group_member"}))
            },
            Err(response) => return response,
        }
    } else if body.surface != "feed" || body.group_id.is_some() {
        return HttpResponse::BadRequest().json(json!({"error": "invalid_surface"}));
    }

    // A reply's parent must exist AND sit on the same surface and group. The
    // retired `validate_reply_parent` enforced both; existence alone lets a
    // public feed reply be parented to a private group post, which threads
    // group content into a feed that cannot show its parent.
    let mut reply_parent_author: Option<String> = None;
    if let Some(parent_id) = body.parent_id.as_deref() {
        let Some(predicate) = equals("post_id", json!(parent_id)) else {
            return read_failed("post");
        };
        let parent = match api
            .read(&square, "post", POST_FIELDS, Some(predicate), Vec::new(), 1)
            .await
        {
            Ok(rows) => rows.first().map(post_from),
            Err(response) => return response,
        };
        let same_surface = parent.as_ref().is_some_and(|parent| {
            parent.surface == body.surface && parent.group_id == body.group_id
        });
        if !same_surface {
            return HttpResponse::BadRequest().json(json!({
                "error": "invalid_reply_parent",
                "message": "social reply parent is absent or outside the target surface"
            }));
        }
        reply_parent_author = parent.map(|parent| parent.author_id);
    }

    // Mention fan-out, reproducing the retired engine's rules rather than
    // "any member id that is not the author". Each of these was load-bearing:
    //
    //   - group posts produce NO deliveries at all, so private-group text never
    //     notifies anyone;
    //   - an explicit target must be named verbatim in the body, or the whole
    //     request is refused — a delivery with no visible cause is worse than
    //     no delivery;
    //   - a recipient must be an enrolled agent, so opted-out agents and the
    //     operator's own row are never targets;
    //   - `@handle` tokens typed in prose notify too, which is how the UI's own
    //     mention affordance works.
    let roster = match api
        .read(
            &square,
            "member",
            MEMBER_FIELDS,
            None,
            Vec::new(),
            ROSTER_BUDGET,
        )
        .await
    {
        Ok(rows) => rows.iter().map(member_from).collect::<Vec<_>>(),
        Err(response) => return response,
    };
    let eligible = |candidate: &str| {
        roster.iter().any(|member| {
            member.member_id == candidate && member.kind == "agent" && !member.opted_out
        })
    };

    let mut mentioned: Vec<String> = Vec::new();
    if body.surface == "feed" {
        for candidate in &body.mentioned_member_ids {
            // The author naming themselves is skipped, not refused -- the
            // retired store did `continue` here, and turning that into a 400
            // would reject posts it accepted.
            if candidate == OPERATOR_MEMBER_ID {
                continue;
            }
            // Body-visibility BEFORE eligibility, and with the retired wording.
            // The order is observable: a target that is both unnamed and
            // ineligible produced the body-visibility message, and the page
            // prints `message` verbatim.
            if !body_contains_exact_mention(post_body, candidate) {
                return HttpResponse::BadRequest().json(json!({
                    "error": "invalid_mention_target",
                    "message": "explicit social mention target is not visible in the post body"
                }));
            }
            if !eligible(candidate) {
                return HttpResponse::BadRequest().json(json!({
                    "error": "invalid_mention_target",
                    "message": "explicit social mention target is not an eligible roster member"
                }));
            }
            if !mentioned.iter().any(|existing| existing == candidate) {
                mentioned.push(candidate.clone());
            }
        }
        for handle in extract_mention_handles(post_body) {
            if mentioned.len() >= MAX_MENTIONS_PER_POST {
                break;
            }
            if eligible(&handle) && !mentioned.iter().any(|existing| *existing == handle) {
                mentioned.push(handle);
            }
        }
    }

    let post_id = Uuid::new_v4().to_string();
    let now_rfc3339 = Utc::now().to_rfc3339();
    let mut operations = Vec::with_capacity(1 + mentioned.len());
    let Some(post_op) = create_op(
        "post",
        "post_row",
        json!({
            "post_id": post_id,
            "author_id": OPERATOR_MEMBER_ID,
            "surface": body.surface,
            "group_id": body.group_id,
            "post_type": body.post_type,
            "body": post_body,
            "parent_id": body.parent_id,
            "created_at": now_rfc3339,
            "synced_at": now_rfc3339,
        }),
    ) else {
        return town_square_unavailable();
    };
    operations.push(post_op);
    for (index, member_id) in mentioned.iter().enumerate() {
        let Some(op) = create_op(
            "mention",
            &format!("mention_{index}"),
            json!({
                "mention_id": format!("{post_id}:{member_id}"),
                "post_id": post_id,
                "mentioned_member_id": member_id,
                "delivery_kind": "explicit_mention",
                "status": "pending",
                "created_at": now_rfc3339,
                "handled_at": Value::Null,
                "response_post_id": Value::Null,
            }),
        ) else {
            return write_failed("post");
        };
        operations.push(op);
    }

    // A reply tells the parent's author that a reply exists. Informational by
    // construction — the package's behavior must never produce an autonomous
    // reply from one — but without it `pending_reply_notifications` on
    // `/social/members` is a badge nothing can ever increment, since the worker
    // that used to write these is gone.
    //
    // Gated on the feed surface for the same reason the mention fan-out is: the
    // retired store returned before ANY delivery for a non-feed post, so
    // private-group text never produced a durable row the behavior could act
    // on. A reply notification is a delivery like any other.
    if let Some(parent_author) = reply_parent_author
        .as_deref()
        .filter(|_| body.surface == "feed")
    {
        if parent_author != OPERATOR_MEMBER_ID && eligible(parent_author) {
            let Some(op) = create_op(
                "mention",
                "reply_notification",
                json!({
                    "mention_id": format!("{post_id}:{parent_author}"),
                    "post_id": post_id,
                    "mentioned_member_id": parent_author,
                    "delivery_kind": "reply_notification",
                    "status": "pending",
                    "created_at": now_rfc3339,
                    "handled_at": Value::Null,
                    "response_post_id": Value::Null,
                }),
            ) else {
                return write_failed("post");
            };
            if !mentioned.iter().any(|existing| existing == parent_author) {
                operations.push(op);
            }
        }
    }

    // The post id IS the idempotency key: a retried POST is the same post.
    match api
        .write(
            &square,
            "post",
            format!("social:post:{post_id}"),
            operations,
        )
        .await
    {
        Ok(()) => HttpResponse::Created().json(json!({
            "status": "success",
            "post_id": post_id,
            "mentioned_member_ids": mentioned
        })),
        Err(response) => response,
    }
}

#[derive(Deserialize)]
pub struct AddReactionRequest {
    pub emoji: String,
}

pub async fn add_reaction(
    req: HttpRequest,
    api: web::Data<SocialApi>,
    path: web::Path<String>,
    body: web::Json<AddReactionRequest>,
) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let emoji = body.emoji.trim();
    if emoji.is_empty() || emoji.chars().count() > 16 {
        return HttpResponse::BadRequest().json(json!({
            "error": "invalid_reaction",
            "message": "Choose a short, non-empty reaction."
        }));
    }
    let post_id = path.into_inner();
    // Visibility, not mere existence. The retired handler asked
    // `is_post_visible_to`, so a post in a private group the operator does not
    // belong to answered 404. Existence alone would let them react to a group
    // post they cannot read — and the reaction would then be visible to the
    // members who can.
    match visible_post(&api, &square, &post_id).await {
        Ok(Some(_)) => {},
        Ok(None) => {
            return HttpResponse::NotFound().json(json!({
                "error": "post_not_found",
                "message": "That Town Square post is unavailable in this workspace."
            }))
        },
        Err(response) => return response,
    }
    let reaction_id = format!("{post_id}:{OPERATOR_MEMBER_ID}:{emoji}");
    // One reaction per (post, member, emoji). The retired store used
    // `INSERT OR REPLACE`, so a second click was a benign 200. Here a repeat
    // has to be detected rather than replayed: the payload carries a fresh
    // `created_at`, so the same idempotency key with different bytes is an
    // `IdempotencyConflict`, not a replay.
    let Some(predicate) = equals("reaction_id", json!(reaction_id)) else {
        return read_failed("reaction");
    };
    match api
        .read(
            &square,
            "reaction",
            REACTION_FIELDS,
            Some(predicate),
            Vec::new(),
            1,
        )
        .await
    {
        Ok(rows) if !rows.is_empty() => {
            return HttpResponse::Ok().json(json!({ "status": "success" }))
        },
        Ok(_) => {},
        Err(response) => return response,
    }
    let Some(op) = create_op(
        "reaction",
        "reaction_row",
        json!({
            "reaction_id": reaction_id,
            "post_id": post_id,
            "member_id": OPERATOR_MEMBER_ID,
            "emoji": emoji,
            "created_at": Utc::now().to_rfc3339(),
        }),
    ) else {
        return write_failed("reaction");
    };
    // A FRESH key per write, not one derived from the reaction.
    //
    // A key stable per `(post, member, emoji)` looked right and was a trap: the
    // payload carries a fresh `created_at`, so react -> un-react -> react
    // replays the same key with different bytes and the store answers
    // `IdempotencyConflict` -- permanently, for that post and emoji. The
    // existence probe above is what makes a repeat click idempotent; the key
    // does not need to. The residual race (two simultaneous first clicks both
    // seeing nothing) is a duplicate row the UI's count aggregation absorbs,
    // which is what the retired store's `INSERT OR REPLACE` did too.
    match api
        .write(
            &square,
            "reaction",
            format!("social:reaction:{}", Uuid::new_v4()),
            vec![op],
        )
        .await
    {
        Ok(()) => HttpResponse::Ok().json(json!({ "status": "success" })),
        Err(response) => response,
    }
}

/// A post the operator may actually see: on the feed, or in a group they belong
/// to. Reproduces the retired `is_post_visible_to`.
async fn visible_post(
    api: &SocialApi,
    square: &TownSquare,
    post_id: &str,
) -> Result<Option<Post>, HttpResponse> {
    let Some(predicate) = equals("post_id", json!(post_id)) else {
        return Err(read_failed("post"));
    };
    let rows = api
        .read(square, "post", POST_FIELDS, Some(predicate), Vec::new(), 1)
        .await?;
    let Some(post) = rows.first().map(post_from) else {
        return Ok(None);
    };
    if post.surface != "group" {
        return Ok(Some(post));
    }
    let Some(group_id) = post.group_id.clone() else {
        return Ok(None);
    };
    if operator_is_group_member(api, square, &group_id).await? {
        Ok(Some(post))
    } else {
        Ok(None)
    }
}

pub async fn delete_reaction(
    req: HttpRequest,
    api: web::Data<SocialApi>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let (post_id, emoji) = path.into_inner();
    let reaction_id = format!("{post_id}:{OPERATOR_MEMBER_ID}:{emoji}");
    let Some(predicate) = equals("reaction_id", json!(reaction_id)) else {
        return town_square_unavailable();
    };
    let rows = match api
        .read_addressable(
            &square,
            "reaction",
            REACTION_FIELDS,
            Some(predicate),
            Vec::new(),
            1,
        )
        .await
    {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    // Deleting a reaction that is not there is success, as it was before: the
    // caller's intent is "this reaction is gone".
    let Some((record_id, _)) = rows.into_iter().next() else {
        return HttpResponse::Ok().json(json!({ "status": "success" }));
    };
    let Ok(entity) = AppName::parse("reaction") else {
        return town_square_unavailable();
    };
    match api
        .write(
            &square,
            "reaction",
            format!("social:unreact:{}", Uuid::new_v4()),
            vec![AppMutationOperation::Delete { entity, record_id }],
        )
        .await
    {
        Ok(()) => HttpResponse::Ok().json(json!({ "status": "success" })),
        Err(response) => response,
    }
}

#[derive(Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
    pub members: Vec<String>,
}

pub async fn create_group(
    req: HttpRequest,
    api: web::Data<SocialApi>,
    body: web::Json<CreateGroupRequest>,
) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return HttpResponse::BadRequest().json(json!({
            "error": "invalid_group_name",
            "message": "Group name must contain 1–120 characters."
        }));
    }
    let mut members = body.members.clone();
    if !members.iter().any(|member| member == OPERATOR_MEMBER_ID) {
        members.push(OPERATOR_MEMBER_ID.to_string());
    }
    members.sort();
    members.dedup();

    // The store used to enforce these on the write; it no longer sees the
    // write, so they live here. Without them a one-member "group" is a 201, and
    // a membership row can point at a member that does not exist.
    if members.len() > MAX_SOCIAL_GROUP_MEMBERS {
        return HttpResponse::BadRequest().json(json!({
            "error": "invalid_group",
            "message": format!(
                "A group cannot contain more than {MAX_SOCIAL_GROUP_MEMBERS} members."
            )
        }));
    }
    if members.len() < 3 {
        return HttpResponse::BadRequest().json(json!({
            "error": "invalid_group",
            "message": "A group requires three or more members."
        }));
    }
    let roster = match api
        .read(
            &square,
            "member",
            MEMBER_FIELDS,
            None,
            Vec::new(),
            ROSTER_BUDGET,
        )
        .await
    {
        Ok(rows) => rows.iter().map(member_from).collect::<Vec<_>>(),
        Err(response) => return response,
    };
    let all_active = members.iter().all(|candidate| {
        roster.iter().any(|member| {
            &member.member_id == candidate && (member.kind == "operator" || !member.opted_out)
        })
    });
    if !all_active {
        return HttpResponse::BadRequest().json(json!({
            "error": "invalid_group",
            "message": "A group can contain only active enrolled members."
        }));
    }

    let group_id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let mut operations = Vec::with_capacity(1 + members.len());
    let Some(group_op) = create_op(
        "group",
        "group_row",
        json!({
            "group_id": group_id,
            "name": name,
            "created_by": OPERATOR_MEMBER_ID,
            "created_at": created_at,
        }),
    ) else {
        return town_square_unavailable();
    };
    operations.push(group_op);
    for (index, member_id) in members.iter().enumerate() {
        let Some(op) = create_op(
            "group_membership",
            &format!("membership_{index}"),
            json!({
                "membership_id": format!("{group_id}:{member_id}"),
                "group_id": group_id,
                "member_id": member_id,
            }),
        ) else {
            return town_square_unavailable();
        };
        operations.push(op);
    }
    match api
        .write(
            &square,
            "group",
            format!("social:group:{group_id}"),
            operations,
        )
        .await
    {
        Ok(()) => HttpResponse::Created().json(json!({
            "status": "success",
            "group_id": group_id
        })),
        Err(response) => response,
    }
}

// ---------------------------------------------------------------------------
// Policy
//
// `GET /social/health` retired with the worker, but `/social/policy` also
// reported worker state and that half of its response still has to mean
// something. It now comes from the behavior scheduler: the scope's own pause
// control, and the `ambient_turn` head. The field names and shape are the
// retired engine's, because the UI reads them.
// ---------------------------------------------------------------------------

const POLICY_FIELDS: &[&str] = &[
    "policy_id",
    "autonomy_state",
    "cooldown_seconds",
    "max_post_chars",
    "max_autonomous_replies",
    "updated_at",
];

/// What the retired engine called the worker's posture, sourced from the
/// behavior that replaced it.
struct AmbientPosture {
    /// The behavior exists for this installation: the square is configured to
    /// have autonomous turns at all.
    configured: bool,
    /// The posture could not be read, as distinct from being read and found
    /// unconfigured. `configured` is false either way — the conservative end —
    /// but only one of the two is an operator's decision, and telling them to
    /// edit a config in the other case sends them after the wrong thing.
    ///
    /// Carried beside `state()` rather than inside it: those four values are
    /// the retired engine's closed vocabulary and adding a fifth would change
    /// what every existing consumer of `scope_policy.state` receives. This
    /// travels as its own `autonomous_scope_unknown` field instead.
    unknown: bool,
    /// A worker attempt is driving the scheduler. Read from the scheduler's own
    /// liveness bit, which the supervisor marks per attempt.
    enabled: bool,
    paused: bool,
    /// The `ambient_turn` head's own last error and accepted-run count, which
    /// are what `worker_global.last_error` and `posts_published` reported.
    last_error: Option<String>,
    accepted: u64,
}

impl AmbientPosture {
    /// The retired engine's precedence, which is NOT the obvious one: a scope
    /// whose worker is stopped reported `disabled` even when it was also
    /// paused. Reordering these silently changes what the UI shows.
    fn state(&self) -> &'static str {
        if !self.configured {
            "not_configured"
        } else if !self.enabled {
            "disabled"
        } else if self.paused {
            "paused"
        } else {
            "enabled"
        }
    }

    /// `chatter_ready` folded in `!paused`; `scope_policy.enabled` did not.
    fn ready(&self) -> bool {
        self.enabled && !self.paused
    }

    /// `worker_global.state`, which has one value `scope_policy.state` never
    /// had. The page renders a banner on `degraded` and shows `last_error`
    /// beside it; without this the banner is dead code and a behavior stuck in
    /// failure reports itself `enabled` with an error nothing displays.
    fn worker_state(&self) -> &'static str {
        if self.configured && self.last_error.is_some() {
            "degraded"
        } else {
            self.state()
        }
    }
}

async fn ambient_posture(api: &SocialApi, square: &TownSquare) -> AmbientPosture {
    let now = Utc::now();
    // Cloned out of the lock before the await: holding a std lock across an
    // await point is a deadlock waiting for a slow read.
    let scheduler = api
        .behaviors
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let Some(scheduler) = scheduler else {
        return AmbientPosture {
            configured: false,
            unknown: false,
            enabled: false,
            paused: false,
            last_error: None,
            accepted: 0,
        };
    };
    // Paged, not sampled. One page is ordered by `updated_at DESC` across the
    // whole scope, so in a workspace with more recently-touched behavior heads
    // than the page holds, `ambient_turn` falls off it and the square reports
    // itself unconfigured while it is running.
    let mut configured = false;
    let mut last_error = None;
    let mut accepted = 0u64;
    // Declared without a seed: both are scope-global, assigned from the first
    // page, and every path that reads them runs after that assignment.
    let mut paused;
    let mut running;
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let snapshot = match scheduler
            .health(&square.authenticated, 64, cursor.clone(), now)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                // A health read that fails must not read as "healthy", so the
                // posture stays conservative. But it must not read as a
                // *decision* either: this branch used to discard the error and
                // log nothing, so a registry read that lost its admission race
                // surfaced in the UI as "autonomous social activity is not
                // configured for this workspace — enable it in
                // magician-config.yaml". That sends an operator to edit a config
                // that was already correct. The reason now travels with the
                // posture and is logged.
                warn!(%error, "Town Square ambient posture unknown: behavior health read failed");
                return AmbientPosture {
                    configured: false,
                    unknown: true,
                    last_error: Some(format!("behavior health read failed: {error}")),
                    enabled: false,
                    paused: false,
                    accepted: 0,
                };
            },
        };
        paused = snapshot.scope_policy.paused;
        running = snapshot.worker_running;
        if let Some(item) = snapshot.items.iter().find(|item| {
            item.installation_id == square.installation_id
                && item.behavior_id.as_str() == AMBIENT_BEHAVIOR_ID
        }) {
            configured = true;
            last_error = item.last_error.clone();
            accepted = item
                .recurring
                .as_ref()
                .and_then(|state| state.published_records.get("post"))
                .copied()
                .unwrap_or(0);
            break;
        }
        pages += 1;
        match snapshot.next_cursor {
            Some(next) if pages < AMBIENT_HEALTH_MAX_PAGES => cursor = Some(next),
            _ => break,
        }
    }
    AmbientPosture {
        configured,
        // The posture was read successfully; `configured` here is an answer,
        // not an absence of one.
        unknown: false,
        // Deliberately does NOT fold in `!paused`: the retired
        // `scope_policy.enabled` was `configured && worker.enabled`, and a
        // paused scope reported `{enabled: true, paused: true}`.
        enabled: configured && running,
        paused,
        last_error,
        accepted,
    }
}

/// The policy singleton's row, if one has been written.
async fn read_policy(
    api: &SocialApi,
    square: &TownSquare,
) -> Result<Option<(AppRecordId, Map<String, Value>)>, HttpResponse> {
    let Some(predicate) = equals("policy_id", json!("singleton")) else {
        return Err(town_square_unavailable());
    };
    Ok(api
        .read_addressable(
            square,
            "policy",
            POLICY_FIELDS,
            Some(predicate),
            Vec::new(),
            1,
        )
        .await?
        .into_iter()
        .next())
}

async fn operator_policy_response(
    api: &SocialApi,
    square: &TownSquare,
    autonomous_enabled: bool,
    updated_at: Option<String>,
) -> HttpResponse {
    let posture = ambient_posture(api, square).await;
    HttpResponse::Ok().json(json!({
        "autonomous_enabled": autonomous_enabled,
        "updated_at": updated_at,
        "chatter_ready": posture.ready() && autonomous_enabled,
        "scope_policy": {
            "enabled": posture.enabled,
            "paused": posture.paused,
            "state": posture.state()
        }
    }))
}

/// `GET /social/health`.
///
/// This route was slated to retire with the worker it reported on, and doing
/// that broke the page: `town-square/+page.svelte` fetches it inside a
/// `Promise.all` alongside `/square`, `/members` and `/groups`, so a 404 here
/// discards three successful responses and disables the autonomy toggle. It is
/// also the ONLY place the UI reads the operator policy from — it never calls
/// `GET /social/policy`. "The wire does not move" and "health retires" could
/// not both be true; the wire wins.
///
/// The shape is the retired one. What changed is where the numbers come from:
/// `worker_global` is the `ambient_turn` behavior head rather than a worker.
pub async fn get_health(req: HttpRequest, api: web::Data<SocialApi>) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let posture = ambient_posture(&api, &square).await;
    // A failing health read answers in the retired engine's UNAVAILABLE shape,
    // not the generic read failure. This route is the page's liveness probe and
    // the only place it reads the operator policy, so the fields it degrades to
    // are load-bearing: `worker_global` keeps the banner alive and
    // `autonomous_scope_enabled` keeps the toggle's copy honest.
    let unavailable = |posture: &AmbientPosture| {
        HttpResponse::ServiceUnavailable().json(json!({
            "status": "unavailable",
            "autonomous_scope_enabled": posture.configured,
            // False when the posture was read; true when it could not be
            // read at all. Both leave `autonomous_scope_enabled` false, but
            // only one of them is something an operator chose.
            "autonomous_scope_unknown": posture.unknown,
            "worker_global": {
                "enabled": posture.enabled,
                "paused": posture.paused,
                "state": posture.worker_state(),
                "last_error": posture.last_error.clone(),
                "posts_published": posture.accepted
            },
            "error": "social_store_unavailable"
        }))
    };
    let Ok(row) = read_policy(&api, &square).await else {
        return unavailable(&posture);
    };
    let (autonomous_enabled, updated_at) = match &row {
        Some((_, fields)) => (
            text(fields, "autonomy_state") == "on",
            opt_text(fields, "updated_at"),
        ),
        None => (false, None),
    };
    let Ok(member_rows) = api
        .read(
            &square,
            "member",
            &["member_id"],
            None,
            Vec::new(),
            ROSTER_BUDGET,
        )
        .await
    else {
        return unavailable(&posture);
    };
    let member_count = member_rows.len();
    HttpResponse::Ok().json(json!({
        "status": "ok",
        "scope": {
            "principal": square.authenticated.scope().principal.as_str(),
            "workspace": square.authenticated.scope().workspace.as_str(),
        },
        "autonomous_scope_enabled": posture.configured,
            // False when the posture was read; true when it could not be
            // read at all. Both leave `autonomous_scope_enabled` false, but
            // only one of them is something an operator chose.
            "autonomous_scope_unknown": posture.unknown,
        "scope_policy": {
            "state": posture.state(),
            "enabled": posture.enabled,
            "paused": posture.paused
        },
        "operator_policy": {
            "autonomous_enabled": autonomous_enabled,
            "updated_at": updated_at,
            "chatter_ready": posture.ready() && autonomous_enabled
        },
        "worker_global": {
            "enabled": posture.enabled,
            "paused": posture.paused,
            "state": posture.worker_state(),
            "last_error": posture.last_error.clone(),
            "posts_published": posture.accepted
        },
        // `database_bytes` had no meaning once the corpus left SQLite. Reported
        // as zero rather than dropped, because the field is on the wire.
        "store": { "members": member_count, "database_bytes": 0 },
        "budget_posture": "fail_closed"
    }))
}

pub async fn get_operator_policy(req: HttpRequest, api: web::Data<SocialApi>) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let row = match read_policy(&api, &square).await {
        Ok(row) => row,
        Err(response) => return response,
    };
    // No policy row is a real state, not an error: an operator who has never
    // saved one gets autonomy off, which is what the retired engine reported.
    let (autonomous_enabled, updated_at) = match &row {
        Some((_, fields)) => (
            text(fields, "autonomy_state") == "on",
            opt_text(fields, "updated_at"),
        ),
        None => (false, None),
    };
    operator_policy_response(&api, &square, autonomous_enabled, updated_at).await
}

#[derive(Deserialize)]
pub struct PutOperatorPolicyRequest {
    autonomous_enabled: bool,
}

pub async fn put_operator_policy(
    req: HttpRequest,
    api: web::Data<SocialApi>,
    body: web::Json<PutOperatorPolicyRequest>,
) -> HttpResponse {
    let square = match api.town_square(&req).await {
        Ok(square) => square,
        Err(response) => return response,
    };
    let existing = match read_policy(&api, &square).await {
        Ok(row) => row,
        Err(response) => return response,
    };
    let enabled = body.autonomous_enabled;
    let autonomy_state = if enabled { "on" } else { "off" };
    let updated_at = Utc::now().to_rfc3339();
    let Ok(entity) = AppName::parse("policy") else {
        return town_square_unavailable();
    };

    let operation = match &existing {
        // Patch only the two fields this route owns. The other three are the
        // operator's own bounds and are not this endpoint's to reset.
        Some((record_id, _)) => AppMutationOperation::Update {
            entity,
            record_id: record_id.clone(),
            patch: json!({
                "autonomy_state": autonomy_state,
                "updated_at": updated_at,
            }),
        },
        // First write creates the singleton complete, under the name the
        // package addresses it by.
        None => {
            let Ok(record_id) = AppRecordId::parse("singleton") else {
                return town_square_unavailable();
            };
            let Ok(temporary_id) = AppName::parse("policy_row") else {
                return town_square_unavailable();
            };
            AppMutationOperation::Create {
                entity,
                temporary_id,
                record_id: Some(record_id),
                payload: json!({
                    "policy_id": "singleton",
                    "autonomy_state": autonomy_state,
                    "cooldown_seconds": DEFAULT_COOLDOWN_SECONDS,
                    "max_post_chars": DEFAULT_MAX_POST_CHARS,
                    "max_autonomous_replies": DEFAULT_MAX_AUTONOMOUS_REPLIES,
                    "updated_at": updated_at,
                }),
            }
        },
    };

    // A fresh key per submission: a policy change must apply, not replay.
    match api
        .write(
            &square,
            "operator policy",
            format!("social:policy:{}", Uuid::new_v4()),
            vec![operation],
        )
        .await
    {
        Ok(()) => operator_policy_response(&api, &square, enabled, Some(updated_at)).await,
        Err(response) => response,
    }
}

// ---------------------------------------------------------------------------
// Routes
//
// All thirteen, unchanged in path and method. `GET /social/health` survives
// because the UI reads it inside a `Promise.all` and it is the only place the
// page gets the operator policy from -- see the note on `get_health`.
// ---------------------------------------------------------------------------

pub fn configure_social_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/social")
            .route("/feed", web::get().to(get_feed))
            .route("/posts", web::post().to(create_post))
            .route("/posts/{post_id}/reactions", web::post().to(add_reaction))
            .route(
                "/posts/{post_id}/reactions/{emoji}",
                web::delete().to(delete_reaction),
            )
            .route("/groups", web::get().to(get_groups))
            .route("/groups", web::post().to(create_group))
            .route("/groups/{group_id}/posts", web::get().to(get_group_posts))
            .route("/members", web::get().to(get_members))
            .route("/members/{member_id}/self", web::get().to(get_member_self))
            .route("/health", web::get().to(get_health))
            .route("/policy", web::get().to(get_operator_policy))
            .route("/policy", web::put().to(put_operator_policy))
            .route("/square", web::get().to(get_square_ambient)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::magician_v2::apps::models::ValidateAppContract;

    /// An idempotency key must be a legal `AppReference` whatever the caller
    /// reacted with.
    ///
    /// An earlier cut interpolated the emoji straight into the key.
    /// `AppReference` admits alphanumerics and `_-.:/@#` only, so every one of
    /// the page's three emoji buttons answered 503 and it looked like a store
    /// outage rather than a bug. A later cut hashed the emoji into a STABLE
    /// key, which fixed the charset and introduced a worse bug: the payload
    /// carries a fresh `created_at`, so re-reacting after un-reacting replayed
    /// one key with different bytes and conflicted forever. Keys are per-write
    /// now, and repeats are made idempotent by an existence probe instead.
    #[test]
    fn every_generated_idempotency_key_is_a_legal_reference() {
        let uuid = Uuid::new_v4();
        for key in [
            format!("social:reaction:{uuid}"),
            format!("social:unreact:{uuid}"),
            format!("social:policy:{uuid}"),
            format!("social:post:{uuid}"),
            format!("social:group:{uuid}"),
        ] {
            assert!(
                AppReference::parse(key.clone()).is_ok(),
                "`{key}` must be a legal idempotency key"
            );
        }
    }

    /// The feed predicate has to be admissible, and the reason it once was not
    /// is subtle enough to be worth a test rather than a comment.
    ///
    /// `validate_typed_query` walks the node arena FLAT, so an inadmissible
    /// comparison is refused even on a branch that would never be taken. An
    /// earlier cut spelled the `(created_at, post_id)` keyset out as a
    /// predicate; `post_id` is text, ordering comparisons are legal only on
    /// integer, decimal and timestamp, and every `?before=` request became a
    /// 503.
    #[test]
    fn the_feed_predicate_never_orders_on_a_text_field() {
        let cursor = (
            "2026-09-03T10:00:00+00:00".to_owned(),
            "post-3f1a".to_owned(),
        );
        for predicate in [feed_predicate(None), feed_predicate(Some(&cursor))] {
            let predicate = predicate.expect("the feed predicate must build");
            for node in &predicate.nodes {
                if let AppPredicateNode::Compare {
                    field, operator, ..
                } = node
                {
                    let ordering = matches!(
                        operator,
                        AppComparisonOperator::LessThan
                            | AppComparisonOperator::LessThanOrEqual
                            | AppComparisonOperator::GreaterThan
                            | AppComparisonOperator::GreaterThanOrEqual
                    );
                    assert!(
                        !ordering || field.as_str() == "created_at",
                        "`{}` is not a timestamp; ordering comparisons on it are refused",
                        field.as_str()
                    );
                }
            }
        }
    }

    #[test]
    fn the_feed_page_size_is_admissible_as_a_query() {
        // A page above `max_page_rows` is refused at the boundary before it
        // reaches the store, so the read helper must page under it.
        let request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("post").unwrap(),
            select: vec![AppFieldPath::parse("post_id").unwrap()],
            predicate: feed_predicate(None),
            order: descending("created_at"),
            cursor: None,
            limit: ENTITY_PAGE_ROWS,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("social_http").unwrap(),
        };
        assert!(request
            .validate_app_contract(
                &magician::magician_v2::apps::models::AppContractLimits::default()
            )
            .is_ok());
    }

    #[test]
    fn the_feed_orders_newest_first_and_breaks_ties_on_post_id_descending() {
        let post = |created_at: &str, post_id: &str| Post {
            post_id: post_id.to_owned(),
            author_id: "operator".to_owned(),
            surface: "feed".to_owned(),
            group_id: None,
            post_type: "thought".to_owned(),
            body: String::new(),
            parent_id: None,
            created_at: created_at.to_owned(),
        };
        let mut posts = vec![
            post("2026-09-03T10:00:00+00:00", "b"),
            post("2026-09-03T11:00:00+00:00", "a"),
            post("2026-09-03T10:00:00+00:00", "c"),
        ];
        posts.sort_by(feed_order);
        assert_eq!(
            posts.iter().map(|p| p.post_id.as_str()).collect::<Vec<_>>(),
            vec!["a", "c", "b"],
            "newest first, then post_id descending -- the retired ORDER BY"
        );
    }

    /// The retired engine's precedence, which is not the obvious one.
    #[test]
    fn the_policy_state_precedence_matches_the_retired_engine() {
        let posture = |configured, enabled, paused| AmbientPosture {
            configured,
            // `state()` is deliberately blind to this: its four values are the
            // retired engine's closed vocabulary, so an unknown posture still
            // reports `not_configured` on that wire and the UI reads the
            // separate `autonomous_scope_unknown` flag, which it checks first.
            unknown: false,
            enabled,
            paused,
            last_error: None,
            accepted: 0,
        };
        assert_eq!(posture(false, false, false).state(), "not_configured");
        // Disabled beats paused: a stopped scheduler reported `disabled` even
        // when the scope was also paused.
        assert_eq!(posture(true, false, true).state(), "disabled");
        assert_eq!(posture(true, true, true).state(), "paused");
        assert_eq!(posture(true, true, false).state(), "enabled");

        // `scope_policy.enabled` did NOT fold in `!paused`; `chatter_ready` did.
        let paused = posture(true, true, true);
        assert!(
            paused.enabled,
            "a paused scope still reported enabled: true"
        );
        assert!(!paused.ready(), "but it was never chatter_ready");
    }

    #[test]
    fn a_failed_read_and_a_failed_write_keep_their_distinct_codes() {
        // Collapsing both into 503 would be a wire change on every failure
        // path, and the page prints `message` verbatim.
        let read = read_failed("feed");
        assert_eq!(
            read.status(),
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let write = write_failed("post");
        assert_eq!(
            write.status(),
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            town_square_unavailable().status(),
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn the_feed_cursor_round_trips_exactly_as_it_did() {
        let post = Post {
            post_id: "post-3f1a".to_owned(),
            author_id: "operator".to_owned(),
            surface: "feed".to_owned(),
            group_id: None,
            post_type: "thought".to_owned(),
            body: String::new(),
            parent_id: None,
            created_at: "2026-09-03T10:00:00+00:00".to_owned(),
        };
        let cursor = encode_feed_cursor(&post);
        assert_eq!(cursor, "2026-09-03T10:00:00+00:00|post-3f1a");
        let (created_at, post_id) = parse_feed_cursor(&cursor).expect("round trip");
        assert_eq!(created_at, post.created_at);
        assert_eq!(post_id, post.post_id);
        assert!(parse_feed_cursor("no-separator").is_err());
        assert!(parse_feed_cursor("not-a-timestamp|post-1").is_err());
    }
}
