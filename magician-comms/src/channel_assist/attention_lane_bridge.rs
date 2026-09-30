//! Store bridges for the attention lane facade: resurfacing cursor codecs,
//! channel/resurfacing lane readers, row converters, and the NeedsApprovalRow
//! lane-record impl. Pure facade machinery stays lib-side.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;

use magician::magician_v2::attention_funnel::{
    AttentionLane, AttentionSourceFamily, AttentionSourceKind,
};
use magician::magician_v2::attention_lane_facade::{
    decode_attention_lane_cursor, encode_attention_lane_cursor, normalize_attention_lane_limit,
    total_to_usize, AttentionLaneItem, AttentionLanePage, AttentionLaneQuery, AttentionLaneRecord,
};
#[cfg(any(test, feature = "test-fixtures"))]
use magician::magician_v2::storage::{row_follows_cursor, ListCursor};
// Test-only, and re-added after `72fae8b75` dropped them: that commit cleaned
// unused imports against the PRODUCTION build only, so every symbol the tests
// below reach through `use super::*` disappeared and the lib-test target has not
// compiled since. Gated rather than restored to the top-level list so the
// production check stays quiet, which is what that commit was for.
#[cfg(any(test, feature = "test-fixtures"))]
use magician::magician_v2::attention_lane_facade::{
    decode_priority_attention_lane_cursor, encode_priority_attention_lane_cursor,
    list_attention_lane, list_priority_attention_lane, AttentionLaneCursor,
    AttentionLanePriorityRecord, PriorityAttentionLaneCursor,
};
use magician::magician_v2::resurfacing_seam::{Candidate, SourceKind};

use crate::channel_assist::channel::ChannelAssistStore;
use crate::channel_assist::store::{NeedsApprovalCursor, NeedsApprovalRow};
use magician::magician_v2::attention::resurfacing::store::{ResurfacingStore, SurfacedCursor};

const RESURFACING_CURSOR_PREFIX: &str = "resurfacing:";
pub fn encode_resurfacing_attention_cursor(cursor: &SurfacedCursor) -> String {
    format!(
        "{RESURFACING_CURSOR_PREFIX}{}:{}:{}",
        cursor.surfaced_at,
        cursor.salience_score,
        urlencoding::encode(&cursor.candidate_id)
    )
}

pub fn decode_resurfacing_attention_cursor(cursor: &str) -> Result<SurfacedCursor> {
    let raw = cursor
        .strip_prefix(RESURFACING_CURSOR_PREFIX)
        .ok_or_else(|| anyhow!("invalid resurfacing attention cursor"))?;
    let mut parts = raw.splitn(3, ':');
    let surfaced_at = parts
        .next()
        .ok_or_else(|| anyhow!("invalid resurfacing attention cursor"))?
        .parse::<i64>()
        .context("invalid resurfacing attention cursor timestamp")?;
    let salience_score = parts
        .next()
        .ok_or_else(|| anyhow!("invalid resurfacing attention cursor"))?
        .parse::<f64>()
        .context("invalid resurfacing attention cursor score")?;
    if !salience_score.is_finite() {
        return Err(anyhow!("invalid resurfacing attention cursor score"));
    }
    let candidate_id = parts
        .next()
        .ok_or_else(|| anyhow!("invalid resurfacing attention cursor"))?;
    let candidate_id = urlencoding::decode(candidate_id)
        .context("invalid resurfacing attention cursor candidate id")?
        .into_owned();
    if candidate_id.trim().is_empty() {
        return Err(anyhow!("invalid resurfacing attention cursor candidate id"));
    }
    Ok(SurfacedCursor {
        surfaced_at,
        salience_score,
        candidate_id,
    })
}

pub async fn list_channel_attention_lane(
    store: &ChannelAssistStore,
    query: AttentionLaneQuery<'_>,
) -> Result<AttentionLanePage<AttentionLaneItem>> {
    let lane = query.lane;
    let page = list_channel_attention_lane_rows(store, query).await?;
    let items = page
        .items
        .iter()
        .map(|row| channel_row_to_lane_item(lane, row))
        .collect();
    Ok(AttentionLanePage {
        lane: page.lane,
        items,
        total: page.total,
        request_hitl_total: None,
        limit: page.limit,
        cursor: page.cursor,
        next_cursor: page.next_cursor,
        has_more: page.has_more,
    })
}

pub async fn list_channel_attention_lane_rows(
    store: &ChannelAssistStore,
    query: AttentionLaneQuery<'_>,
) -> Result<AttentionLanePage<NeedsApprovalRow>> {
    let attention_lane = match query.lane {
        AttentionLane::FollowUp | AttentionLane::NeedsYou => query.lane.as_str(),
        _ => bail!("channel annotations do not back {} lane", query.lane),
    };
    let cursor = query
        .cursor
        .map(decode_attention_lane_cursor)
        .transpose()?
        .map(|cursor| NeedsApprovalCursor {
            created_at: cursor.updated_at,
            annotation_id: cursor.item_id,
        });
    let page = store
        .list_needs_approval_attention_lane_page(
            query.principal,
            query.workspace,
            attention_lane,
            normalize_attention_lane_limit(query.limit),
            query.offset,
            cursor,
        )
        .await?;
    Ok(AttentionLanePage {
        lane: query.lane,
        items: page.rows,
        total: total_to_usize(page.total),
        request_hitl_total: None,
        limit: page.limit,
        cursor: query.cursor.map(ToOwned::to_owned),
        next_cursor: page
            .next_cursor
            .map(|cursor| encode_attention_lane_cursor(cursor.created_at, &cursor.annotation_id)),
        has_more: page.has_more,
    })
}

pub async fn list_resurfacing_attention_lane(
    store: &ResurfacingStore,
    query: AttentionLaneQuery<'_>,
) -> Result<AttentionLanePage<AttentionLaneItem>> {
    let page = list_resurfacing_attention_lane_candidates(store, query).await?;
    let items = page
        .items
        .iter()
        .map(resurfacing_candidate_to_lane_item)
        .collect();
    Ok(AttentionLanePage {
        lane: page.lane,
        items,
        total: page.total,
        request_hitl_total: None,
        limit: page.limit,
        cursor: page.cursor,
        next_cursor: page.next_cursor,
        has_more: page.has_more,
    })
}

pub async fn list_resurfacing_attention_lane_candidates(
    store: &ResurfacingStore,
    query: AttentionLaneQuery<'_>,
) -> Result<AttentionLanePage<Candidate>> {
    if query.lane != AttentionLane::WorthALook {
        bail!("resurfacing store does not back {} lane", query.lane);
    }
    let cursor = query
        .cursor
        .map(decode_resurfacing_attention_cursor)
        .transpose()?;
    let page = store
        .list_surfaced_page(
            query.principal,
            query.workspace,
            normalize_attention_lane_limit(query.limit),
            query.offset,
            cursor,
        )
        .await?;
    Ok(AttentionLanePage {
        lane: query.lane,
        items: page.candidates,
        total: total_to_usize(page.total),
        request_hitl_total: None,
        limit: page.limit,
        cursor: query.cursor.map(ToOwned::to_owned),
        next_cursor: page
            .next_cursor
            .as_ref()
            .map(encode_resurfacing_attention_cursor),
        has_more: page.has_more,
    })
}
fn channel_row_to_lane_item(lane: AttentionLane, row: &NeedsApprovalRow) -> AttentionLaneItem {
    let source_family = AttentionSourceFamily::from_follow_up_route_metadata_or_signals(
        row.label.as_deref(),
        row.proposed_action.as_ref(),
    );
    let source_ref = format!(
        "{}:{}:{}",
        row.provider.as_str(),
        row.account_alias.as_str(),
        row.thread_id.as_str()
    );
    let sender = match (&row.from_name, &row.from_address) {
        (Some(name), Some(address)) => Some(format!("{name} <{address}>")),
        (Some(name), None) => Some(name.clone()),
        (None, Some(address)) => Some(address.clone()),
        (None, None) => None,
    };
    AttentionLaneItem {
        id: row.annotation_id.clone(),
        lane,
        title: non_empty_string(row.subject.as_deref())
            .or_else(|| sender.clone())
            .unwrap_or_else(|| "Follow-up".to_string()),
        summary: non_empty_string(row.latest_summary.as_deref())
            .or_else(|| non_empty_string(row.reason.as_deref())),
        source_kind: AttentionSourceKind::Comm,
        source_family,
        source_ref,
        created_at: row.created_at,
        updated_at: row.created_at,
        metadata: json!({
            "provider": row.provider.as_str(),
            "account_alias": row.account_alias.as_str(),
            "account_email": row.account_email.as_deref(),
            "thread_id": row.thread_id.as_str(),
            "lane": row.lane.as_str(),
            "label": row.label.as_deref(),
            "confidence": row.confidence,
            "reason": row.reason.as_deref(),
            "sender": sender,
            "received_at": row.last_message_at,
            "evidence_message_id": row.evidence_message_id.as_deref(),
            "evidence_message_at": row.evidence_message_at,
            "proposed_action": row.proposed_action.as_ref(),
        }),
    }
}

fn resurfacing_candidate_to_lane_item(candidate: &Candidate) -> AttentionLaneItem {
    AttentionLaneItem {
        id: candidate.candidate_id.clone(),
        lane: AttentionLane::WorthALook,
        title: candidate.title.clone(),
        summary: non_empty_string(Some(candidate.content_digest.as_str())),
        source_kind: resurfacing_source_kind(candidate.source_kind),
        source_family: AttentionSourceFamily::Resurfacing,
        source_ref: candidate.source_ref.clone(),
        created_at: candidate.first_seen_at,
        updated_at: candidate
            .last_surfaced_at
            .unwrap_or(candidate.last_scored_at),
        metadata: json!({
            "source_kind": candidate.source_kind.as_str(),
            "salience_score": candidate.salience_score,
            "signals": {
                "recency": candidate.signals.recency,
                "frequency": candidate.signals.frequency,
                "centrality": candidate.signals.centrality,
                "cooccurrence": candidate.signals.cooccurrence,
                "temporal_anchor": candidate.signals.temporal_anchor,
                "dormancy": candidate.signals.dormancy,
            },
            "temporal_anchor_at": candidate.temporal_anchor_at,
            "last_scored_at": candidate.last_scored_at,
            "last_surfaced_at": candidate.last_surfaced_at,
            "surface_count": candidate.surface_count,
            "dismiss_count": candidate.dismiss_count,
        }),
    }
}

fn resurfacing_source_kind(source_kind: SourceKind) -> AttentionSourceKind {
    match source_kind {
        SourceKind::Memory => AttentionSourceKind::Memory,
        SourceKind::Task => AttentionSourceKind::Task,
        SourceKind::Episode => AttentionSourceKind::Episode,
        SourceKind::Comm => AttentionSourceKind::Comm,
        SourceKind::Calendar => AttentionSourceKind::Calendar,
        SourceKind::Note => AttentionSourceKind::Note,
        SourceKind::Web => AttentionSourceKind::Web,
    }
}

impl AttentionLaneRecord for NeedsApprovalRow {
    fn attention_lane_record_id(&self) -> &str {
        &self.annotation_id
    }

    fn attention_lane_record_updated_at(&self) -> i64 {
        self.created_at
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Row {
        id: String,
        updated_at: i64,
    }

    impl Row {
        fn new(id: &str, updated_at: i64) -> Self {
            Self {
                id: id.to_string(),
                updated_at,
            }
        }
    }

    impl AttentionLaneRecord for Row {
        fn attention_lane_record_id(&self) -> &str {
            &self.id
        }

        fn attention_lane_record_updated_at(&self) -> i64 {
            self.updated_at
        }
    }

    #[test]
    fn cursor_pages_from_last_item_in_previous_page() {
        let rows = vec![Row::new("a", 30), Row::new("b", 20), Row::new("c", 10)];

        let first = list_attention_lane(AttentionLane::FollowUp, &rows, None, 2).unwrap();
        assert_eq!(
            first
                .items
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert!(first.has_more);

        let second = list_attention_lane(
            AttentionLane::FollowUp,
            &rows,
            first.next_cursor.as_deref(),
            2,
        )
        .unwrap();
        assert_eq!(
            second
                .items
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["c"]
        );
        assert!(!second.has_more);
    }

    #[test]
    fn disappeared_cursor_falls_forward_in_desc_id_order() {
        let rows = vec![Row::new("c", 20), Row::new("a", 20), Row::new("z", 10)];
        let cursor = encode_attention_lane_cursor(20, "b");

        let page = list_attention_lane(AttentionLane::FollowUp, &rows, Some(&cursor), 10).unwrap();

        assert_eq!(
            page.items
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "z"]
        );
    }

    /// The lane in the keyset order every cursor is defined against:
    /// `updated_at` descending, then id descending inside a tie. Three rows
    /// share one instant so the TIEBREAK, not the timestamp, decides most of
    /// what follows.
    fn keyset_lane() -> Vec<Row> {
        vec![
            Row::new("e", 30),
            Row::new("c", 20),
            Row::new("b", 20),
            Row::new("a", 20),
            Row::new("d", 10),
        ]
    }

    /// The definition `cursor_offset` used to carry, kept here as the thing
    /// the seek must agree with. Deleting the seek and keeping this would
    /// pass; the point is that the two must never diverge.
    fn linear_cursor_offset(rows: &[Row], cursor: &AttentionLaneCursor) -> usize {
        if let Some(index) = rows
            .iter()
            .position(|row| row.id == cursor.item_id && row.updated_at == cursor.updated_at)
        {
            return index + 1;
        }
        rows.iter()
            .position(|row| {
                row.updated_at < cursor.updated_at
                    || (row.updated_at == cursor.updated_at
                        && row.id.as_str() < cursor.item_id.as_str())
            })
            .unwrap_or(rows.len())
    }

    /// A lane whose same-instant rows are in ASCENDING id order — the shape
    /// Today's sections actually produce (`left.id.cmp(&right.id)`), and the
    /// reason the seek is checked instead of trusted.
    fn product_ordered_lane() -> Vec<Row> {
        vec![
            Row::new("e", 30),
            Row::new("a", 20),
            Row::new("b", 20),
            Row::new("c", 20),
            Row::new("d", 10),
        ]
    }

    /// Every position a cursor can name over one lane: each real row, ids
    /// that sort before / between / after them, and instants either side of
    /// the lane — the positions a cursor only reaches after a row was
    /// dismissed or a lane re-scored.
    fn cursor_probes() -> Vec<(i64, &'static str)> {
        vec![
            (30, "e"),
            (30, "f"),
            (30, "a"),
            (20, "c"),
            (20, "bb"),
            (20, "b"),
            (20, "a"),
            (20, "z"),
            (20, ""),
            (10, "d"),
            (10, "e"),
            (40, "anything"),
            (5, "anything"),
        ]
    }

    /// **This test is the point of the task.** The seek replaced a linear
    /// scan; it has to land in exactly the same place for every position a
    /// cursor can name — over the keyset order it is fast on, AND over the
    /// product order it must fall back for.
    #[test]
    fn the_seek_lands_where_the_scan_did_for_every_cursor_position() {
        for (label, rows) in [
            ("keyset order", keyset_lane()),
            ("product order", product_ordered_lane()),
        ] {
            let mut distinct = std::collections::BTreeSet::new();
            for (updated_at, item_id) in cursor_probes() {
                let cursor = AttentionLaneCursor {
                    updated_at,
                    item_id: item_id.to_string(),
                };
                let offset = cursor_offset(&rows, &cursor);
                assert_eq!(
                    offset,
                    linear_cursor_offset(&rows, &cursor),
                    "{label}: seek and scan disagree at {updated_at}:{item_id}"
                );
                assert!(offset <= rows.len(), "{label}");
                distinct.insert(offset);
            }
            // An implementation that answered 0 for everything, or `len` for
            // everything, would satisfy the equality above just as well.
            assert!(
                distinct.len() >= 4,
                "{label}: the probes must reach different positions: {distinct:?}"
            );
            assert!(distinct.contains(&0), "{label}");
            assert!(distinct.contains(&rows.len()), "{label}");
        }
    }

    #[test]
    fn a_cursor_walks_every_row_of_a_tie_group_exactly_once() {
        // Page size one, so every step is a fresh seek from the cursor the
        // previous page minted — the walk a client actually performs, and
        // the one that loops forever or skips a row if the resume position
        // is off by one inside a same-instant group.
        for (label, rows, expected) in [
            ("keyset order", keyset_lane(), vec!["e", "c", "b", "a", "d"]),
            (
                "product order",
                product_ordered_lane(),
                vec!["e", "a", "b", "c", "d"],
            ),
        ] {
            let mut seen: Vec<String> = Vec::new();
            let mut cursor: Option<String> = None;
            for _ in 0..rows.len() + 1 {
                let page =
                    list_attention_lane(AttentionLane::FollowUp, &rows, cursor.as_deref(), 1)
                        .unwrap();
                seen.extend(page.items.iter().map(|row| row.id.clone()));
                assert_eq!(page.total, rows.len(), "{label}: total is the lane");
                match page.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            assert_eq!(
                seen, expected,
                "{label}: the walk must visit every row once, in the CALLER's order"
            );
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct BandedRow {
        id: String,
        priority: i64,
        updated_at: i64,
    }

    impl BandedRow {
        fn new(id: &str, priority: i64, updated_at: i64) -> Self {
            Self {
                id: id.to_string(),
                priority,
                updated_at,
            }
        }
    }

    impl AttentionLaneRecord for BandedRow {
        fn attention_lane_record_id(&self) -> &str {
            &self.id
        }

        fn attention_lane_record_updated_at(&self) -> i64 {
            self.updated_at
        }
    }

    impl AttentionLanePriorityRecord for BandedRow {
        fn attention_lane_record_priority(&self) -> i64 {
            self.priority
        }
    }

    /// Follow-ups in the order it is ACTUALLY in: `priority DESC, updated_at
    /// ASC`, over the real bands (780 blocked task, 730 meeting action, 610
    /// stale routine). Note what this shape does that an `updated_at DESC`
    /// fixture cannot: `updated_at` runs 100, 300, 200, 400 down the lane, so
    /// nothing about the timestamp alone can say what follows what.
    fn follow_up_ordered_lane() -> Vec<BandedRow> {
        vec![
            BandedRow::new("a", 780, 100),
            BandedRow::new("b", 780, 300),
            BandedRow::new("c", 730, 200),
            BandedRow::new("d", 610, 400),
        ]
    }

    #[test]
    fn a_dismissed_cursor_row_resumes_without_replaying_the_page() {
        // The cursor named row `b`, which was dismissed before the next
        // request. Resuming must land on the row after where `b` was, not
        // back at the top of the lane and not on an error.
        let rows = vec![Row::new("e", 30), Row::new("c", 20), Row::new("d", 10)];
        let cursor = encode_attention_lane_cursor(20, "b");

        let page = list_attention_lane(AttentionLane::FollowUp, &rows, Some(&cursor), 10).unwrap();

        assert_eq!(
            page.items
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["d"],
            "`c` sorts before the dismissed `b` in the lane order and must not repeat"
        );
        assert_eq!(page.total, 3);

        // The same defect over the order Follow-ups is really in. This is the
        // half the old fixture could not test: `b` was served on the previous
        // page and then completed, and the reader that completed it is the one
        // asking for the next page.
        let lane = follow_up_ordered_lane();
        let served = list_priority_attention_lane(AttentionLane::FollowUp, &lane, None, 2).unwrap();
        assert_eq!(
            served
                .items
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        let cursor = served.next_cursor.expect("a next cursor");
        assert_eq!(
            cursor, "780:300:b",
            "the cursor must carry the band the lane is ordered by"
        );

        let after_dismissal: Vec<BandedRow> =
            lane.iter().filter(|row| row.id != "b").cloned().collect();
        let resumed = list_priority_attention_lane(
            AttentionLane::FollowUp,
            &after_dismissal,
            Some(&cursor),
            10,
        )
        .unwrap();

        assert_eq!(
            resumed
                .items
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "d"],
            "`a` was already served and sits ahead of the dismissed `b`; it must not repeat"
        );

        // Exactly what the old `{updated_at}:{item_id}` key answers for the
        // same lane and the same dismissal. It replays `a` — which is the bug,
        // stated rather than described, so this test cannot pass without the
        // band in the key.
        let legacy_cursor = encode_attention_lane_cursor(300, "b");
        let legacy_offset = cursor_offset(
            &after_dismissal
                .iter()
                .map(|row| Row::new(&row.id, row.updated_at))
                .collect::<Vec<_>>(),
            &decode_attention_lane_cursor(&legacy_cursor).unwrap(),
        );
        assert_eq!(
            legacy_offset, 0,
            "the two-part key resumes at the top of the lane and re-serves `a`"
        );
    }

    #[test]
    fn a_priority_cursor_walks_every_band_of_the_lane_exactly_once() {
        // Page size one, so every step is a fresh resolve from the cursor the
        // previous page minted — across three bands and a timestamp order that
        // is not the lane's order.
        let lane = follow_up_ordered_lane();
        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..lane.len() + 1 {
            let page =
                list_priority_attention_lane(AttentionLane::FollowUp, &lane, cursor.as_deref(), 1)
                    .unwrap();
            seen.extend(page.items.iter().map(|row| row.id.clone()));
            assert_eq!(page.total, lane.len(), "total is the lane");
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn a_priority_cursor_minted_before_the_band_still_resolves() {
        // One page turn can be in flight across the deploy that added the
        // band. A two-part cursor still names its row, and a two-part cursor
        // whose row is gone behaves exactly as it did before the change rather
        // than erroring.
        let lane = follow_up_ordered_lane();

        let decoded = decode_priority_attention_lane_cursor("300:b").unwrap();
        assert_eq!(decoded.priority, None);
        assert_eq!(decoded.updated_at, 300);
        assert_eq!(decoded.item_id, "b");

        let page = list_priority_attention_lane(AttentionLane::FollowUp, &lane, Some("300:b"), 10)
            .unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "d"],
            "a legacy cursor whose row is still there resumes exactly after it"
        );

        // And an id carrying the separator survives the round trip.
        let encoded = encode_priority_attention_lane_cursor(780, 300, "a:b");
        assert_eq!(encoded, "780:300:a%3Ab");
        assert_eq!(
            decode_priority_attention_lane_cursor(&encoded).unwrap(),
            PriorityAttentionLaneCursor {
                priority: Some(780),
                updated_at: 300,
                item_id: "a:b".to_string(),
            }
        );
    }

    #[test]
    fn the_lane_cursor_is_byte_for_byte_the_storage_index_cursor() {
        // Attention's wire contract did not change when its implementation
        // did, and this is why: the two encoders emit the same bytes, so the
        // index's keyset predicate can be applied to a cursor this facade
        // minted. Spelled out rather than round-tripped — a round trip
        // agrees with itself in any format at all.
        assert_eq!(
            encode_attention_lane_cursor(1_730_000_000_000, "item-a"),
            "1730000000000:item-a"
        );
        assert_eq!(
            encode_attention_lane_cursor(20, "a:b"),
            ListCursor::new(20, "a:b").encode(),
            "an id containing the separator must encode identically on both sides"
        );
        let decoded = decode_attention_lane_cursor("20:a%3Ab").expect("decoding");
        assert_eq!(decoded.updated_at, 20);
        assert_eq!(decoded.item_id, "a:b");
    }

    #[test]
    fn resurfacing_cursor_round_trips_exact_database_score() {
        let cursor = SurfacedCursor {
            surfaced_at: 10_000,
            salience_score: 0.619_111_161_635_338_7_f64,
            candidate_id: "candidate/with delimiter".to_string(),
        };

        let encoded = encode_resurfacing_attention_cursor(&cursor);
        let decoded = decode_resurfacing_attention_cursor(&encoded).unwrap();

        assert_eq!(decoded, cursor);
    }
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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
#[cfg(any(test, feature = "test-fixtures"))]
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
