//! Boundary B: the working-set evaluation suite.
//!
//! Deterministic fixtures with planted ground truth, a comparison harness
//! measuring bounded working-set retrieval against ordinary context packing,
//! and the adversarial cases the plan names. Everything here runs without a
//! network or a model on purpose: what this suite can prove deterministically
//! is *evidence grounding* — whether a planted fact is reachable, exactly
//! attributed, and cheap to ship — which is the plan's claimed value
//! ("coverage of searchable evidence, not a claim of 100x answer
//! correctness"). LLM-judged answer quality is the live-evaluation lane and
//! deliberately not simulated here.
//!
//! The activation rule these results feed lives in
//! `content_sources::WorkingSetActivationSettings`; its `enabled_lanes`
//! default stays empty until this suite's evidence opens a lane.

use super::working_sets::{CreateWorkingSetRequest, WorkingSetSourceInput, WorkingSetStore};
use crate::magician_v2::content_sources::{
    ContentDocument, ContentPrivacy, ContentProvenance, SourceIdentity,
    CONTENT_SOURCE_SCHEMA_VERSION,
};

/// The context budget the packing baseline is measured at. Roughly a
/// 32k-token window: generous for chat, and still far below the corpus sizes
/// research lanes carry.
pub const CONTEXT_PACK_BUDGET_BYTES: usize = 128 * 1024;

/// One planted fact the harness must recover, with its exact home.
#[derive(Debug, Clone)]
pub struct GroundTruthProbe {
    /// What a researcher would be trying to establish. Recorded so eval
    /// output reads as an investigation, not a token hunt.
    pub question: &'static str,
    /// The bounded search query the working-set lane issues.
    pub query: &'static str,
    /// The planted literal that answers the question — the whole line as it
    /// sits in the corpus. The deterministic lane greps a chunk for it.
    pub fact: String,
    /// The value a correct *answer* must state. A model asked for one or two
    /// sentences does not reproduce the planted line verbatim, so grading an
    /// answer against `fact` fails answers that are right; this is the part
    /// that cannot be right by accident.
    pub answer_key: &'static str,
    /// The source that truthfully contains the fact.
    pub source_id: &'static str,
    /// Whether the fact sits beyond [`CONTEXT_PACK_BUDGET_BYTES`] in the
    /// packed corpus — the probes that demonstrate the boundary's value.
    pub beyond_pack_budget: bool,
}

/// A deterministic corpus plus the ground truth planted in it.
pub struct EvalFixture {
    pub name: &'static str,
    pub request: CreateWorkingSetRequest,
    pub probes: Vec<GroundTruthProbe>,
}

/// What one retrieval lane achieved for one probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneMetrics {
    pub fact_found: bool,
    /// True only when the lane can name the exact source the fact came from.
    /// Context packing ships anonymous text, so it is always false there.
    pub attributed: bool,
    pub bytes_shipped: u64,
}

#[derive(Debug)]
pub struct ProbeComparison {
    pub fixture: &'static str,
    pub question: &'static str,
    pub beyond_pack_budget: bool,
    pub context_pack: LaneMetrics,
    pub working_set: LaneMetrics,
}

pub fn public_document(item_id: &str, title: &str, text: String) -> ContentDocument {
    let content_hash = blake3::hash(text.as_bytes()).to_hex().to_string();
    ContentDocument {
        schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
        identity: SourceIdentity::new("eval", item_id).expect("eval source identity"),
        title: title.to_string(),
        text,
        canonical_url: Some(format!("https://eval.example/{item_id}")),
        media_type: Some("text/plain".to_string()),
        fetched_at_ms: 1,
        privacy: ContentPrivacy::Public,
        content_hash,
        provenance: ContentProvenance {
            source_label: "eval fixture".to_string(),
            source_url: Some(format!("https://eval.example/{item_id}")),
            retrieved_by: "working-set-eval".to_string(),
        },
        metadata: Default::default(),
    }
}

/// Filler that cannot collide with any planted fact or query: a numbered
/// prose line, repeated with a changing counter so Bloom filters and exact
/// search both see realistic, non-degenerate text.
fn filler(lines: usize, topic: &str) -> String {
    let mut text = String::with_capacity(lines * 64);
    for index in 0..lines {
        text.push_str(&format!(
            "paragraph {index:06} continues the {topic} background discussion without conclusions.\n"
        ));
    }
    text
}

/// A single long document — the report nobody can pack into context. Facts
/// are planted near the head, past the middle, and deep in the tail (beyond
/// the packing budget), which is the shape of every "the answer was in
/// section 47" research failure.
pub fn long_document_fixture() -> EvalFixture {
    let head_fact = "The migration freeze begins on 2026-09-02.".to_string();
    let middle_fact = "Vendor Northwind quoted 14 microcents per call.".to_string();
    let tail_fact = "The rollback rehearsal exposed a 41-minute replication lag.".to_string();

    let mut text = String::new();
    text.push_str(&filler(200, "governance"));
    text.push_str(&head_fact);
    text.push('\n');
    text.push_str(&filler(2_600, "architecture"));
    text.push_str(&middle_fact);
    text.push('\n');
    text.push_str(&filler(12_000, "operations"));
    text.push_str(&tail_fact);
    text.push('\n');
    text.push_str(&filler(400, "appendix"));

    let head_beyond = false;
    let middle_beyond =
        text.find(&middle_fact).expect("middle fact planted") > CONTEXT_PACK_BUDGET_BYTES;
    let tail_beyond = text.find(&tail_fact).expect("tail fact planted") > CONTEXT_PACK_BUDGET_BYTES;

    EvalFixture {
        name: "long-document",
        request: CreateWorkingSetRequest {
            working_set_id: "eval-long-document".to_string(),
            title: "Quarterly infrastructure report".to_string(),
            created_by: "working-set-eval".to_string(),
            sources: vec![WorkingSetSourceInput {
                source_id: "report".to_string(),
                document: public_document("report", "Quarterly infrastructure report", text),
            }],
        },
        probes: vec![
            GroundTruthProbe {
                question: "When does the migration freeze begin?",
                query: "migration freeze begins",
                fact: head_fact,
                answer_key: "2026-09-02",
                source_id: "report",
                beyond_pack_budget: head_beyond,
            },
            GroundTruthProbe {
                question: "What did Northwind quote per call?",
                query: "northwind quoted",
                fact: middle_fact,
                answer_key: "14 microcents",
                source_id: "report",
                beyond_pack_budget: middle_beyond,
            },
            GroundTruthProbe {
                question: "What did the rollback rehearsal expose?",
                query: "rollback rehearsal exposed",
                fact: tail_fact,
                answer_key: "41-minute replication lag",
                source_id: "report",
                beyond_pack_budget: tail_beyond,
            },
        ],
    }
}

/// A multi-file repository slice: a symbol defined in one file, used in
/// another, with the definition file sitting late in packing order so a
/// truncated pack loses it.
pub fn repository_fixture() -> EvalFixture {
    let definition_fact =
        "fn resolve_capability_grant(scope: &ScopeRef) -> GrantOutcome".to_string();
    let usage_fact =
        "let outcome = resolve_capability_grant(&request.scope); // audit trail".to_string();

    let mut sources = Vec::new();
    for module in ["ingest", "planner", "router", "telemetry"] {
        sources.push(WorkingSetSourceInput {
            source_id: format!("src-{module}"),
            document: public_document(
                &format!("src-{module}"),
                &format!("src/{module}.rs"),
                filler(9_000, module) + &format!("mod {module}_support {{}}\n"),
            ),
        });
    }
    sources.push(WorkingSetSourceInput {
        source_id: "src-dispatch".to_string(),
        document: public_document(
            "src-dispatch",
            "src/dispatch.rs",
            filler(300, "dispatch") + &usage_fact + "\n",
        ),
    });
    sources.push(WorkingSetSourceInput {
        source_id: "src-grants".to_string(),
        document: public_document(
            "src-grants",
            "src/grants.rs",
            filler(300, "grants") + &definition_fact + " {\n    GrantOutcome::Denied\n}\n",
        ),
    });

    EvalFixture {
        name: "repository",
        request: CreateWorkingSetRequest {
            working_set_id: "eval-repository".to_string(),
            title: "Capability grant investigation".to_string(),
            created_by: "working-set-eval".to_string(),
            sources,
        },
        probes: vec![
            GroundTruthProbe {
                question: "Where is resolve_capability_grant defined?",
                query: "fn resolve_capability_grant",
                fact: definition_fact,
                // "Where" is a file; the source is titled `src/grants.rs`.
                answer_key: "grants.rs",
                source_id: "src-grants",
                beyond_pack_budget: true,
            },
            GroundTruthProbe {
                // The corpus plants a call line in a file, so the file is the
                // answer. Asked "who calls", a model shown the line with no
                // enclosing function said NOT IN CONTEXT — twice, from both
                // lanes — which is a careful answer to a question the corpus
                // does not pose.
                question: "Which file calls resolve_capability_grant?",
                query: "resolve_capability_grant(&request.scope)",
                fact: usage_fact,
                answer_key: "dispatch",
                source_id: "src-dispatch",
                beyond_pack_budget: true,
            },
        ],
    }
}

/// A log investigation: one incident chain buried deep in a day of noise.
pub fn log_investigation_fixture() -> EvalFixture {
    let incident_fact =
        "2026-08-11T03:41:07Z ERROR lease-keeper: permit q-1183 expired mid-dispatch".to_string();

    let mut text = String::new();
    for index in 0..18_000_usize {
        let minute = index % 60;
        text.push_str(&format!(
            "2026-08-11T{:02}:{minute:02}:00Z INFO worker-{}: heartbeat cycle {index} settled clean\n",
            index / 3_600,
            index % 7,
        ));
        if index == 13_250 {
            text.push_str(&incident_fact);
            text.push('\n');
            text.push_str("2026-08-11T03:41:09Z WARN dispatcher: retrying without a permit\n");
        }
    }

    let beyond = text.find(&incident_fact).expect("incident planted") > CONTEXT_PACK_BUDGET_BYTES;
    EvalFixture {
        name: "log-investigation",
        request: CreateWorkingSetRequest {
            working_set_id: "eval-log-investigation".to_string(),
            title: "Lease keeper incident".to_string(),
            created_by: "working-set-eval".to_string(),
            sources: vec![WorkingSetSourceInput {
                source_id: "worker-log".to_string(),
                document: public_document("worker-log", "worker.log", text),
            }],
        },
        probes: vec![GroundTruthProbe {
            question: "What happened to permit q-1183?",
            query: "permit q-1183",
            fact: incident_fact,
            answer_key: "expired mid-dispatch",
            source_id: "worker-log",
            beyond_pack_budget: beyond,
        }],
    }
}

/// Two sources that disagree about the same fact. A trustworthy research
/// path must surface BOTH, each with its own citation, rather than silently
/// picking one — this fixture's probes are one per side.
pub fn conflicting_sources_fixture() -> EvalFixture {
    let published_fact = "The public API rate limit is 500 requests per minute.".to_string();
    let internal_fact =
        "The public API rate limit was reduced to 50 requests per minute.".to_string();

    EvalFixture {
        name: "conflicting-sources",
        request: CreateWorkingSetRequest {
            working_set_id: "eval-conflicting-sources".to_string(),
            title: "Rate limit discrepancy".to_string(),
            created_by: "working-set-eval".to_string(),
            sources: vec![
                WorkingSetSourceInput {
                    source_id: "public-docs".to_string(),
                    document: public_document(
                        "public-docs",
                        "developer.example.com/limits",
                        filler(120, "documentation") + &published_fact + "\n",
                    ),
                },
                WorkingSetSourceInput {
                    source_id: "changelog".to_string(),
                    document: public_document(
                        "changelog",
                        "developer.example.com/changelog",
                        filler(120, "changelog") + &internal_fact + "\n",
                    ),
                },
            ],
        },
        probes: vec![
            GroundTruthProbe {
                question: "What rate limit do the docs state?",
                query: "rate limit is 500",
                fact: published_fact,
                answer_key: "500 requests per minute",
                source_id: "public-docs",
                beyond_pack_budget: false,
            },
            GroundTruthProbe {
                question: "What does the changelog say the limit became?",
                query: "reduced to 50 requests",
                fact: internal_fact,
                answer_key: "50 requests per minute",
                source_id: "changelog",
                beyond_pack_budget: false,
            },
        ],
    }
}

pub fn all_fixtures() -> Vec<EvalFixture> {
    vec![
        long_document_fixture(),
        repository_fixture(),
        log_investigation_fixture(),
        conflicting_sources_fixture(),
    ]
}

/// The packing baseline: concatenate sources in request order and truncate
/// at the budget — exactly what "just put it all in the prompt" does. The
/// fact either survives truncation or silently vanishes, and nothing in the
/// shipped text says where it came from.
pub fn context_pack_lane(request: &CreateWorkingSetRequest, fact: &str) -> LaneMetrics {
    let mut packed = String::new();
    for source in &request.sources {
        packed.push_str(&source.document.text);
        packed.push('\n');
        if packed.len() >= CONTEXT_PACK_BUDGET_BYTES {
            break;
        }
    }
    let mut budget_end = CONTEXT_PACK_BUDGET_BYTES.min(packed.len());
    while budget_end > 0 && !packed.is_char_boundary(budget_end) {
        budget_end -= 1;
    }
    let shipped = &packed[..budget_end];
    LaneMetrics {
        fact_found: shipped.contains(fact),
        attributed: false,
        bytes_shipped: shipped.len() as u64,
    }
}

/// The working-set lane, following the `research-working-sets` procedure:
/// one bounded search, then one bounded read of the top citation. Bytes
/// shipped are what actually reaches a model — excerpts plus the cited
/// chunk — not the corpus.
pub async fn working_set_lane(
    store: &WorkingSetStore,
    principal: &str,
    workspace: &str,
    working_set_id: &str,
    probe: &GroundTruthProbe,
) -> LaneMetrics {
    let matches = match store
        .search(principal, workspace, working_set_id, probe.query, 5)
        .await
    {
        Ok(matches) => matches,
        Err(_) => {
            return LaneMetrics {
                fact_found: false,
                attributed: false,
                bytes_shipped: 0,
            }
        },
    };
    let mut bytes_shipped: u64 = matches
        .iter()
        .map(|search_match| search_match.excerpt.len() as u64)
        .sum();
    let Some(top) = matches.first() else {
        return LaneMetrics {
            fact_found: false,
            attributed: false,
            bytes_shipped,
        };
    };
    let attributed = top.source_id == probe.source_id;
    let chunk = store
        .read_chunk(
            principal,
            workspace,
            working_set_id,
            &top.source_id,
            top.chunk_index,
        )
        .await;
    let fact_found = match chunk {
        Ok(chunk) => {
            bytes_shipped += chunk.text.len() as u64;
            chunk.text.contains(&probe.fact)
        },
        Err(_) => false,
    };
    LaneMetrics {
        fact_found,
        attributed,
        bytes_shipped,
    }
}

pub async fn evaluate_fixture(
    store: &WorkingSetStore,
    principal: &str,
    workspace: &str,
    fixture: &EvalFixture,
) -> Vec<ProbeComparison> {
    let mut comparisons = Vec::with_capacity(fixture.probes.len());
    for probe in &fixture.probes {
        comparisons.push(ProbeComparison {
            fixture: fixture.name,
            question: probe.question,
            beyond_pack_budget: probe.beyond_pack_budget,
            context_pack: context_pack_lane(&fixture.request, &probe.fact),
            working_set: working_set_lane(
                store,
                principal,
                workspace,
                &fixture.request.working_set_id,
                probe,
            )
            .await,
        });
    }
    comparisons
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::{
        artifact_v2::{workspace::ArtifactV2Workspace, WorkingSetManifest},
        content_sources::{
            WorkingSetActivationDecision, WorkingSetActivationProbe, WorkingSetActivationSettings,
        },
    };

    const MAX_EXCERPT_CHARS: usize = 480; // mirrors the store's excerpt bound

    fn scoped_store() -> (WorkingSetStore, ArtifactV2Workspace, std::path::PathBuf) {
        // Nextest launches these cases in parallel processes. A timestamp-only
        // name can collide at the host clock's effective resolution, allowing
        // one test's cleanup to remove another test's active workspace.
        let root = tempfile::Builder::new()
            .prefix("magician-working-set-eval-")
            .tempdir()
            .expect("create isolated working-set eval root")
            .keep();
        let workspace = ArtifactV2Workspace::new(&root);
        (WorkingSetStore::new(workspace.clone()), workspace, root)
    }

    /// Boundary B deliverable 2 and its decision-gate evidence, in one place.
    ///
    /// The working-set lane must recover and exactly attribute every planted
    /// fact; the packing baseline must demonstrably lose every fact planted
    /// beyond its budget; and the working-set lane must ship a small fraction
    /// of the baseline's bytes. If any of these stops holding, the routing
    /// gate's premise is gone and `enabled_lanes` should stay closed.
    #[tokio::test]
    async fn working_set_lane_beats_context_packing_on_grounding_and_cost() {
        let (store, _workspace, root) = scoped_store();
        let mut deep_probes = 0_usize;
        for fixture in all_fixtures() {
            let corpus_bytes: u64 = fixture
                .request
                .sources
                .iter()
                .map(|source| source.document.text.len() as u64)
                .sum();
            store
                .create("eval-owner", "eval-workspace", fixture.request.clone())
                .await
                .expect("create eval fixture");
            let comparisons =
                evaluate_fixture(&store, "eval-owner", "eval-workspace", &fixture).await;
            for comparison in &comparisons {
                eprintln!(
                    "[{}] {} | corpus {}B | pack: found={} {}B | working-set: found={} attributed={} {}B",
                    comparison.fixture,
                    comparison.question,
                    corpus_bytes,
                    comparison.context_pack.fact_found,
                    comparison.context_pack.bytes_shipped,
                    comparison.working_set.fact_found,
                    comparison.working_set.attributed,
                    comparison.working_set.bytes_shipped,
                );
                assert!(
                    comparison.working_set.fact_found,
                    "working-set lane lost `{}` in {}",
                    comparison.question, comparison.fixture
                );
                assert!(
                    comparison.working_set.attributed,
                    "working-set lane misattributed `{}` in {}",
                    comparison.question, comparison.fixture
                );
                // The lane's answer always fits well inside a quarter of the
                // packing budget: bounded excerpts plus one cited chunk.
                assert!(
                    comparison.working_set.bytes_shipped <= (CONTEXT_PACK_BUDGET_BYTES as u64) / 4,
                    "working-set lane shipped {}B for `{}` — no longer bounded",
                    comparison.working_set.bytes_shipped,
                    comparison.question,
                );
                if comparison.beyond_pack_budget {
                    deep_probes += 1;
                    assert!(
                        !comparison.context_pack.fact_found,
                        "packing was expected to lose `{}` in {} — the fixture no longer \
                         demonstrates the boundary's value",
                        comparison.question, comparison.fixture
                    );
                    // For the probes packing cannot serve, the honest
                    // comparison is against shipping the corpus that WOULD
                    // contain the fact: the lane must cost at most a tenth.
                    assert!(
                        comparison.working_set.bytes_shipped * 10 <= corpus_bytes,
                        "working-set lane shipped {}B against a {}B corpus for `{}` — \
                         the cost advantage is gone",
                        comparison.working_set.bytes_shipped,
                        corpus_bytes,
                        comparison.question,
                    );
                }
            }
        }
        assert!(
            deep_probes >= 4,
            "the suite must keep enough beyond-budget probes to demonstrate value"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Conflicting sources must BOTH be reachable with distinct citations —
    /// surfacing one side of a contradiction as truth is worse than failing.
    #[tokio::test]
    async fn conflicting_sources_surface_both_sides_with_distinct_citations() {
        let (store, _workspace, root) = scoped_store();
        let fixture = conflicting_sources_fixture();
        store
            .create("eval-owner", "eval-workspace", fixture.request.clone())
            .await
            .expect("create conflict fixture");
        let mut cited_sources = std::collections::HashSet::new();
        for probe in &fixture.probes {
            let lane = working_set_lane(
                &store,
                "eval-owner",
                "eval-workspace",
                &fixture.request.working_set_id,
                probe,
            )
            .await;
            assert!(lane.fact_found && lane.attributed);
            cited_sources.insert(probe.source_id);
        }
        assert_eq!(
            cited_sources.len(),
            2,
            "both sides of the conflict must cite"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Boundary B deliverable 3: the activation rule and its closed gate.
    /// The rule measures scale — evidence the ordinary window cannot show —
    /// and nothing else. The day the lane was opened showed why: source
    /// count and read depth put ordinary research on the path, which paid
    /// four model calls per task to search a set that held nothing the
    /// window had not already shown.
    #[test]
    fn activation_requires_an_enabled_lane_and_evidence_beyond_the_window() {
        let closed = WorkingSetActivationSettings::default();
        assert!(matches!(
            closed.decide(&WorkingSetActivationProbe {
                lane: "web-research",
                total_source_bytes: 10 * 1024 * 1024,
                beyond_window_bytes: 10 * 1024 * 1024,
            }),
            WorkingSetActivationDecision::Stay { .. }
        ));

        let open = WorkingSetActivationSettings {
            enabled_lanes: vec!["web-research".to_string()],
            ..WorkingSetActivationSettings::default()
        };
        // Enough hidden past the window: the set holds what the model could
        // not see, and searching it pays.
        let beyond = open.decide(&WorkingSetActivationProbe {
            lane: "web-research",
            total_source_bytes: open.min_beyond_window_bytes + 6_000,
            beyond_window_bytes: open.min_beyond_window_bytes,
        });
        match &beyond {
            WorkingSetActivationDecision::Activate { reason } => {
                assert!(reason.contains("beyond the window"), "{reason}");
            },
            other => panic!("expected activation on bytes beyond the window, got {other:?}"),
        }
        // Enough in total, even shown whole page by page: context packing
        // will not keep it all, so the durable set pays.
        assert!(matches!(
            open.decide(&WorkingSetActivationProbe {
                lane: "web-research",
                total_source_bytes: open.min_total_source_bytes,
                beyond_window_bytes: 0,
            }),
            WorkingSetActivationDecision::Activate { .. }
        ));
        // Ordinary research: several pages over several rounds, each shown
        // whole or nearly so. Below both thresholds it stays on the ordinary
        // path however many sources and rounds it took, and the sentence
        // says what it measured.
        let stay = open.decide(&WorkingSetActivationProbe {
            lane: "web-research",
            total_source_bytes: 30 * 1024,
            beyond_window_bytes: 11 * 1024,
        });
        match &stay {
            WorkingSetActivationDecision::Stay { reason } => {
                assert!(reason.contains("beyond the window"), "{reason}");
                assert!(
                    !reason.contains("sources") && !reason.contains("depth"),
                    "{reason}"
                );
            },
            other => panic!("ordinary research must stay, got {other:?}"),
        }

        // Zero thresholds would activate everything; validation refuses them.
        for degenerate in [
            WorkingSetActivationSettings {
                min_total_source_bytes: 0,
                ..WorkingSetActivationSettings::default()
            },
            WorkingSetActivationSettings {
                min_beyond_window_bytes: 0,
                ..WorkingSetActivationSettings::default()
            },
        ] {
            assert!(degenerate.validate_bounds().is_err());
        }
    }

    // ─── Boundary B deliverable 4: adversarial cases ───

    #[tokio::test]
    async fn cross_scope_working_set_ids_are_refused() {
        let (store, _workspace, root) = scoped_store();
        let fixture = conflicting_sources_fixture();
        store
            .create("owner-a", "default", fixture.request.clone())
            .await
            .expect("create in scope A");

        // A different principal, and separately a different workspace,
        // presenting the same working-set id: both must fail closed.
        for (principal, workspace) in [("owner-b", "default"), ("owner-a", "elsewhere")] {
            assert!(store
                .search(
                    principal,
                    workspace,
                    &fixture.request.working_set_id,
                    "rate limit",
                    5
                )
                .await
                .is_err());
            assert!(store
                .read_chunk(
                    principal,
                    workspace,
                    &fixture.request.working_set_id,
                    "public-docs",
                    0
                )
                .await
                .is_err());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn forged_source_metadata_fails_integrity_not_silence() {
        let (store, workspace, root) = scoped_store();
        let fixture = conflicting_sources_fixture();
        store
            .create("eval-owner", "eval-workspace", fixture.request.clone())
            .await
            .expect("create fixture");

        let set_root = workspace
            .scope_root("eval-owner", "eval-workspace")
            .join("research")
            .join("working_sets")
            .join(&fixture.request.working_set_id);

        // Same-length tamper of the chunk bytes on disk: the recorded hash no
        // longer matches, and the read must refuse rather than return the
        // forged text.
        let chunk_path = set_root
            .join("sources")
            .join("public-docs")
            .join("00000.txt");
        let mut bytes = std::fs::read(&chunk_path).expect("read chunk bytes");
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'x' { b'y' } else { b'x' };
        std::fs::write(&chunk_path, &bytes).expect("write forged chunk");
        let error = store
            .read_chunk(
                "eval-owner",
                "eval-workspace",
                &fixture.request.working_set_id,
                "public-docs",
                0,
            )
            .await
            .expect_err("forged chunk must fail");
        assert!(error.to_string().contains("integrity"));

        // A manifest rewritten to claim another scope is quarantined by the
        // next create and refused by reads.
        let manifest_path = set_root.join("manifest.json");
        let mut manifest: WorkingSetManifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
                .expect("decode manifest");
        manifest.scope.principal = "someone-else".to_string();
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("encode forged manifest"),
        )
        .expect("write forged manifest");
        assert!(store
            .get(
                "eval-owner",
                "eval-workspace",
                &fixture.request.working_set_id
            )
            .await
            .is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn derived_outputs_stay_bounded_even_for_deep_matches() {
        let (store, _workspace, root) = scoped_store();
        let fixture = log_investigation_fixture();
        store
            .create("eval-owner", "eval-workspace", fixture.request.clone())
            .await
            .expect("create log fixture");
        let matches = store
            .search(
                "eval-owner",
                "eval-workspace",
                &fixture.request.working_set_id,
                "permit q-1183",
                20,
            )
            .await
            .expect("search log fixture");
        assert!(!matches.is_empty());
        for search_match in &matches {
            assert!(
                search_match.excerpt.chars().count() <= MAX_EXCERPT_CHARS,
                "excerpt exceeded its bound"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Cancellation mid-derivation: a staging directory that never published
    /// must be reclaimed once stale, not accumulate forever or block later
    /// captures.
    #[tokio::test]
    async fn abandoned_staging_is_reclaimed_by_the_next_create() {
        let (store, workspace, root) = scoped_store();
        let working_sets_root = workspace
            .scope_root("eval-owner", "eval-workspace")
            .join("research")
            .join("working_sets");
        let abandoned = working_sets_root.join(".cancelled-run-deadbeef.staging");
        std::fs::create_dir_all(&abandoned).expect("create abandoned staging");
        std::fs::write(abandoned.join("manifest.json"), b"{").expect("partial write");
        // Age the directory past the five-minute reclaim window.
        let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        let directory = std::fs::File::open(&abandoned).expect("open staging dir");
        directory
            .set_modified(stale)
            .expect("age staging directory");

        let fixture = conflicting_sources_fixture();
        store
            .create("eval-owner", "eval-workspace", fixture.request.clone())
            .await
            .expect("create after abandoned staging");
        assert!(
            !abandoned.exists(),
            "stale staging survived the next capture"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Stale child results: a delegated worker holding citations to a
    /// working set that has since expired must get a typed, actionable
    /// refusal — never silently empty evidence.
    #[tokio::test]
    async fn citations_into_an_expired_working_set_fail_typed() {
        let (store, workspace, root) = scoped_store();
        let fixture = conflicting_sources_fixture();
        store
            .create("eval-owner", "eval-workspace", fixture.request.clone())
            .await
            .expect("create fixture");

        // The child kept its citation; meanwhile the set aged past retention.
        let manifest_path = workspace
            .scope_root("eval-owner", "eval-workspace")
            .join("research")
            .join("working_sets")
            .join(&fixture.request.working_set_id)
            .join("manifest.json");
        let mut manifest: WorkingSetManifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
                .expect("decode manifest");
        manifest.created_at = chrono::Utc::now() - chrono::Duration::days(8);
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("encode aged manifest"),
        )
        .expect("write aged manifest");

        let error = store
            .read_chunk(
                "eval-owner",
                "eval-workspace",
                &fixture.request.working_set_id,
                "public-docs",
                0,
            )
            .await
            .expect_err("expired citation must fail");
        assert!(error.to_string().contains("expired"));

        // The expiry read also removed the set; the citation now reports a
        // plain not-found rather than resurrecting.
        let error = store
            .search(
                "eval-owner",
                "eval-workspace",
                &fixture.request.working_set_id,
                "rate limit",
                5,
            )
            .await
            .expect_err("second use must be not-found");
        assert!(error.to_string().contains("not found"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Boundary B through the seam it will actually run on. Each fixture's
    /// sources arrive as sequential reads under one execution, the way a task
    /// gathers them; the rule opens the path at the documented threshold and
    /// not before; after that every read the model sees is bounded; and every
    /// planted fact stays reachable through the execution-scoped search with
    /// exact attribution. The suite above proves the lane in isolation — this
    /// proves the router that puts a task on it.
    #[tokio::test]
    async fn sequential_reads_activate_the_path_bound_the_projection_and_keep_every_fact_reachable()
    {
        use crate::magician_v2::execution::compiled_handlers::working_sets::{
            content_read_capture_request, materialize_content_read_result,
            ACTIVATED_PAGE_EXCERPT_CHARS,
        };
        use serde_json::json;

        let (store, workspace, root) = scoped_store();
        let policy = crate::magician_v2::content_sources::WorkingSetCaptureSettings {
            activation: WorkingSetActivationSettings {
                enabled_lanes: vec!["web-research".to_string()],
                ..WorkingSetActivationSettings::default()
            },
            ..Default::default()
        };
        let settings = &policy.activation;
        let ordinary_excerpt_chars = 6_000usize;

        for fixture in all_fixtures() {
            let execution_id = format!("exec-{}", fixture.name);
            let mut cumulative_bytes = 0u64;
            let mut beyond_window_bytes = 0u64;
            let mut activated_at_read: Option<usize> = None;

            for (round, source) in fixture.request.sources.iter().enumerate() {
                let capture = content_read_capture_request(
                    &json!({
                        "__principal": "eval-owner",
                        "__workspace": "eval-workspace",
                        "__agent_id": "web-researcher",
                        "__execution_id": execution_id,
                    }),
                    &policy,
                )
                .expect("capture request")
                .expect("web-researcher captures automatically");
                let text = &source.document.text;
                let mut result = json!({
                    "status": "complete",
                    "claim_eligible": true,
                    "document": source.document,
                    "excerpt": text.chars().take(ordinary_excerpt_chars).collect::<String>(),
                    "excerpt_complete": text.chars().count() <= ordinary_excerpt_chars,
                });
                materialize_content_read_result(
                    workspace.clone(),
                    Some(capture),
                    settings,
                    &mut result,
                )
                .await;
                assert_eq!(
                    result["working_set"]["status"],
                    "created",
                    "[{}] read {} did not capture",
                    fixture.name,
                    round + 1
                );

                cumulative_bytes += text.len() as u64;
                beyond_window_bytes +=
                    (text.len() as u64).saturating_sub(ordinary_excerpt_chars as u64);
                // The documented rule, restated independently of `decide`:
                // scale, measured as what the window cannot show, or the
                // total context packing will not keep.
                let expected = activated_at_read.is_some()
                    || cumulative_bytes >= settings.min_total_source_bytes
                    || beyond_window_bytes >= settings.min_beyond_window_bytes;
                let routing = &result["working_set"]["routing"];
                assert_eq!(
                    routing["activated"].as_bool(),
                    Some(expected),
                    "[{}] read {}: {} bytes, {} beyond the window — reason: {}",
                    fixture.name,
                    round + 1,
                    cumulative_bytes,
                    beyond_window_bytes,
                    routing["reason"]
                );
                if expected {
                    if activated_at_read.is_none() {
                        activated_at_read = Some(round + 1);
                        assert_eq!(routing["sticky"], false);
                    } else {
                        assert_eq!(
                            routing["sticky"], true,
                            "[{}] a later read stays on the path",
                            fixture.name
                        );
                    }
                    let shown = result["excerpt"].as_str().expect("excerpt").chars().count();
                    let page_chars = text.chars().count();
                    // Non-lossy: only a page larger than the ordinary window
                    // is narrowed; the model never sees less than it would
                    // have without routing.
                    if page_chars > ordinary_excerpt_chars {
                        assert!(
                            shown <= ACTIVATED_PAGE_EXCERPT_CHARS + 1,
                            "[{}] read {} showed {} chars of a {}-char page on the working-set path",
                            fixture.name, round + 1, shown, page_chars
                        );
                        assert_eq!(result["excerpt_bounded_by_routing"], true);
                    } else {
                        // Nothing more to find in the working set than the
                        // excerpt already shows, so it is shown whole.
                        assert_eq!(
                            shown, page_chars,
                            "[{}] a page that fits is shown whole",
                            fixture.name
                        );
                        assert!(result.get("excerpt_bounded_by_routing").is_none());
                    }
                    assert!(result["working_set"]["guidance"].is_string());
                } else {
                    let shown = result["excerpt"].as_str().expect("excerpt").chars().count();
                    assert_eq!(
                        shown,
                        text.chars().count().min(ordinary_excerpt_chars),
                        "[{}] below the gate the ordinary window stays",
                        fixture.name
                    );
                }
            }

            // Scale decides. A fixture with a fact past the pack budget has
            // pages far past the window and crosses the threshold; the
            // two-page rate-limit comparison, whose facts both sit inside
            // the window, is ordinary research and stays — that is the rule
            // working, not a gap. Its facts are still reachable below.
            let crossed = beyond_window_bytes >= settings.min_beyond_window_bytes
                || cumulative_bytes >= settings.min_total_source_bytes;
            assert_eq!(
                activated_at_read.is_some(),
                crossed,
                "[{}] {} bytes beyond the window, {} in total: activation must follow scale",
                fixture.name,
                beyond_window_bytes,
                cumulative_bytes
            );
            let has_hidden_fact = fixture.probes.iter().any(|probe| probe.beyond_pack_budget);
            assert_eq!(
                crossed, has_hidden_fact,
                "[{}] the fixtures with a fact packing cannot reach are exactly the ones the rule routes",
                fixture.name
            );

            // Every planted fact is reachable through the execution, and the
            // top match names the source it was planted in.
            for probe in &fixture.probes {
                let found = store
                    .search_execution(
                        "eval-owner",
                        "eval-workspace",
                        &execution_id,
                        probe.query,
                        5,
                    )
                    .await
                    .expect("execution search");
                assert_eq!(found.members_evicted, 0);
                let top = found.matches.first().unwrap_or_else(|| {
                    panic!(
                        "[{}] `{}` is unreachable through the execution",
                        fixture.name, probe.question
                    )
                });
                let planted_title = fixture
                    .request
                    .sources
                    .iter()
                    .find(|source| source.source_id == probe.source_id)
                    .map(|source| source.document.title.as_str())
                    .expect("the probe names a fixture source");
                assert_eq!(
                    top.source_title, planted_title,
                    "[{}] `{}` attributed to the wrong source",
                    fixture.name, probe.question
                );
                let chunk = store
                    .read_chunk(
                        "eval-owner",
                        "eval-workspace",
                        &top.working_set_id,
                        &top.source_id,
                        top.chunk_index,
                    )
                    .await
                    .expect("cited chunk reads");
                assert!(
                    chunk.text.contains(&probe.fact),
                    "[{}] the cited chunk for `{}` does not hold the fact",
                    fixture.name,
                    probe.question
                );
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
