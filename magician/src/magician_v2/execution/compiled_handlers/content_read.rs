use std::{collections::BTreeMap, sync::Arc};

use chrono::Utc;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::magician_v2::{
    content_sources::{
        ContentAcquisitionResolver, ContentAcquisitionService, ContentCandidate,
        ContentInvocationSource, ContentPrivacy, ContentProvenance, EvidenceGoal, FreshnessPolicy,
        ProgressiveRetrievalSettings, ReadDepth, ReadEvidenceGoal, RemoteDataPolicy,
        RetrievalAuthority, RetrievalNeed, RetrievalOperation, RetrievalOutputKind,
        RetrievalTarget, SourceIdentity, CONTENT_SOURCE_SCHEMA_VERSION,
        RETRIEVAL_NEED_SCHEMA_VERSION,
    },
    execution::{agent_resources::AgentResources, error::ExecutionError},
};

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    execute_and_materialize(resources, args).await
}

/// Execute a controller-owned read and add a durable evidence reference when
/// the trusted capture policy selects the active agent or the caller opted in.
/// Both public retrieval facades use this boundary so their storage behavior
/// cannot drift.
pub async fn execute_and_materialize(
    resources: Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    let capture_policy = resources
        .magician_config_snapshot()
        .content_acquisition
        .working_sets
        .clone();
    let capture = super::working_sets::content_read_capture_request(&args, &capture_policy)?;
    let mut result = execute(Arc::clone(&resources), args).await?;
    super::working_sets::materialize_content_read_result(
        resources.artifact_workspace.clone(),
        capture,
        &capture_policy.activation,
        &mut result,
    )
    .await;
    Ok(result)
}

pub async fn execute(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    // The enforcement point for `browser_transports` on this path.
    //
    // An authenticated retrieval reads the page as the OWNER —
    // `RetrievalBrowserMode::AuthenticatedCdpRead` attaches to his signed-in
    // Chrome — and it never passes through the browser dispatch where the
    // ceiling is applied to a `browser` tool call. So an agent barred from the
    // owner's browser through one door reached it through this one.
    //
    // Conditional on purpose: only the identity-bearing authorities are gated.
    // A public read carries no owner identity, and refusing those would take
    // away the reading that a ceiling-restricted agent exists to do.
    //
    // The test reads the WHOLE envelope, not the top-level key: the vector form
    // carries its authority in `common` and per `requests` branch, and each
    // branch is executed straight through `execute_with_service` without passing
    // this point again. See `identity_bearing_authority_requested`.
    //
    // `execute_with_runtime` is deliberately NOT gated: it is the CLI/evaluator
    // entry point, takes a caller-supplied resolver, and carries no agent — the
    // ceiling is a fact about agents.
    if identity_bearing_authority_requested(&args)? {
        super::shared::refuse_identity_bearing_retrieval(&resources, &args, "content_read").await?;
    }
    let resolver = resources.content_acquisition_resolver().ok_or_else(|| {
        ExecutionError::Step("content acquisition resolver is not configured".into())
    })?;
    let settings = resources
        .magician_config_snapshot()
        .content_acquisition
        .progressive_retrieval;
    execute_with_runtime(resolver, settings, args).await
}

/// Execute the compiled handler against an explicitly supplied production
/// resolver. This keeps CLI/live evaluators on the same argument parsing,
/// ladder, authority, and result path as normal compiled-tool dispatch.
pub async fn execute_with_runtime(
    resolver: Arc<ContentAcquisitionResolver>,
    settings: ProgressiveRetrievalSettings,
    args: Value,
) -> Result<Value, ExecutionError> {
    execute_with_runtime_and_cancellation(resolver, settings, args, execution_cancellation_token())
        .await
}

/// Run one read with an explicit cancellation scope. Vector acquisition uses a
/// child token so sufficient evidence can stop sibling reads without
/// cancelling the owning execution.
pub async fn execute_with_runtime_and_cancellation(
    resolver: Arc<ContentAcquisitionResolver>,
    settings: ProgressiveRetrievalSettings,
    args: Value,
    cancellation: CancellationToken,
) -> Result<Value, ExecutionError> {
    if uses_vector_contract(&args) {
        return super::content_batch::execute_read_vector_with_runtime_and_cancellation(
            resolver,
            settings,
            args,
            cancellation,
        )
        .await;
    }
    let principal = require_scope_str(&args, "__principal", "content_read")?;
    let workspace = require_scope_str(&args, "__workspace", "content_read")?;
    let service = resolver
        .resolve(principal, workspace)
        .await
        .map_err(|error| ExecutionError::Step(format!("resolving content acquisition: {error}")))?;
    execute_with_service(service, settings, args, cancellation).await
}

fn uses_vector_contract(args: &Value) -> bool {
    [
        "requests",
        "common",
        "max_concurrency",
        "minimum_successes",
        "per_host_concurrency",
    ]
    .iter()
    .any(|field| args.get(*field).is_some())
}

pub async fn execute_with_service(
    service: Arc<ContentAcquisitionService>,
    settings: ProgressiveRetrievalSettings,
    args: Value,
    cancellation: CancellationToken,
) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "content_read")?;
    let workspace = require_scope_str(&args, "__workspace", "content_read")?;
    let (candidate, selection_receipt) = match args.get("candidate") {
        Some(value) => {
            let candidate: ContentCandidate =
                serde_json::from_value(value.clone()).map_err(|error| {
                    ExecutionError::Step(format!("content_read candidate is invalid: {error}"))
                })?;
            let receipt = args
                .get("selection_receipt")
                .and_then(Value::as_str)
                .map(str::to_string);
            (candidate, receipt)
        },
        None => (direct_candidate(&args)?, None),
    };
    let (depth, output) = read_output(&args)?;
    let need = RetrievalNeed {
        schema_version: RETRIEVAL_NEED_SCHEMA_VERSION,
        principal,
        workspace,
        operation: RetrievalOperation::Read,
        target: RetrievalTarget::Candidate {
            candidate,
            selection_receipt,
        },
        goal: EvidenceGoal::Read(ReadEvidenceGoal {
            depth,
            output,
            required_metadata: args
                .get("required_metadata")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            min_chars: args
                .get("min_chars")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
        }),
        freshness: if args.get("fresh").and_then(Value::as_bool) == Some(true) {
            FreshnessPolicy::Fresh
        } else {
            FreshnessPolicy::CachedOk
        },
        remote_query_policy: RemoteDataPolicy::Deny,
        remote_content_policy: RemoteDataPolicy::Allow,
        invocation_source: ContentInvocationSource::InteractiveRead,
        maximum_authority: maximum_authority(&args)?,
        authority_grant_id: args
            .get("authority_grant_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        deadline_ms: args.get("deadline_ms").and_then(Value::as_u64),
        max_attempts: args
            .get("max_attempts")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        cost_budget_microunits: cost_budget(&args)?,
        allowed_actions: args
            .get("allowed_actions")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    };
    let result = service
        .retrieval_controller(settings)
        .retrieve(need, cancellation)
        .await
        .map_err(|error| ExecutionError::Step(format!("content read failed: {error}")))?;
    let mut value = serde_json::to_value(result)
        .map_err(|error| ExecutionError::Step(format!("serializing content read: {error}")))?;
    project_scalar_evidence_classification(&mut value);
    Ok(value)
}

/// Bound of the model-facing `excerpt` an opened-page producer carries beside
/// its full `document.text`. Sized under the autonomous surface's projection
/// scalar budget (8 KiB) so the excerpt survives projection whole while the
/// full text is retained in the raw result and its content hash.
pub(crate) const OPENED_PAGE_EXCERPT_CHARS: usize = 6_000;

/// The bounded excerpt every sanctioned page producer emits — `web_fetch` and
/// the browser `read` primitive share it with the vector read lane so the
/// model, and the terminal grounding judge, see the same kind of window into
/// a page regardless of which path opened it.
pub(crate) fn opened_page_excerpt(text: &str) -> (String, bool) {
    super::content_batch::bounded_text_excerpt(text, OPENED_PAGE_EXCERPT_CHARS)
}

/// The per-document claim-eligibility decision, in one place.
///
/// Keep evidence origin visible even if a bounded model projection admits a
/// document excerpt but cannot retain the document's nested metadata. The pack
/// contract admits these three top-level fields atomically.
///
/// `pub(crate)` because eligibility is a property of a document, not of a
/// tool: `web_fetch` carries this verdict to its root and the browser `read`
/// primitive runs it on the document it materialises, so every sanctioned
/// evidence path is admitted — or declared non-evidence — by the same rule.
/// A genuinely opened, complete page is claim-eligible. A discovery snippet
/// (`inline_only`) and a degraded fetch — a login wall, an error page, a
/// JavaScript shell, an incomplete read — are `discovery_only`: still emitted,
/// so a reviewer can name them as coverage, never admitted as a source.
pub(crate) fn project_scalar_evidence_classification(value: &mut Value) {
    let Some(root) = value.as_object_mut() else {
        return;
    };
    let status = root
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let Some(document) = root.get("document").and_then(Value::as_object) else {
        return;
    };
    let inline_only = document
        .get("media_type")
        .and_then(Value::as_str)
        .is_some_and(|media_type| media_type.contains("source=inline"))
        || document
            .get("metadata")
            .and_then(|metadata| metadata.get("fetch_status"))
            .and_then(Value::as_str)
            == Some("not_fetched");
    let (fetch_status, evidence_role, claim_eligible) = if inline_only {
        ("not_fetched", "discovery_only", false)
    } else if status == "complete" {
        ("complete", "opened_page", true)
    } else {
        ("partial", "discovery_only", false)
    };
    root.insert("fetch_status".into(), Value::String(fetch_status.into()));
    root.insert("evidence_role".into(), Value::String(evidence_role.into()));
    root.insert("claim_eligible".into(), Value::Bool(claim_eligible));
}

/// Whether this call asks for an owner-identity authority ANYWHERE in it.
///
/// The scalar form names one `maximum_authority` at the top level. The vector
/// form spreads it across three places: a top-level spelling that `parse_batch`
/// folds into `common`, `common` itself, and a per-branch override inside each
/// `requests` entry — all three are in the model-facing schema, and every one of
/// them accepts `authenticated_read` / `authenticated_interact`.
///
/// Reading only the top level would therefore have left the whole vector form
/// outside the ceiling: an envelope that names no authority above `requests`
/// reads as the `public_browser_read` default here, while a branch below asks
/// for the owner's signed-in Chrome and is executed through
/// `execute_with_service`, which never passes this point again.
///
/// A branch that names nothing inherits `common` (or the top level), which this
/// checks separately, so per-branch absence is not a gap.
fn identity_bearing_authority_requested(args: &Value) -> Result<bool, ExecutionError> {
    if names_identity_bearing_authority(args)? {
        return Ok(true);
    }
    if let Some(common) = args.get("common") {
        if names_identity_bearing_authority(common)? {
            return Ok(true);
        }
    }
    if let Some(requests) = args.get("requests").and_then(Value::as_array) {
        for request in requests {
            if names_identity_bearing_authority(request)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// The identity-bearing test on one argument object. A value that is not an
/// object, or that names no authority, reads as the public default — the same
/// answer [`maximum_authority`] gives, so the two cannot disagree about what a
/// missing key means.
fn names_identity_bearing_authority(args: &Value) -> Result<bool, ExecutionError> {
    Ok(matches!(
        maximum_authority(args)?,
        RetrievalAuthority::AuthenticatedRead | RetrievalAuthority::AuthenticatedInteract
    ))
}

fn maximum_authority(args: &Value) -> Result<RetrievalAuthority, ExecutionError> {
    match args
        .get("maximum_authority")
        .and_then(Value::as_str)
        .unwrap_or("public_browser_read")
    {
        "public_remote_read" => Ok(RetrievalAuthority::PublicRemoteRead),
        "public_browser_read" => Ok(RetrievalAuthority::PublicBrowserRead),
        "public_browser_interact" => Ok(RetrievalAuthority::PublicBrowserInteract),
        "authenticated_read" => Ok(RetrievalAuthority::AuthenticatedRead),
        "authenticated_interact" => Ok(RetrievalAuthority::AuthenticatedInteract),
        other => Err(ExecutionError::Step(format!(
            "unsupported content_read maximum_authority `{other}`"
        ))),
    }
}

fn read_output(args: &Value) -> Result<(ReadDepth, Option<RetrievalOutputKind>), ExecutionError> {
    let output = match args.get("output_kind").and_then(Value::as_str) {
        None => None,
        Some("gist") => Some(RetrievalOutputKind::Gist),
        Some("full_text") => Some(RetrievalOutputKind::FullText),
        Some("structured") => Some(RetrievalOutputKind::Structured),
        Some(other) => {
            return Err(ExecutionError::Step(format!(
                "unsupported content_read output_kind `{other}`"
            )));
        },
    };
    let default_depth = match output {
        Some(RetrievalOutputKind::Gist | RetrievalOutputKind::Structured) => "gist",
        _ => "full_text",
    };
    let depth = match args
        .get("depth")
        .and_then(Value::as_str)
        .unwrap_or(default_depth)
    {
        "gist" => ReadDepth::Gist,
        "full_text" => ReadDepth::FullText,
        other => {
            return Err(ExecutionError::Step(format!(
                "unsupported content_read depth `{other}`"
            )));
        },
    };
    Ok((depth, output))
}

fn execution_cancellation_token() -> CancellationToken {
    crate::magician_v2::execution::compiled_dispatch::EXECUTION_CANCEL_TOKEN
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn direct_candidate(args: &Value) -> Result<ContentCandidate, ExecutionError> {
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| ExecutionError::Step("content_read requires `candidate` or `url`".into()))?;
    let canonical_url = crate::magician_v2::content_sources::canonicalize_http_url(url)
        .map_err(|error| ExecutionError::Step(format!("content_read URL is invalid: {error}")))?;
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("Direct web page")
        .to_string();
    // Direct URLs carry explicit read intent, but model-supplied text is not
    // evidence that the URL returned that text. Only a complete discovered
    // candidate protected by its server-issued selection receipt may use the
    // inline gist shortcut in the retrieval controller.
    let cheap_text = title.clone();
    Ok(ContentCandidate {
        schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
        identity: SourceIdentity::new(
            "direct-url",
            blake3::hash(canonical_url.as_bytes()).to_hex().to_string(),
        )
        .map_err(|error| ExecutionError::Step(error.to_string()))?,
        title,
        cheap_text,
        canonical_url: Some(canonical_url.clone()),
        published_at_ms: None,
        observed_at_ms: Utc::now().timestamp_millis(),
        privacy: ContentPrivacy::Public,
        content_hash: None,
        provenance: ContentProvenance {
            source_label: url::Url::parse(&canonical_url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string))
                .unwrap_or_else(|| "web".into()),
            source_url: Some(canonical_url),
            retrieved_by: "direct-user-target".into(),
        },
        metadata: BTreeMap::new(),
    })
}

fn cost_budget(args: &Value) -> Result<BTreeMap<String, u64>, ExecutionError> {
    let Some(value) = args.get("cost_budget_microunits") else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| ExecutionError::Step("content_read cost budget must be an object".into()))?;
    object
        .iter()
        .map(|(commodity, value)| {
            value
                .as_u64()
                .filter(|amount| *amount > 0)
                .map(|amount| (commodity.clone(), amount))
                .ok_or_else(|| {
                    ExecutionError::Step(format!(
                        "content_read cost budget `{commodity}` must be a positive integer"
                    ))
                })
        })
        .collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn request_shape_selects_scalar_or_vector_scheduler_without_tool_renaming() {
        assert!(!uses_vector_contract(
            &json!({"url": "https://example.com"})
        ));
        assert!(uses_vector_contract(&json!({
            "requests": [{"url": "https://example.com"}]
        })));
        assert!(uses_vector_contract(&json!({"common": {"depth": "gist"}})));
        assert!(uses_vector_contract(&json!({"max_concurrency": 2})));
        assert!(uses_vector_contract(&json!({"per_host_concurrency": 1})));
    }

    #[test]
    fn structured_output_defaults_to_gist_depth() {
        let (depth, output) = read_output(&json!({"output_kind": "structured"})).unwrap();
        assert_eq!(depth, ReadDepth::Gist);
        assert_eq!(output, Some(RetrievalOutputKind::Structured));
        assert!(read_output(&json!({"output_kind": "browser"})).is_err());
    }

    #[test]
    fn direct_candidate_is_canonical_and_contains_no_credentials() {
        let candidate = direct_candidate(&json!({
            "url": "https://example.com/article?utm_source=test&section=one#fragment",
            "title": "Article",
            "inline_text": "Fabricated page body that must never bypass the reader."
        }))
        .unwrap();
        assert_eq!(
            candidate.canonical_url.as_deref(),
            Some("https://example.com/article?section=one")
        );
        assert_eq!(candidate.cheap_text, "Article");
        assert!(direct_candidate(&json!({
            "url": "https://user:secret@example.com/private"
        }))
        .is_err());
    }

    #[test]
    fn scalar_projection_keeps_inline_and_opened_page_evidence_roles_explicit() {
        let mut inline = json!({
            "status": "complete",
            "document": {
                "media_type": "text/plain; source=inline",
                "text": "selection snippet"
            }
        });
        project_scalar_evidence_classification(&mut inline);
        assert_eq!(inline["fetch_status"], "not_fetched");
        assert_eq!(inline["evidence_role"], "discovery_only");
        assert_eq!(inline["claim_eligible"], false);

        let mut opened = json!({
            "status": "complete",
            "document": {
                "media_type": "text/html",
                "text": "fetched page"
            }
        });
        project_scalar_evidence_classification(&mut opened);
        assert_eq!(opened["fetch_status"], "complete");
        assert_eq!(opened["evidence_role"], "opened_page");
        assert_eq!(opened["claim_eligible"], true);

        // A degraded fetch — a shell the ladder could not read cleanly — is
        // still reported, but it is not a source a claim may rest on.
        let mut degraded = json!({
            "status": "degraded",
            "document": {
                "media_type": "text/html",
                "text": "Please enable JavaScript to view this page"
            }
        });
        project_scalar_evidence_classification(&mut degraded);
        assert_eq!(degraded["fetch_status"], "partial");
        assert_eq!(degraded["evidence_role"], "discovery_only");
        assert_eq!(degraded["claim_eligible"], false);
    }

    #[test]
    fn cost_budget_requires_positive_integer_amounts() {
        assert!(cost_budget(&json!({"cost_budget_microunits": {"usd": 0}})).is_err());
        assert!(cost_budget(&json!({"cost_budget_microunits": {"usd": "5"}})).is_err());
        assert_eq!(
            cost_budget(&json!({"cost_budget_microunits": {"usd": 5000}})).unwrap()["usd"],
            5000
        );
    }

    #[tokio::test]
    async fn retrieval_inherits_compiled_dispatch_cancellation() {
        let parent = CancellationToken::new();
        crate::magician_v2::execution::compiled_dispatch::EXECUTION_CANCEL_TOKEN
            .scope(Some(parent.clone()), async move {
                let inherited = execution_cancellation_token();
                assert!(!inherited.is_cancelled());
                parent.cancel();
                assert!(inherited.is_cancelled());
            })
            .await;
    }
}
