//! Live Thinking Map — canonical wire fixture emitter (S1a).
//!
//! Constructs representative canonical values with FIXED ids/timestamps
//! (fully deterministic — no `Utc::now()`, no UUIDs) and writes pretty JSON to
//! `magios/Magios/ThinkingMapCanonical/Fixtures/<name>.json`.
//!
//! These fixtures are the authoritative wire contract the iOS `LTM.*` Codable
//! models must round-trip losslessly. Because every value here is fixed, the
//! output is byte-stable across runs (only timestamps/ids that we hard-code).
//!
//! Run:
//! ```sh
//! CARGO_TARGET_DIR=/Volumes/build/magician/builds \
//!   cargo run -p magician --example thinking_map_wire_fixtures
//! ```
//!
//! The response fixtures (`response_applied` / `response_idempotent` /
//! `response_no_operations`) are built with `serde_json::json!` to EXACTLY
//! match the shapes emitted by the handlers in
//! `magician_v2::api::thinking_maps_api` (the operations/interpret/patch
//! responses).

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use magician_surfaces::thinking_map::{
    AppliedEnvelopeRecord, AssertionOrigin, Clarification, ClarificationState, EdgeKind,
    EpistemicState, MapEvent, MapLifecycle, MapManifest, MapOperation, MapOperationEnvelope,
    MapSummary, ModelTraceRef, NodeKind, NodePreview, NodePreviewEdge, NodePreviewNode,
    OperationActor, Position, PromotedRef, PromotionKind, SharedViewState, SourceRef, SpeakerRef,
    ThinkingEdge, ThinkingMap, ThinkingMapSource, ThinkingNode, ViewLens,
};

const TS: &str = "2026-07-20T00:00:00Z";
const TS2: &str = "2026-07-20T01:00:00Z";

/// Stable node ids for the `node_preview` fixture — the preview's edge endpoints
/// must match its node ids exactly, so both are named rather than repeated.
const PREVIEW_ROOT_ID: &str = "11111111-1111-1111-1111-111111111111";
const PREVIEW_CHILD_ID: &str = "22222222-2222-2222-2222-222222222222";

/// Node 1 — an owner-spoken, asserted decision node with a speaker + a promoted
/// task ref + a source ref.
fn node_owner_spoken() -> ThinkingNode {
    ThinkingNode {
        node_id: "node-1".to_string(),
        kind: NodeKind::Decision,
        label: "Ship the iOS thinking map".to_string(),
        detail_markdown: Some("**Decide** to ship v1 on iOS.".to_string()),
        epistemic_state: EpistemicState::Asserted,
        assertion_origin: AssertionOrigin::OwnerSpoken,
        confidence: 0.9,
        speaker: Some(SpeakerRef {
            speaker_id: "speaker-owner".to_string(),
            display_name: Some("Alex".to_string()),
        }),
        source_refs: vec![SourceRef {
            utterance_id: Some("utterance-1".to_string()),
            thread_id: Some("thread-42".to_string()),
            quote: Some("Let's ship the iOS thinking map.".to_string()),
            timestamp: Some(TS.to_string()),
        }],
        parent_id: None,
        position: Some(Position { x: 120.0, y: 48.5 }),
        position_locked: true,
        promoted_refs: vec![PromotedRef {
            destination_kind: PromotionKind::Task,
            object_id: "task-1".to_string(),
            linked_at: TS.to_string(),
        }],
        tombstoned: false,
        created_at: TS.to_string(),
        updated_at: TS.to_string(),
    }
}

/// Node 2 — a model-inferred, provisional risk node WITH a source_ref and a
/// parent pointing at node-1.
fn node_model_inferred() -> ThinkingNode {
    ThinkingNode {
        node_id: "node-2".to_string(),
        kind: NodeKind::Risk,
        label: "Wire contract drift between Rust and Swift".to_string(),
        detail_markdown: None,
        epistemic_state: EpistemicState::Provisional,
        assertion_origin: AssertionOrigin::ModelInferred,
        confidence: 0.4,
        speaker: None,
        source_refs: vec![SourceRef {
            utterance_id: Some("utterance-2".to_string()),
            thread_id: None,
            quote: Some("the field names could diverge".to_string()),
            timestamp: Some(TS.to_string()),
        }],
        parent_id: Some("node-1".to_string()),
        position: None,
        position_locked: false,
        promoted_refs: vec![],
        tombstoned: false,
        created_at: TS.to_string(),
        updated_at: TS.to_string(),
    }
}

fn related_edge() -> ThinkingEdge {
    ThinkingEdge {
        edge_id: "edge-1".to_string(),
        from_node: "node-1".to_string(),
        to_node: "node-2".to_string(),
        kind: EdgeKind::RelatedTo,
        assertion_origin: AssertionOrigin::ModelInferred,
        tombstoned: false,
        created_at: TS.to_string(),
        updated_at: TS.to_string(),
    }
}

fn open_clarification() -> Clarification {
    Clarification {
        clarification_id: "clar-1".to_string(),
        node_id: "node-2".to_string(),
        question: "Which fields are most likely to drift?".to_string(),
        state: ClarificationState::Open,
        answer: None,
        created_at: TS.to_string(),
        resolved_at: None,
    }
}

/// The canonical populated `ThinkingMap`.
fn build_map() -> ThinkingMap {
    let mut map = ThinkingMap::new(
        "map-1".to_string(),
        "anonymous",
        "default",
        "iOS thinking map migration",
        ThinkingMapSource::Meeting {
            thread_id: "thread-42".to_string(),
        },
        TS,
    );
    map.revision = 4;
    map.lifecycle = MapLifecycle::Active;
    map.view_state = SharedViewState {
        active_node: Some("node-1".to_string()),
        lens: ViewLens::Outline,
    };
    map.updated_at = TS2.to_string();

    let mut nodes = BTreeMap::new();
    nodes.insert("node-1".to_string(), node_owner_spoken());
    nodes.insert("node-2".to_string(), node_model_inferred());
    map.nodes = nodes;

    let mut edges = BTreeMap::new();
    edges.insert("edge-1".to_string(), related_edge());
    map.edges = edges;

    let mut clarifications = BTreeMap::new();
    clarifications.insert("clar-1".to_string(), open_clarification());
    map.clarifications = clarifications;

    // One applied-envelope ledger record (bookkeeping) so the field is exercised.
    let mut applied = VecDeque::new();
    applied.push_back(AppliedEnvelopeRecord {
        envelope_id: "envelope-1".to_string(),
        idempotency_key: "idem-1".to_string(),
        resulting_revision: 4,
    });
    map.applied_envelopes = applied;

    map
}

/// A representative spread of operations covering the requested variants plus
/// the owner-only metadata ops.
fn representative_operations() -> Vec<MapOperation> {
    vec![
        MapOperation::AddNode {
            node: node_model_inferred(),
        },
        // detail_markdown: Some(Some("x")) → set to "x".
        MapOperation::UpdateNode {
            node_id: "node-1".to_string(),
            label: Some("Ship the iOS thinking map (v1)".to_string()),
            detail_markdown: Some(Some("Updated detail body.".to_string())),
            confidence: Some(0.95),
        },
        MapOperation::SetNodeKind {
            node_id: "node-2".to_string(),
            kind: NodeKind::Assumption,
        },
        MapOperation::SetEpistemicState {
            node_id: "node-2".to_string(),
            state: EpistemicState::Confirmed,
        },
        MapOperation::Connect {
            edge: related_edge(),
        },
        MapOperation::MoveToParent {
            node_id: "node-2".to_string(),
            parent_id: Some("node-1".to_string()),
        },
        MapOperation::TombstoneNode {
            node_id: "node-2".to_string(),
        },
        MapOperation::CreateClarification {
            clarification: open_clarification(),
        },
        MapOperation::SetTitle {
            title: "iOS thinking map migration (renamed)".to_string(),
        },
        MapOperation::SetLifecycle {
            lifecycle: MapLifecycle::Archived,
        },
    ]
}

/// The canonical operation envelope (owner actor, with a model_trace ref set so
/// the optional field is exercised).
fn build_envelope() -> MapOperationEnvelope {
    let mut envelope = MapOperationEnvelope::new(
        "envelope-1".to_string(),
        "map-1".to_string(),
        4,
        OperationActor::Owner {
            principal: "anonymous".to_string(),
        },
        "idem-1",
        representative_operations(),
        TS,
    );
    envelope.utterance_id = Some("utterance-3".to_string());
    envelope.model_trace = Some(ModelTraceRef {
        trace_id: "trace-1".to_string(),
        model_profile: Some("thinking_map_interpret".to_string()),
    });
    envelope
}

/// The BACK-COMPAT summary shape: an older server that never emits
/// `node_preview`. Clients must still decode it with the preview absent.
fn build_summary() -> MapSummary {
    MapSummary {
        map_id: "map-1".to_string(),
        title: "iOS thinking map migration".to_string(),
        lifecycle: MapLifecycle::Active,
        latest_revision: 4,
        updated_at: TS2.to_string(),
        node_preview: None,
    }
}

/// The CURRENT summary shape: the same map plus the bounded `node_preview` the
/// library-card mini-graph draws — two nodes (a root idea and a model-suggested
/// child question) joined by the one parent→child branch edge. Shares every
/// non-preview field with [`build_summary`] so the two fixtures cannot drift.
fn build_summary_with_preview() -> MapSummary {
    MapSummary {
        node_preview: Some(NodePreview {
            nodes: vec![
                NodePreviewNode {
                    node_id: PREVIEW_ROOT_ID.to_string(),
                    parent_id: None,
                    kind: NodeKind::Idea,
                    suggested: false,
                    title: "Root idea".to_string(),
                },
                NodePreviewNode {
                    node_id: PREVIEW_CHILD_ID.to_string(),
                    parent_id: Some(PREVIEW_ROOT_ID.to_string()),
                    kind: NodeKind::Question,
                    suggested: true,
                    title: "Open question".to_string(),
                },
            ],
            edges: vec![NodePreviewEdge {
                from: PREVIEW_ROOT_ID.to_string(),
                to: PREVIEW_CHILD_ID.to_string(),
            }],
        }),
        ..build_summary()
    }
}

fn build_event() -> MapEvent {
    MapEvent {
        sequence: 1,
        envelope: build_envelope(),
        resulting_revision: 4,
        semantic_hash: "sha256:deadbeefcafef00d".to_string(),
        applied_at: TS2.to_string(),
    }
}

fn build_manifest() -> MapManifest {
    MapManifest {
        schema_version: 1,
        map_id: "map-1".to_string(),
        principal: "anonymous".to_string(),
        workspace: "default".to_string(),
        title: "iOS thinking map migration".to_string(),
        source: ThinkingMapSource::Meeting {
            thread_id: "thread-42".to_string(),
        },
        lifecycle: MapLifecycle::Active,
        latest_revision: 4,
        latest_sequence: 1,
        latest_semantic_hash: "sha256:deadbeefcafef00d".to_string(),
        created_at: TS.to_string(),
        updated_at: TS2.to_string(),
        branched_from_map_id: None,
        branched_from_sequence: None,
    }
}

/// `{"outcome":"applied", ...}` — exactly as `apply_operations_handler` /
/// `patch_map_handler` / `interpret_handler` emit it.
fn response_applied() -> Value {
    json!({
        "outcome": "applied",
        "resulting_revision": 5,
        "semantic_hash": "sha256:deadbeefcafef00d",
        "map": serde_json::to_value(build_map()).expect("map to value"),
    })
}

/// `{"outcome":"idempotent_replay","resulting_revision":N}`.
fn response_idempotent() -> Value {
    json!({
        "outcome": "idempotent_replay",
        "resulting_revision": 4,
    })
}

/// `{"outcome":"no_operations"}` — the interpret handler's zero-move response.
fn response_no_operations() -> Value {
    json!({ "outcome": "no_operations" })
}

fn repo_root() -> PathBuf {
    // examples run with CWD = the workspace root (where `cargo run` is invoked).
    // We target the repo-relative path so the emitter is location-independent.
    let manifest_dir = env!("CARGO_MANIFEST_DIR"); // .../magician/magician
    Path::new(manifest_dir)
        .parent() // .../magician (repo root)
        .expect("repo root")
        .to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    repo_root().join("magios/Magios/ThinkingMapCanonical/Fixtures")
}

/// Where the generated Swift fixture constants land (compiled into MagiosTests).
fn swift_fixtures_path() -> PathBuf {
    repo_root().join("magios/MagiosTests/LTMWireFixtures.swift")
}

fn write_json<T: Serialize>(dir: &Path, name: &str, value: &T) -> String {
    let path = dir.join(format!("{name}.json"));
    let mut body = serde_json::to_string_pretty(value).expect("serialize fixture");
    body.push('\n');
    std::fs::write(&path, &body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    println!("wrote {}", path.display());
    body
}

/// Emit `magios/MagiosTests/LTMWireFixtures.swift` — a `@generated` Swift enum of
/// raw-string constants, one per fixture, so the XCTests decode from an in-target
/// constant (no bundle-resource loading). Each constant is the EXACT bytes of the
/// on-disk `.json` fixture. Uses a single-hash raw literal (`#"..."#`); the
/// emitter asserts no fixture contains the closing delimiter `"#` so this is safe.
fn write_swift_fixtures(fixtures: &[(&str, &str, String)]) {
    let path = swift_fixtures_path();
    let mut out = String::new();
    out.push_str(
        "// @generated by magician/examples/thinking_map_wire_fixtures.rs — do not edit.\n\
         //\n\
         // Raw JSON constants for the LTM wire fixtures, one per `Fixtures/<name>.json`.\n\
         // The XCTests decode these directly (no bundle-resource loading). Regenerate via:\n\
         //   CARGO_TARGET_DIR=/Volumes/build/magician/builds \\\n\
         //     cargo run -p magician --example thinking_map_wire_fixtures\n\
         \n\
         enum LTMWireFixtures {\n",
    );
    for (swift_name, _fixture_name, json) in fixtures {
        assert!(
            !json.contains("\"#"),
            "fixture {swift_name} contains the raw-string delimiter `\"#`; \
             bump the emitter to `##\"…\"##`"
        );
        // Trim the trailing newline the on-disk pretty JSON carries so the Swift
        // constant is the clean document (decoders don't care, but it's tidier).
        let trimmed = json.trim_end_matches('\n');
        out.push_str(&format!(
            "    static let {swift_name} = #\"\"\"\n{trimmed}\n\"\"\"#\n\n"
        ));
    }
    out.push_str("}\n");
    std::fs::write(&path, out).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    println!("wrote {}", path.display());
}

fn main() {
    let dir = fixtures_dir();
    std::fs::create_dir_all(&dir).expect("create fixtures dir");

    // (swift constant name, fixture file stem, JSON body) — order = declaration order.
    let fixtures = vec![
        ("mapJSON", "map", write_json(&dir, "map", &build_map())),
        (
            "envelopeJSON",
            "envelope",
            write_json(&dir, "envelope", &build_envelope()),
        ),
        (
            "summaryJSON",
            "summary",
            write_json(&dir, "summary", &build_summary()),
        ),
        (
            "summaryWithPreviewJSON",
            "summary_with_preview",
            write_json(&dir, "summary_with_preview", &build_summary_with_preview()),
        ),
        (
            "eventJSON",
            "event",
            write_json(&dir, "event", &build_event()),
        ),
        (
            "manifestJSON",
            "manifest",
            write_json(&dir, "manifest", &build_manifest()),
        ),
        (
            "responseAppliedJSON",
            "response_applied",
            write_json(&dir, "response_applied", &response_applied()),
        ),
        (
            "responseIdempotentJSON",
            "response_idempotent",
            write_json(&dir, "response_idempotent", &response_idempotent()),
        ),
        (
            "responseNoOperationsJSON",
            "response_no_operations",
            write_json(&dir, "response_no_operations", &response_no_operations()),
        ),
    ];

    write_swift_fixtures(&fixtures);

    println!(
        "\nAll thinking-map wire fixtures written to {}",
        dir.display()
    );
}
