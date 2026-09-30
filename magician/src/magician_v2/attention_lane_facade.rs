//! Internal lane-query facade for attention surfaces.
//!
//! This gives callers one small abstraction for "a stable cursor page from an
//! attention lane". Backing stores can still differ: channel annotations,
//! resurfacing rows, Today projections, and future adapters can all implement
//! [`AttentionLaneRecord`].

use crate::magician_v2::resurfacing_seam::Candidate;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::magician_v2::{
    attention_funnel::{AttentionLane, AttentionSourceFamily, AttentionSourceKind},
    feed::{FeedAttentionLane, FeedAttentionPageQuery, FeedItem, FeedPageCursor, FeedStore},
    storage::{row_follows_cursor, ListCursor},
};
const MAX_ATTENTION_LANE_LIMIT: usize = 200;
#[derive(Debug, Clone, Copy)]
pub struct AttentionLaneQuery<'a> {
    pub principal: &'a str,
    pub workspace: &'a str,
    pub lane: AttentionLane,
    pub cursor: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

impl<'a> AttentionLaneQuery<'a> {
    pub fn new(principal: &'a str, workspace: &'a str, lane: AttentionLane, limit: usize) -> Self {
        Self {
            principal,
            workspace,
            lane,
            cursor: None,
            offset: 0,
            limit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttentionLaneItem {
    pub id: String,
    pub lane: AttentionLane,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub source_kind: AttentionSourceKind,
    pub source_family: AttentionSourceFamily,
    pub source_ref: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionLaneCursor {
    pub updated_at: i64,
    pub item_id: String,
}

/// A cursor for a lane whose order leads with a priority band.
///
/// `priority` is `None` for a cursor minted before the band was part of the
/// key. Such a cursor can only be in flight across one page turn, and is
/// resolved exactly as it was then — see [`priority_cursor_offset`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorityAttentionLaneCursor {
    pub priority: Option<i64>,
    pub updated_at: i64,
    pub item_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionLanePage<T> {
    pub lane: AttentionLane,
    pub items: Vec<T>,
    pub total: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_hitl_total: Option<usize>,
    pub limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

pub trait AttentionLaneRecord {
    fn attention_lane_record_id(&self) -> &str;
    fn attention_lane_record_updated_at(&self) -> i64;
}

/// A record in a lane ordered by a priority band before anything else.
///
/// Follow-ups is such a lane: nine deliberate bands, descending, and within a
/// band the oldest first. A cursor over it has to carry the band, because
/// "after the cursor" is not a question `updated_at` alone can answer there.
pub trait AttentionLanePriorityRecord: AttentionLaneRecord {
    fn attention_lane_record_priority(&self) -> i64;
}

impl AttentionLaneRecord for AttentionLaneItem {
    fn attention_lane_record_id(&self) -> &str {
        &self.id
    }

    fn attention_lane_record_updated_at(&self) -> i64 {
        self.updated_at
    }
}
impl AttentionLaneRecord for Candidate {
    fn attention_lane_record_id(&self) -> &str {
        &self.candidate_id
    }

    fn attention_lane_record_updated_at(&self) -> i64 {
        self.last_surfaced_at.unwrap_or(self.last_scored_at)
    }
}

pub fn normalize_attention_lane_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_ATTENTION_LANE_LIMIT)
}

pub fn encode_attention_lane_cursor(updated_at: i64, item_id: &str) -> String {
    format!("{updated_at}:{}", urlencoding::encode(item_id))
}

pub fn decode_attention_lane_cursor(cursor: &str) -> Result<AttentionLaneCursor> {
    let (updated_at, encoded_id) = cursor
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid attention lane cursor"))?;
    let updated_at = updated_at
        .parse::<i64>()
        .context("invalid attention lane cursor timestamp")?;
    let item_id = urlencoding::decode(encoded_id)
        .context("invalid attention lane cursor item id")?
        .into_owned();
    if item_id.trim().is_empty() {
        return Err(anyhow!("invalid attention lane cursor item id"));
    }
    Ok(AttentionLaneCursor {
        updated_at,
        item_id,
    })
}

pub fn encode_priority_attention_lane_cursor(
    priority: i64,
    updated_at: i64,
    item_id: &str,
) -> String {
    format!("{priority}:{updated_at}:{}", urlencoding::encode(item_id))
}

/// Decode a priority lane cursor, accepting the two-part form that preceded it.
///
/// The item id is percent-encoded, so a `:` inside it cannot be mistaken for a
/// separator and the number of parts alone tells the two forms apart.
pub fn decode_priority_attention_lane_cursor(cursor: &str) -> Result<PriorityAttentionLaneCursor> {
    let parts = cursor.split(':').collect::<Vec<_>>();
    let (priority, updated_at, encoded_id) = match parts.as_slice() {
        [priority, updated_at, item_id] => (
            Some(
                priority
                    .parse::<i64>()
                    .context("invalid attention lane cursor priority")?,
            ),
            *updated_at,
            *item_id,
        ),
        [updated_at, item_id] => (None, *updated_at, *item_id),
        _ => return Err(anyhow!("invalid attention lane cursor")),
    };
    let updated_at = updated_at
        .parse::<i64>()
        .context("invalid attention lane cursor timestamp")?;
    let item_id = urlencoding::decode(encoded_id)
        .context("invalid attention lane cursor item id")?
        .into_owned();
    if item_id.trim().is_empty() {
        return Err(anyhow!("invalid attention lane cursor item id"));
    }
    Ok(PriorityAttentionLaneCursor {
        priority,
        updated_at,
        item_id,
    })
}
pub async fn list_feed_attention_lane(
    store: &FeedStore,
    query: AttentionLaneQuery<'_>,
    feed_lane: FeedAttentionLane,
    ui_thread_id: Option<&str>,
    exclude_task_ids: &[String],
) -> Result<AttentionLanePage<FeedItem>> {
    let cursor = query
        .cursor
        .map(decode_attention_lane_cursor)
        .transpose()?
        .map(|cursor| FeedPageCursor {
            updated_at: cursor.updated_at,
            id: cursor.item_id,
        });
    let page = store
        .list_attention_lane_page(FeedAttentionPageQuery {
            principal: query.principal.to_string(),
            workspace: query.workspace.to_string(),
            lane: feed_lane,
            ui_thread_id: ui_thread_id.map(str::to_string),
            cursor,
            offset: query.offset,
            limit: normalize_attention_lane_limit(query.limit),
            exclude_task_ids: exclude_task_ids.to_vec(),
        })
        .await?;
    Ok(AttentionLanePage {
        lane: query.lane,
        items: page.items,
        total: total_to_usize(page.total),
        request_hitl_total: page.request_hitl_total.map(total_to_usize),
        limit: normalize_attention_lane_limit(query.limit),
        cursor: query.cursor.map(ToOwned::to_owned),
        next_cursor: page
            .next_cursor
            .map(|cursor| encode_attention_lane_cursor(cursor.updated_at, &cursor.id)),
        has_more: page.has_more,
    })
}

/// Return a cursor page from a caller-supplied stable lane order.
///
/// The facade deliberately preserves the caller's ordering. For current lanes
/// that is the existing product order; future store-backed implementations can
/// pass rows already ordered by their canonical cursor index.
pub fn list_attention_lane<T>(
    lane: AttentionLane,
    ordered_items: &[T],
    cursor: Option<&str>,
    limit: usize,
) -> Result<AttentionLanePage<T>>
where
    T: AttentionLaneRecord + Clone,
{
    let decoded_cursor = cursor.map(decode_attention_lane_cursor).transpose()?;
    let offset = decoded_cursor
        .as_ref()
        .map(|cursor| cursor_offset(ordered_items, cursor))
        .unwrap_or(0);
    Ok(page_from_offset(
        lane,
        ordered_items,
        offset,
        limit,
        cursor.map(ToOwned::to_owned),
    ))
}

/// Return a cursor page from a lane ordered by a priority band first.
///
/// Identical to [`list_attention_lane`] except in the one thing that decides a
/// resume: the cursor carries `{priority}:{updated_at}:{item_id}`, the key the
/// lane is actually sorted on. Encoding only `{updated_at}:{item_id}` for a
/// lane ordered `priority DESC, updated_at ASC` describes an order the lane is
/// not in, so a cursor whose row vanished resumed by the wrong key and served
/// rows the reader had already been given.
pub fn list_priority_attention_lane<T>(
    lane: AttentionLane,
    ordered_items: &[T],
    cursor: Option<&str>,
    limit: usize,
) -> Result<AttentionLanePage<T>>
where
    T: AttentionLanePriorityRecord + Clone,
{
    let decoded_cursor = cursor
        .map(decode_priority_attention_lane_cursor)
        .transpose()?;
    let offset = decoded_cursor
        .as_ref()
        .map(|cursor| priority_cursor_offset(ordered_items, cursor))
        .unwrap_or(0);
    let limit = normalize_attention_lane_limit(limit);
    let total = ordered_items.len();
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    let items = ordered_items[start..end].to_vec();
    let has_more = end < total;
    let next_cursor = if has_more {
        items.last().map(|item| {
            encode_priority_attention_lane_cursor(
                item.attention_lane_record_priority(),
                item.attention_lane_record_updated_at(),
                item.attention_lane_record_id(),
            )
        })
    } else {
        None
    };
    Ok(AttentionLanePage {
        lane,
        items,
        total,
        request_hitl_total: None,
        limit,
        cursor: cursor.map(ToOwned::to_owned),
        next_cursor,
        has_more,
    })
}

/// Where the next page of a priority-ordered lane starts.
///
/// "Sorts after the cursor" here is the lane's own order — a lower band, or the
/// same band later in time. The same three-step resolution as
/// [`cursor_offset`]: seek, confirm the row it landed just after IS the
/// cursor's, and otherwise fall forward to the first row that sorts after the
/// position the cursor named. The band ties are broken by the producer's
/// insertion order rather than by a key, so the seek is checked here for the
/// same reason it is checked there.
///
/// A cursor minted before the band was in the key carries no band. It is
/// resolved exactly as it was before this change — by exact row match, then by
/// the old `updated_at`-descending keyset — so a page turn in flight across the
/// deploy behaves no worse than it did.
fn priority_cursor_offset<T>(ordered_items: &[T], cursor: &PriorityAttentionLaneCursor) -> usize
where
    T: AttentionLanePriorityRecord,
{
    let exact = |item: &T| {
        item.attention_lane_record_id() == cursor.item_id.as_str()
            && item.attention_lane_record_updated_at() == cursor.updated_at
            && cursor
                .priority
                .is_none_or(|priority| item.attention_lane_record_priority() == priority)
    };

    let Some(priority) = cursor.priority else {
        if let Some(index) = ordered_items.iter().position(exact) {
            return index.saturating_add(1);
        }
        let keyset = ListCursor::new(cursor.updated_at, cursor.item_id.clone());
        return ordered_items
            .iter()
            .position(|item| {
                row_follows_cursor(
                    &keyset,
                    item.attention_lane_record_updated_at(),
                    item.attention_lane_record_id(),
                )
            })
            .unwrap_or(ordered_items.len());
    };

    let follows = |item: &T| {
        let item_priority = item.attention_lane_record_priority();
        item_priority < priority
            || (item_priority == priority
                && item.attention_lane_record_updated_at() > cursor.updated_at)
    };

    let seek = ordered_items.partition_point(|item| !follows(item));
    if seek > 0 && exact(&ordered_items[seek - 1]) {
        return seek;
    }
    if let Some(index) = ordered_items.iter().position(exact) {
        return index.saturating_add(1);
    }
    ordered_items
        .iter()
        .position(follows)
        .unwrap_or(ordered_items.len())
}

fn page_from_offset<T>(
    lane: AttentionLane,
    ordered_items: &[T],
    offset: usize,
    limit: usize,
    cursor: Option<String>,
) -> AttentionLanePage<T>
where
    T: AttentionLaneRecord + Clone,
{
    let total = ordered_items.len();
    let limit = normalize_attention_lane_limit(limit);
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    let items = ordered_items[start..end].to_vec();
    let has_more = end < total;
    let next_cursor = if has_more {
        items.last().map(|item| {
            encode_attention_lane_cursor(
                item.attention_lane_record_updated_at(),
                item.attention_lane_record_id(),
            )
        })
    } else {
        None
    };

    AttentionLanePage {
        lane,
        items,
        total,
        request_hitl_total: None,
        limit,
        cursor,
        next_cursor,
        has_more,
    }
}

/// Where the next page starts: **a keyset seek**, not a scan of the lane.
///
/// A lane is ordered newest-first, and this facade's cursor has always
/// encoded that ordering's key — `updated_at`, then the item id — which is
/// byte-for-byte the storage index's keyset. Under that ordering "sorts
/// after the cursor" is monotone, so the rows the next page may contain are
/// a contiguous SUFFIX and its start is one `partition_point`: `log n`
/// comparisons instead of the `position(…)` that made the cost of page 50
/// the cost of page 1, exactly as the list handlers' `skip`/`take` did.
///
/// The predicate is `list_index::row_follows_cursor` — the same one the
/// index's SQL `WHERE` clause is. A second definition of "after" here would
/// make the same lane skip or repeat a row depending on which side computed
/// the page, and neither page would look wrong on its own.
///
/// **The seek is checked, not trusted.** `list_attention_lane` deliberately
/// preserves the CALLER's ordering, and not every producer breaks a
/// same-instant tie the way the keyset does (Today's sections currently
/// break theirs by ascending id). Where the two agree, the row just before
/// the seek IS the cursor's row and the answer is exact. Where they do not,
/// this falls back to the scan that reads the caller's own order — which is
/// the only thing that can be right when the order is not the keyset's.
/// Correctness never depends on the fast path.
///
/// **A cursor whose row disappeared between page requests still resolves**,
/// landing on the first surviving row that sorts after it rather than
/// erroring or replaying the first page. A cursor past the end of the lane
/// resolves to its length — an empty page, never an error.
fn cursor_offset<T>(ordered_items: &[T], cursor: &AttentionLaneCursor) -> usize
where
    T: AttentionLaneRecord,
{
    let keyset = ListCursor::new(cursor.updated_at, cursor.item_id.clone());
    let follows = |item: &T| {
        row_follows_cursor(
            &keyset,
            item.attention_lane_record_updated_at(),
            item.attention_lane_record_id(),
        )
    };

    let seek = ordered_items.partition_point(|item| !follows(item));
    if seek > 0 {
        let landed_on = &ordered_items[seek - 1];
        if landed_on.attention_lane_record_id() == cursor.item_id.as_str()
            && landed_on.attention_lane_record_updated_at() == cursor.updated_at
        {
            return seek;
        }
    }

    if let Some(index) = ordered_items.iter().position(|item| {
        item.attention_lane_record_id() == cursor.item_id.as_str()
            && item.attention_lane_record_updated_at() == cursor.updated_at
    }) {
        return index.saturating_add(1);
    }

    // The cursor's row is gone. Resume at the first row that sorts after the
    // position it named, so a mutable lane does not replay its whole first
    // page when a row is dismissed between requests.
    //
    // Scanned rather than reusing `seek`: a binary search is only meaningful
    // over an order the predicate actually partitions, and this branch is
    // reached precisely when it may not.
    ordered_items
        .iter()
        .position(follows)
        .unwrap_or(ordered_items.len())
}

pub fn total_to_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
