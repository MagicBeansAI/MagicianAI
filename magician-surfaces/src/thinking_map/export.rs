//! Live Thinking Map — deterministic Markdown export (plan Phase 9 item 2).
//!
//! A pure render of a [`ThinkingMap`] into a readable Markdown document:
//! title header, a **Board** tree walked over the `parent_id` hierarchy
//! (indented bullets carrying the label, a `kind` tag, an origin/state tag
//! for non-owner-asserted content — e.g. `✦ AI-suggested` — and
//! `detail_markdown` as a nested blockquote), then **Connections** (semantic
//! edges), **Open clarifications**, and **Pending proposals** sections.
//!
//! Determinism: the render reads ONLY the supplied map (no clock, no
//! randomness, no I/O), and every collection it walks is a `BTreeMap`, so
//! equal maps always produce byte-identical output.
//!
//! Inclusion rules ([`ExportOptions`]):
//! - `include_provisional` (default `true`) — nodes still in the
//!   `provisional` epistemic state;
//! - `include_superseded` (default `false`) — rejected / superseded /
//!   contradicted nodes;
//! - tombstoned content is NEVER exported, regardless of options.
//!
//! Edges render only when both endpoints are visible; open clarifications
//! only for visible nodes. A node whose parent is filtered out (or missing)
//! surfaces at the root — content never silently disappears with its parent.

use std::collections::{BTreeMap, BTreeSet};

use super::models::{
    AssertionOrigin, ClarificationState, EdgeKind, EpistemicState, MapLifecycle, NodeKind,
    ProposalState, ThinkingMap, ThinkingMapSource, ThinkingNode,
};

/// Inclusion controls for [`render_markdown`]. `Default` = provisional
/// included, superseded/rejected/contradicted excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOptions {
    /// Include nodes still in the `provisional` epistemic state.
    pub include_provisional: bool,
    /// Include rejected / superseded / contradicted nodes (tagged with their
    /// state). Tombstoned content is never included, even with this set.
    pub include_superseded: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            include_provisional: true,
            include_superseded: false,
        }
    }
}

// ── Token helpers (stable, wire-format snake_case) ───────────────────────────

fn kind_token(kind: NodeKind) -> &'static str {
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

fn edge_kind_token(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::RelatedTo => "related_to",
        EdgeKind::Supports => "supports",
        EdgeKind::Contradicts => "contradicts",
        EdgeKind::Answers => "answers",
        EdgeKind::DependsOn => "depends_on",
        EdgeKind::LeadsTo => "leads_to",
        EdgeKind::AlternativeTo => "alternative_to",
        EdgeKind::Measures => "measures",
        EdgeKind::GroupedUnder => "grouped_under",
    }
}

fn state_token(state: EpistemicState) -> &'static str {
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

fn lifecycle_token(lifecycle: MapLifecycle) -> &'static str {
    match lifecycle {
        MapLifecycle::Active => "active",
        MapLifecycle::Paused => "paused",
        MapLifecycle::Archived => "archived",
        MapLifecycle::Deleted => "deleted",
    }
}

fn source_token(source: &ThinkingMapSource) -> &'static str {
    match source {
        ThinkingMapSource::Solo => "solo",
        ThinkingMapSource::Meeting { .. } => "meeting",
        ThinkingMapSource::Observe { .. } => "observe",
        ThinkingMapSource::Chat { .. } => "chat",
        ThinkingMapSource::Imported { .. } => "imported",
        ThinkingMapSource::Tutor { .. } => "tutor",
    }
}

/// Origin tag for non-owner content. Owner-authored assertions carry no tag.
fn origin_tag(origin: AssertionOrigin) -> Option<&'static str> {
    match origin {
        AssertionOrigin::OwnerSpoken | AssertionOrigin::OwnerEdited => None,
        AssertionOrigin::ModelInferred => Some("✦ AI-suggested"),
        AssertionOrigin::ParticipantSpoken => Some("participant"),
        AssertionOrigin::ImportedSource => Some("imported"),
        AssertionOrigin::SystemDerived => Some("system"),
    }
}

/// The tag list rendered after a node's kind: origin (when not owner) +
/// epistemic state (when not the plain `asserted` baseline). Empty for
/// owner-asserted content.
fn node_tags(node: &ThinkingNode) -> Vec<&'static str> {
    let mut tags = Vec::new();
    if let Some(origin) = origin_tag(node.assertion_origin) {
        tags.push(origin);
    }
    if node.epistemic_state != EpistemicState::Asserted {
        tags.push(state_token(node.epistemic_state));
    }
    tags
}

/// Collapse all whitespace runs (incl. newlines) to single spaces so labels /
/// titles / questions can never break the surrounding Markdown structure.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `1 operation` / `3 operations` — naive plural is fine for our nouns.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn node_visible(node: &ThinkingNode, opts: &ExportOptions) -> bool {
    if node.tombstoned {
        return false;
    }
    if !opts.include_superseded
        && matches!(
            node.epistemic_state,
            EpistemicState::Rejected | EpistemicState::Superseded | EpistemicState::Contradicted
        )
    {
        return false;
    }
    if !opts.include_provisional && node.epistemic_state == EpistemicState::Provisional {
        return false;
    }
    true
}

/// Emit one node (bullet + optional detail blockquote) then its children,
/// depth-first. `visited` guards against pathological `parent_id` cycles —
/// each node renders at most once.
fn emit_node<'a>(
    id: &'a str,
    depth: usize,
    visible: &BTreeMap<&'a str, &'a ThinkingNode>,
    children: &BTreeMap<&'a str, Vec<&'a str>>,
    visited: &mut BTreeSet<&'a str>,
    out: &mut Vec<String>,
) {
    if !visited.insert(id) {
        return;
    }
    let Some(node) = visible.get(id) else {
        return;
    };
    let indent = "  ".repeat(depth);
    let mut line = format!(
        "{indent}- **{}** `{}`",
        single_line(&node.label),
        kind_token(node.kind)
    );
    let tags = node_tags(node);
    if !tags.is_empty() {
        line.push_str(&format!(" — _{}_", tags.join(", ")));
    }
    out.push(line);
    if let Some(detail) = node.detail_markdown.as_deref() {
        if !detail.trim().is_empty() {
            for detail_line in detail.lines() {
                out.push(format!("{indent}  > {}", detail_line.trim_end()));
            }
        }
    }
    if let Some(kids) = children.get(id) {
        for &kid in kids {
            emit_node(kid, depth + 1, visible, children, visited, out);
        }
    }
}

/// Render `map` to a deterministic Markdown document. Pure: no clock, no
/// randomness, no I/O — equal `(map, opts)` inputs always produce
/// byte-identical output (all walked collections are `BTreeMap`s).
pub fn render_markdown(map: &ThinkingMap, opts: &ExportOptions) -> String {
    let mut out: Vec<String> = Vec::new();

    // ── Header ───────────────────────────────────────────────────────────────
    let title = single_line(&map.title);
    out.push(format!(
        "# {}",
        if title.is_empty() {
            "(untitled map)".to_string()
        } else {
            title
        }
    ));
    out.push(String::new());
    out.push(format!(
        "`{}` map · lifecycle `{}` · revision {}",
        source_token(&map.source),
        lifecycle_token(map.lifecycle),
        map.revision
    ));
    out.push(String::new());

    // ── Visible node set + parent tree (BTreeMap order = by node_id) ─────────
    let visible: BTreeMap<&str, &ThinkingNode> = map
        .nodes
        .iter()
        .filter(|(_, node)| node_visible(node, opts))
        .map(|(id, node)| (id.as_str(), node))
        .collect();

    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut roots: Vec<&str> = Vec::new();
    for (&id, node) in &visible {
        // A parent that is hidden/unknown promotes the child to root level.
        match node
            .parent_id
            .as_deref()
            .filter(|parent| visible.contains_key(parent))
        {
            Some(parent) => children.entry(parent).or_default().push(id),
            None => roots.push(id),
        }
    }

    out.push("## Board".to_string());
    out.push(String::new());
    if visible.is_empty() {
        out.push("_No visible nodes._".to_string());
    } else {
        let mut visited: BTreeSet<&str> = BTreeSet::new();
        for &root in &roots {
            emit_node(root, 0, &visible, &children, &mut visited, &mut out);
        }
        // Cycle defense: a `parent_id` loop has no root, so its members are
        // still unvisited — emit them at top level (id order) rather than
        // dropping content.
        let leftovers: Vec<&str> = visible
            .keys()
            .copied()
            .filter(|id| !visited.contains(id))
            .collect();
        for id in leftovers {
            emit_node(id, 0, &visible, &children, &mut visited, &mut out);
        }
    }

    // ── Semantic edges (both endpoints must be visible) ──────────────────────
    let mut edge_lines: Vec<String> = Vec::new();
    for edge in map.edges.values() {
        if edge.tombstoned {
            continue;
        }
        let (Some(from), Some(to)) = (
            visible.get(edge.from_node.as_str()),
            visible.get(edge.to_node.as_str()),
        ) else {
            continue;
        };
        let mut line = format!(
            "- **{}** —`{}`→ **{}**",
            single_line(&from.label),
            edge_kind_token(edge.kind),
            single_line(&to.label)
        );
        if let Some(tag) = origin_tag(edge.assertion_origin) {
            line.push_str(&format!(" _({tag})_"));
        }
        edge_lines.push(line);
    }

    // ── Open clarifications (visible nodes only) ─────────────────────────────
    let mut clarification_lines: Vec<String> = Vec::new();
    for clarification in map.clarifications.values() {
        if clarification.state != ClarificationState::Open {
            continue;
        }
        let Some(node) = visible.get(clarification.node_id.as_str()) else {
            continue;
        };
        clarification_lines.push(format!(
            "- **{}**: {}",
            single_line(&node.label),
            single_line(&clarification.question)
        ));
    }

    // ── Pending proposals ─────────────────────────────────────────────────────
    let mut proposal_lines: Vec<String> = Vec::new();
    for proposal in map.proposals.values() {
        if proposal.state != ProposalState::Proposed {
            continue;
        }
        proposal_lines.push(format!(
            "- {} _({} · {})_",
            single_line(&proposal.rationale),
            count(proposal.operations.len(), "operation"),
            count(proposal.affected_node_ids.len(), "affected node"),
        ));
    }

    for (heading, lines) in [
        ("## Connections", edge_lines),
        ("## Open clarifications", clarification_lines),
        ("## Pending proposals", proposal_lines),
    ] {
        if lines.is_empty() {
            continue;
        }
        out.push(String::new());
        out.push(heading.to_string());
        out.push(String::new());
        out.extend(lines);
    }

    let mut rendered = out.join("\n");
    rendered.push('\n');
    rendered
}

#[cfg(test)]
mod tests {
    use super::super::models::{Clarification, RestructureProposal, ThinkingEdge};
    use super::super::operations::OperationActor;
    use super::*;

    const TS: &str = "2026-07-22T00:00:00Z";

    fn node(
        id: &str,
        kind: NodeKind,
        label: &str,
        origin: AssertionOrigin,
        state: EpistemicState,
        parent: Option<&str>,
    ) -> ThinkingNode {
        ThinkingNode {
            node_id: id.to_string(),
            kind,
            label: label.to_string(),
            detail_markdown: None,
            epistemic_state: state,
            assertion_origin: origin,
            confidence: 0.7,
            speaker: None,
            source_refs: vec![],
            parent_id: parent.map(str::to_string),
            position: None,
            position_locked: false,
            promoted_refs: vec![],
            tombstoned: false,
            created_at: TS.to_string(),
            updated_at: TS.to_string(),
        }
    }

    fn edge(
        id: &str,
        from: &str,
        to: &str,
        kind: EdgeKind,
        origin: AssertionOrigin,
        tombstoned: bool,
    ) -> ThinkingEdge {
        ThinkingEdge {
            edge_id: id.to_string(),
            from_node: from.to_string(),
            to_node: to.to_string(),
            kind,
            assertion_origin: origin,
            tombstoned,
            created_at: TS.to_string(),
            updated_at: TS.to_string(),
        }
    }

    /// One map exercising every render path: hierarchy, detail blockquote,
    /// origin/state tags, superseded + tombstoned exclusion, dangling-edge
    /// suppression, answered-clarification exclusion, resolved proposals.
    fn fixture() -> ThinkingMap {
        let mut map = ThinkingMap::new(
            "map-1".to_string(),
            "anonymous",
            "default",
            "Launch plan",
            ThinkingMapSource::Solo,
            TS,
        );
        map.revision = 4;

        let mut root = node(
            "a-root",
            NodeKind::Decision,
            "Ship v1",
            AssertionOrigin::OwnerSpoken,
            EpistemicState::Asserted,
            None,
        );
        root.detail_markdown = Some("Ship the **web** build first.\nThen iOS.".to_string());
        map.nodes.insert("a-root".to_string(), root);
        map.nodes.insert(
            "b-risk".to_string(),
            node(
                "b-risk",
                NodeKind::Risk,
                "Churn risk",
                AssertionOrigin::ModelInferred,
                EpistemicState::Provisional,
                Some("a-root"),
            ),
        );
        map.nodes.insert(
            "c-old".to_string(),
            node(
                "c-old",
                NodeKind::Idea,
                "Old framing",
                AssertionOrigin::OwnerSpoken,
                EpistemicState::Superseded,
                Some("a-root"),
            ),
        );
        let mut gone = node(
            "d-gone",
            NodeKind::Idea,
            "Deleted thought",
            AssertionOrigin::OwnerSpoken,
            EpistemicState::Asserted,
            None,
        );
        gone.tombstoned = true;
        map.nodes.insert("d-gone".to_string(), gone);
        map.nodes.insert(
            "e-mail".to_string(),
            node(
                "e-mail",
                NodeKind::Action,
                "Write launch email",
                AssertionOrigin::OwnerEdited,
                EpistemicState::Confirmed,
                None,
            ),
        );

        map.edges.insert(
            "e1".to_string(),
            edge(
                "e1",
                "b-risk",
                "a-root",
                EdgeKind::Contradicts,
                AssertionOrigin::ModelInferred,
                false,
            ),
        );
        map.edges.insert(
            "e2".to_string(),
            edge(
                "e2",
                "c-old",
                "a-root",
                EdgeKind::RelatedTo,
                AssertionOrigin::OwnerSpoken,
                false,
            ),
        );
        map.edges.insert(
            "e3".to_string(),
            edge(
                "e3",
                "e-mail",
                "a-root",
                EdgeKind::DependsOn,
                AssertionOrigin::OwnerSpoken,
                true,
            ),
        );

        map.clarifications.insert(
            "cl-a".to_string(),
            Clarification {
                clarification_id: "cl-a".to_string(),
                node_id: "a-root".to_string(),
                question: "Which platform first?".to_string(),
                state: ClarificationState::Open,
                answer: None,
                created_at: TS.to_string(),
                resolved_at: None,
            },
        );
        map.clarifications.insert(
            "cl-b".to_string(),
            Clarification {
                clarification_id: "cl-b".to_string(),
                node_id: "b-risk".to_string(),
                question: "Is churn actually rising?".to_string(),
                state: ClarificationState::Open,
                answer: None,
                created_at: TS.to_string(),
                resolved_at: None,
            },
        );
        map.clarifications.insert(
            "cl-c".to_string(),
            Clarification {
                clarification_id: "cl-c".to_string(),
                node_id: "a-root".to_string(),
                question: "Already answered".to_string(),
                state: ClarificationState::Answered,
                answer: Some("yes".to_string()),
                created_at: TS.to_string(),
                resolved_at: Some(TS.to_string()),
            },
        );

        map.proposals.insert(
            "p1".to_string(),
            RestructureProposal {
                proposal_id: "p1".to_string(),
                proposed_by: OperationActor::Model { trace_id: None },
                rationale: "Group the risks".to_string(),
                operations: vec![],
                state: ProposalState::Proposed,
                affected_node_ids: vec!["a-root".to_string(), "b-risk".to_string()],
                created_at: TS.to_string(),
                resolved_at: None,
            },
        );
        map.proposals.insert(
            "p2".to_string(),
            RestructureProposal {
                proposal_id: "p2".to_string(),
                proposed_by: OperationActor::Model { trace_id: None },
                rationale: "Old proposal".to_string(),
                operations: vec![],
                state: ProposalState::Rejected,
                affected_node_ids: vec![],
                created_at: TS.to_string(),
                resolved_at: Some(TS.to_string()),
            },
        );

        map
    }

    #[test]
    fn default_render_matches_snapshot() {
        let expected = r#"# Launch plan

`solo` map · lifecycle `active` · revision 4

## Board

- **Ship v1** `decision`
  > Ship the **web** build first.
  > Then iOS.
  - **Churn risk** `risk` — _✦ AI-suggested, provisional_
- **Write launch email** `action` — _confirmed_

## Connections

- **Churn risk** —`contradicts`→ **Ship v1** _(✦ AI-suggested)_

## Open clarifications

- **Ship v1**: Which platform first?
- **Churn risk**: Is churn actually rising?

## Pending proposals

- Group the risks _(0 operations · 2 affected nodes)_
"#;
        assert_eq!(
            render_markdown(&fixture(), &ExportOptions::default()),
            expected
        );
    }

    #[test]
    fn render_is_deterministic() {
        let map = fixture();
        let opts = ExportOptions::default();
        assert_eq!(render_markdown(&map, &opts), render_markdown(&map, &opts));
        // A structural clone renders byte-identically too.
        assert_eq!(
            render_markdown(&map.clone(), &opts),
            render_markdown(&map, &opts)
        );
    }

    #[test]
    fn default_options_are_provisional_in_superseded_out() {
        let opts = ExportOptions::default();
        assert!(opts.include_provisional);
        assert!(!opts.include_superseded);
    }

    #[test]
    fn include_superseded_reveals_discredited_content_tagged() {
        let rendered = render_markdown(
            &fixture(),
            &ExportOptions {
                include_provisional: true,
                include_superseded: true,
            },
        );
        // The superseded child appears (nested + tagged) and its edge returns.
        assert!(
            rendered.contains("  - **Old framing** `idea` — _superseded_"),
            "rendered was:\n{rendered}"
        );
        assert!(
            rendered.contains("- **Old framing** —`related_to`→ **Ship v1**"),
            "rendered was:\n{rendered}"
        );
        // Tombstoned content stays out no matter what.
        assert!(!rendered.contains("Deleted thought"));
        assert!(!rendered.contains("depends_on"));
    }

    #[test]
    fn exclude_provisional_drops_nodes_edges_and_clarifications() {
        let rendered = render_markdown(
            &fixture(),
            &ExportOptions {
                include_provisional: false,
                include_superseded: false,
            },
        );
        assert!(
            !rendered.contains("Churn risk"),
            "rendered was:\n{rendered}"
        );
        // Its edge dangles → suppressed; its clarification is hidden with it.
        assert!(!rendered.contains("contradicts"));
        assert!(!rendered.contains("Is churn actually rising?"));
        // Owner-asserted content is untouched.
        assert!(rendered.contains("- **Ship v1** `decision`"));
        assert!(rendered.contains("- **Ship v1**: Which platform first?"));
    }

    #[test]
    fn hidden_parent_promotes_child_to_root_and_cycles_still_render() {
        let mut map = ThinkingMap::new(
            "map-2".to_string(),
            "anonymous",
            "default",
            "Edge cases",
            ThinkingMapSource::Solo,
            TS,
        );
        // Child of a nonexistent parent → root-level bullet (no indent).
        map.nodes.insert(
            "orphan".to_string(),
            node(
                "orphan",
                NodeKind::Fact,
                "Orphaned fact",
                AssertionOrigin::OwnerSpoken,
                EpistemicState::Asserted,
                Some("ghost-parent"),
            ),
        );
        // A parent_id cycle: neither is a root, both must still render once.
        map.nodes.insert(
            "cyc-a".to_string(),
            node(
                "cyc-a",
                NodeKind::Idea,
                "Cycle A",
                AssertionOrigin::OwnerSpoken,
                EpistemicState::Asserted,
                Some("cyc-b"),
            ),
        );
        map.nodes.insert(
            "cyc-b".to_string(),
            node(
                "cyc-b",
                NodeKind::Idea,
                "Cycle B",
                AssertionOrigin::OwnerSpoken,
                EpistemicState::Asserted,
                Some("cyc-a"),
            ),
        );

        let rendered = render_markdown(&map, &ExportOptions::default());
        assert!(
            rendered.contains("\n- **Orphaned fact** `fact`"),
            "orphan should render at root level, rendered was:\n{rendered}"
        );
        assert_eq!(rendered.matches("**Cycle A**").count(), 1);
        assert_eq!(rendered.matches("**Cycle B**").count(), 1);
    }

    #[test]
    fn empty_map_renders_placeholder_and_omits_empty_sections() {
        let map = ThinkingMap::new(
            "map-3".to_string(),
            "anonymous",
            "default",
            "  ", // whitespace-only title falls back
            ThinkingMapSource::Solo,
            TS,
        );
        let rendered = render_markdown(&map, &ExportOptions::default());
        assert!(rendered.starts_with("# (untitled map)\n"));
        assert!(rendered.contains("_No visible nodes._"));
        assert!(!rendered.contains("## Connections"));
        assert!(!rendered.contains("## Open clarifications"));
        assert!(!rendered.contains("## Pending proposals"));
    }

    #[test]
    fn labels_and_questions_are_collapsed_to_single_lines() {
        let mut map = ThinkingMap::new(
            "map-4".to_string(),
            "anonymous",
            "default",
            "Line\nbreaks",
            ThinkingMapSource::Solo,
            TS,
        );
        map.nodes.insert(
            "n1".to_string(),
            node(
                "n1",
                NodeKind::Idea,
                "multi\nline   label",
                AssertionOrigin::OwnerSpoken,
                EpistemicState::Asserted,
                None,
            ),
        );
        let rendered = render_markdown(&map, &ExportOptions::default());
        assert!(rendered.starts_with("# Line breaks\n"));
        assert!(
            rendered.contains("- **multi line label** `idea`"),
            "rendered was:\n{rendered}"
        );
    }
}
