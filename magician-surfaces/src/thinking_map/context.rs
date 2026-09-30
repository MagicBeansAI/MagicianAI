//! Live Thinking Map — bounded interpreter context builder (Phase 3, first slice).
//!
//! The interpreter LLM must see *enough* of the current board to author sensible,
//! grounded operations, but it must NEVER be handed the whole map — that would
//! be unbounded (a 5000-node board would blow the prompt budget) and would leak
//! irrelevant structure. This module projects a [`ThinkingMap`] + the incoming
//! utterance text into a small, fixed-shape [`InterpreterContext`] whose every
//! list is capped by an explicit [`ContextBudget`], plus an O(1)-ish aggregate
//! [`MapDigest`] so the model always knows the board's overall shape without
//! enumerating it.
//!
//! Pure + deterministic: no `Utc::now()`, no IO, stable ordering (updated_at
//! desc, then node_id). The ONLY inputs are the map, the utterance text, and the
//! budget.

use serde::Serialize;

use super::models::{EpistemicState, NodeKind, ThinkingMap, ThinkingNode};

/// Bounds on the produced [`InterpreterContext`]. Every list the context carries
/// is capped by one of these so the serialized context stays small regardless of
/// how large the underlying map grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    /// Max recently-updated nodes surfaced.
    pub max_recent_nodes: usize,
    /// Max graph neighbors of the focus node surfaced.
    pub max_neighbor_nodes: usize,
    /// Max characters of transcript/utterance text embedded (soft guard used by
    /// callers rendering the utterance; the builder itself does not truncate the
    /// utterance — that is the prompt renderer's job — but we carry the budget so
    /// the whole shape is in one place).
    pub max_transcript_chars: usize,
    /// Max nodes considered when computing the aggregate digest counts. The
    /// digest is capped so an enormous board cannot make context-building
    /// super-linear; counts beyond this are approximate but still bounded.
    pub max_digest_nodes: usize,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            max_recent_nodes: 12,
            max_neighbor_nodes: 8,
            max_transcript_chars: 2000,
            max_digest_nodes: 40,
        }
    }
}

/// A minimal, model-facing projection of a node — id + kind + label + epistemic
/// state only. Detail markdown / positions / source refs are intentionally
/// omitted to keep the context compact.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompactNode {
    pub node_id: String,
    pub kind: NodeKind,
    pub label: String,
    pub epistemic_state: EpistemicState,
}

impl CompactNode {
    fn of(node: &ThinkingNode) -> Self {
        Self {
            node_id: node.node_id.clone(),
            kind: node.kind,
            label: node.label.clone(),
            epistemic_state: node.epistemic_state,
        }
    }
}

/// A minimal, model-facing projection of an open clarification.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompactClarification {
    pub clarification_id: String,
    pub node_id: String,
    pub question: String,
}

/// Aggregate, fixed-size summary of the board — counts only, never the nodes
/// themselves. Lets the model reason about overall shape without enumeration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapDigest {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub live_nodes: usize,
    /// Counts by node kind (only kinds with a nonzero count, stable order).
    pub kind_counts: Vec<KindCount>,
    /// Counts by epistemic state (only states with a nonzero count, stable order).
    pub state_counts: Vec<StateCount>,
    /// True if the digest counts were computed over a truncated sample of nodes
    /// (board exceeded `max_digest_nodes`), so counts are a lower bound.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KindCount {
    pub kind: NodeKind,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateCount {
    pub state: EpistemicState,
    pub count: usize,
}

/// The bounded context handed to the interpreter LLM. Every list is capped; the
/// digest is a fixed-size aggregate. Serialized to compact JSON for the prompt.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InterpreterContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_node: Option<CompactNode>,
    pub recent_nodes: Vec<CompactNode>,
    pub lexical_matches: Vec<CompactNode>,
    pub neighbors: Vec<CompactNode>,
    pub open_clarifications: Vec<CompactClarification>,
    pub digest: MapDigest,
}

// ── Tokenization for lexical matching ────────────────────────────────────────

/// Very small English stopword set — filtered out of lexical-match tokens so a
/// shared "the"/"a" does not falsely match every node.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "to", "of", "in", "on", "for", "is", "it", "we", "i",
    "you", "that", "this", "with", "as", "at", "be", "by", "so", "if", "our", "was", "are",
];

/// Lowercase alphanumeric tokens of length >= 3 that are not stopwords.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .filter(|t| t.len() >= 3 && !STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// Does `label` share at least one significant token with `tokens`?
fn label_shares_token(label: &str, tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let label_tokens = tokenize(label);
    label_tokens.iter().any(|lt| tokens.contains(lt))
}

// ── Context builder ──────────────────────────────────────────────────────────

/// Build a bounded [`InterpreterContext`] from `map` + the utterance `text`.
///
/// The produced context is bounded regardless of map size:
/// - `recent_nodes` / `lexical_matches` / `neighbors` are each capped by the
///   budget.
/// - `digest` is a fixed-size aggregate (never the nodes themselves).
///
/// A node appears in AT MOST ONE list, preferring focus > neighbor > lexical >
/// recent. Ordering is deterministic: updated_at desc, then node_id asc.
pub fn build_context(
    map: &ThinkingMap,
    utterance_text: &str,
    budget: ContextBudget,
) -> InterpreterContext {
    let tokens = tokenize(utterance_text);

    // Deterministic candidate ordering: all non-tombstoned nodes sorted by
    // updated_at desc, then node_id asc. BTreeMap iteration is node_id asc, so we
    // just re-sort by (updated_at desc, node_id asc).
    let mut live: Vec<&ThinkingNode> = map.nodes.values().filter(|n| !n.tombstoned).collect();
    live.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    // Focus node (the shared-view active node), if present + live.
    let focus_id: Option<String> = map
        .view_state
        .active_node
        .as_ref()
        .filter(|id| map.nodes.get(*id).is_some_and(|n| !n.tombstoned))
        .cloned();
    let focus_node = focus_id
        .as_ref()
        .and_then(|id| map.nodes.get(id))
        .map(CompactNode::of);

    // Track claimed ids so each node lands in exactly one list.
    let mut claimed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(id) = &focus_id {
        claimed.insert(id.clone());
    }

    // Neighbors: nodes connected to the focus node via any non-tombstoned edge.
    let mut neighbors: Vec<CompactNode> = Vec::new();
    if let Some(fid) = &focus_id {
        // Collect neighbor ids deterministically (edges iterate in edge_id order).
        let mut neighbor_ids: Vec<String> = Vec::new();
        for edge in map.edges.values() {
            if edge.tombstoned {
                continue;
            }
            let other = if &edge.from_node == fid {
                Some(&edge.to_node)
            } else if &edge.to_node == fid {
                Some(&edge.from_node)
            } else {
                None
            };
            if let Some(oid) = other {
                if !neighbor_ids.contains(oid) {
                    neighbor_ids.push(oid.clone());
                }
            }
        }
        // Order neighbors by the same (updated_at desc, node_id asc) as `live`.
        let neighbor_set: std::collections::BTreeSet<&String> = neighbor_ids.iter().collect();
        for n in &live {
            if neighbors.len() >= budget.max_neighbor_nodes {
                break;
            }
            if neighbor_set.contains(&n.node_id) && !claimed.contains(&n.node_id) {
                claimed.insert(n.node_id.clone());
                neighbors.push(CompactNode::of(n));
            }
        }
    }

    // Lexical matches: live, unclaimed nodes whose label shares a token.
    let mut lexical_matches: Vec<CompactNode> = Vec::new();
    for n in &live {
        // No explicit cap here yet; truncated after collection so ordering wins.
        if claimed.contains(&n.node_id) {
            continue;
        }
        if label_shares_token(&n.label, &tokens) {
            claimed.insert(n.node_id.clone());
            lexical_matches.push(CompactNode::of(n));
        }
    }
    // Cap lexical (max reuses max_recent_nodes to avoid a separate knob).
    lexical_matches.truncate(budget.max_recent_nodes);

    // Recent: remaining live, unclaimed nodes, capped.
    let mut recent_nodes: Vec<CompactNode> = Vec::new();
    for n in &live {
        if recent_nodes.len() >= budget.max_recent_nodes {
            break;
        }
        if claimed.contains(&n.node_id) {
            continue;
        }
        claimed.insert(n.node_id.clone());
        recent_nodes.push(CompactNode::of(n));
    }

    // Open clarifications, capped (BTreeMap iteration = stable clarification_id order).
    let open_clarifications: Vec<CompactClarification> = map
        .clarifications
        .values()
        .filter(|c| matches!(c.state, super::models::ClarificationState::Open))
        .take(budget.max_recent_nodes)
        .map(|c| CompactClarification {
            clarification_id: c.clarification_id.clone(),
            node_id: c.node_id.clone(),
            question: c.question.clone(),
        })
        .collect();

    let digest = build_digest(map, &live, budget);

    InterpreterContext {
        focus_node,
        recent_nodes,
        lexical_matches,
        neighbors,
        open_clarifications,
        digest,
    }
}

/// Aggregate the board into a fixed-size digest. Counts by kind/state are drawn
/// from at most `max_digest_nodes` live nodes (sampled from the deterministically
/// ordered `live` slice) so digest building is bounded.
fn build_digest(map: &ThinkingMap, live: &[&ThinkingNode], budget: ContextBudget) -> MapDigest {
    let total_nodes = map.nodes.len();
    let total_edges = map.edges.values().filter(|e| !e.tombstoned).count();
    let live_nodes = live.len();

    let truncated = live.len() > budget.max_digest_nodes;
    let sample = &live[..live.len().min(budget.max_digest_nodes)];

    // Ordered accumulation keyed by the enum's serialized position via a Vec so
    // output ordering is stable (first-seen order over the deterministic sample).
    let mut kind_counts: Vec<KindCount> = Vec::new();
    let mut state_counts: Vec<StateCount> = Vec::new();
    for n in sample {
        match kind_counts.iter_mut().find(|k| k.kind == n.kind) {
            Some(k) => k.count += 1,
            None => kind_counts.push(KindCount {
                kind: n.kind,
                count: 1,
            }),
        }
        match state_counts
            .iter_mut()
            .find(|s| s.state == n.epistemic_state)
        {
            Some(s) => s.count += 1,
            None => state_counts.push(StateCount {
                state: n.epistemic_state,
                count: 1,
            }),
        }
    }
    // Stable ordering by descending count then by serialized tag for determinism.
    kind_counts.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| kind_tag(a.kind).cmp(kind_tag(b.kind)))
    });
    state_counts.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| state_tag(a.state).cmp(state_tag(b.state)))
    });

    MapDigest {
        total_nodes,
        total_edges,
        live_nodes,
        kind_counts,
        state_counts,
        truncated,
    }
}

fn kind_tag(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Idea => "idea",
        NodeKind::Fact => "fact",
        NodeKind::Question => "question",
        NodeKind::Decision => "decision",
        NodeKind::Option => "option",
        NodeKind::Risk => "risk",
        NodeKind::Action => "action",
        NodeKind::Metric => "metric",
        NodeKind::Assumption => "assumption",
        NodeKind::Evidence => "evidence",
        NodeKind::Group => "group",
    }
}

fn state_tag(state: EpistemicState) -> &'static str {
    match state {
        EpistemicState::Provisional => "provisional",
        EpistemicState::Asserted => "asserted",
        EpistemicState::Confirmed => "confirmed",
        EpistemicState::Contradicted => "contradicted",
        EpistemicState::Rejected => "rejected",
        EpistemicState::Resolved => "resolved",
        EpistemicState::Superseded => "superseded",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thinking_map::models::{
        new_edge_id, new_node_id, AssertionOrigin, Clarification, ClarificationState, EdgeKind,
        SharedViewState, ThinkingEdge, ThinkingMap, ThinkingMapSource, ThinkingNode,
    };

    const TS: &str = "2026-07-19T00:00:00Z";

    fn empty_map() -> ThinkingMap {
        ThinkingMap::new(
            "map-1".to_string(),
            "anonymous",
            "default",
            "Test",
            ThinkingMapSource::Solo,
            TS,
        )
    }

    fn node_with(id: &str, label: &str, updated_at: &str) -> ThinkingNode {
        ThinkingNode {
            node_id: id.to_string(),
            kind: NodeKind::Idea,
            label: label.to_string(),
            detail_markdown: None,
            epistemic_state: EpistemicState::Provisional,
            assertion_origin: AssertionOrigin::OwnerSpoken,
            confidence: 0.5,
            speaker: None,
            source_refs: vec![],
            parent_id: None,
            position: None,
            position_locked: false,
            promoted_refs: vec![],
            tombstoned: false,
            created_at: TS.to_string(),
            updated_at: updated_at.to_string(),
        }
    }

    #[test]
    fn context_is_bounded_for_large_map() {
        let mut map = empty_map();
        for i in 0..500 {
            // Vary updated_at so ordering is meaningful.
            let ts = format!("2026-07-19T00:{:02}:{:02}Z", i / 60, i % 60);
            let n = node_with(&format!("node-{i:04}"), &format!("label {i}"), &ts);
            map.nodes.insert(n.node_id.clone(), n);
        }
        let budget = ContextBudget::default();
        let ctx = build_context(&map, "nothing matches here xyzzy", budget);

        assert!(ctx.recent_nodes.len() <= budget.max_recent_nodes);
        assert!(ctx.lexical_matches.len() <= budget.max_recent_nodes);
        assert!(ctx.neighbors.len() <= budget.max_neighbor_nodes);
        assert_eq!(ctx.digest.total_nodes, 500);
        assert_eq!(ctx.digest.live_nodes, 500);
        assert!(ctx.digest.truncated, "digest sampled a subset");

        // The serialized context must NOT embed all 500 nodes.
        let json = serde_json::to_string(&ctx).unwrap();
        let embedded = ctx.recent_nodes.len() + ctx.lexical_matches.len() + ctx.neighbors.len();
        assert!(embedded <= budget.max_recent_nodes + budget.max_neighbor_nodes);
        assert!(embedded < 500);
        // node-0000 has the EARLIEST updated_at → lowest recency priority and no
        // lexical/neighbor claim → it must not fit in any capped list.
        assert!(
            !json.contains("node-0000"),
            "the least-recent node must not be embedded"
        );
    }

    #[test]
    fn focus_neighbors_and_lexical_matches() {
        let mut map = empty_map();
        let f = node_with("focus", "central topic", "2026-07-19T00:10:00Z");
        let nbr = node_with("nbr", "connected neighbor", "2026-07-19T00:09:00Z");
        let lex = node_with("lex", "hiring plan", "2026-07-19T00:08:00Z");
        let other = node_with("other", "unrelated stuff", "2026-07-19T00:07:00Z");
        map.nodes.insert(f.node_id.clone(), f);
        map.nodes.insert(nbr.node_id.clone(), nbr);
        map.nodes.insert(lex.node_id.clone(), lex);
        map.nodes.insert(other.node_id.clone(), other);
        map.edges.insert(
            "e1".to_string(),
            ThinkingEdge {
                edge_id: "e1".to_string(),
                from_node: "focus".to_string(),
                to_node: "nbr".to_string(),
                kind: EdgeKind::RelatedTo,
                assertion_origin: AssertionOrigin::OwnerSpoken,
                tombstoned: false,
                created_at: TS.to_string(),
                updated_at: TS.to_string(),
            },
        );
        map.view_state = SharedViewState {
            active_node: Some("focus".to_string()),
            ..Default::default()
        };

        let ctx = build_context(
            &map,
            "let's talk about the hiring plan",
            ContextBudget::default(),
        );

        assert_eq!(ctx.focus_node.as_ref().unwrap().node_id, "focus");
        // Neighbor is present and not double-listed.
        assert!(ctx.neighbors.iter().any(|n| n.node_id == "nbr"));
        // Lexical match on "hiring"/"plan".
        assert!(ctx.lexical_matches.iter().any(|n| n.node_id == "lex"));
        // Each node appears in at most one list.
        let mut seen = std::collections::BTreeSet::new();
        for n in ctx
            .recent_nodes
            .iter()
            .chain(&ctx.lexical_matches)
            .chain(&ctx.neighbors)
        {
            assert!(
                seen.insert(n.node_id.clone()),
                "node {} duplicated",
                n.node_id
            );
        }
        // Focus is not also in another list.
        assert!(!seen.contains("focus"));
    }

    #[test]
    fn open_clarifications_only() {
        let mut map = empty_map();
        let n = node_with("n1", "a node", TS);
        map.nodes.insert(n.node_id.clone(), n);
        map.clarifications.insert(
            "c1".to_string(),
            Clarification {
                clarification_id: "c1".to_string(),
                node_id: "n1".to_string(),
                question: "open?".to_string(),
                state: ClarificationState::Open,
                answer: None,
                created_at: TS.to_string(),
                resolved_at: None,
            },
        );
        map.clarifications.insert(
            "c2".to_string(),
            Clarification {
                clarification_id: "c2".to_string(),
                node_id: "n1".to_string(),
                question: "answered?".to_string(),
                state: ClarificationState::Answered,
                answer: Some("yes".to_string()),
                created_at: TS.to_string(),
                resolved_at: Some(TS.to_string()),
            },
        );
        let ctx = build_context(&map, "hello", ContextBudget::default());
        assert_eq!(ctx.open_clarifications.len(), 1);
        assert_eq!(ctx.open_clarifications[0].clarification_id, "c1");
    }

    #[test]
    fn tombstoned_nodes_excluded() {
        let mut map = empty_map();
        let mut n = node_with("dead", "tombstoned node", TS);
        n.tombstoned = true;
        map.nodes.insert(n.node_id.clone(), n);
        let live = node_with("live", "live node", TS);
        map.nodes.insert(live.node_id.clone(), live);

        let ctx = build_context(&map, "hello", ContextBudget::default());
        assert_eq!(ctx.digest.live_nodes, 1);
        assert_eq!(ctx.digest.total_nodes, 2);
        assert!(ctx.recent_nodes.iter().all(|n| n.node_id != "dead"));
    }

    #[test]
    fn determinism_stable_across_runs() {
        // Building the same map+utterance twice yields identical output.
        let mut map = empty_map();
        for i in 0..30 {
            let n = node_with(
                &new_node_id(),
                &format!("topic {i}"),
                &format!("2026-07-19T00:00:{:02}Z", i),
            );
            map.nodes.insert(n.node_id.clone(), n);
        }
        // Add a couple of edges so the code path is exercised.
        let ids: Vec<String> = map.nodes.keys().take(2).cloned().collect();
        map.edges.insert(
            new_edge_id(),
            ThinkingEdge {
                edge_id: "e".to_string(),
                from_node: ids[0].clone(),
                to_node: ids[1].clone(),
                kind: EdgeKind::RelatedTo,
                assertion_origin: AssertionOrigin::OwnerSpoken,
                tombstoned: false,
                created_at: TS.to_string(),
                updated_at: TS.to_string(),
            },
        );
        let a = build_context(&map, "topic 5", ContextBudget::default());
        let b = build_context(&map, "topic 5", ContextBudget::default());
        assert_eq!(a, b);
    }
}
