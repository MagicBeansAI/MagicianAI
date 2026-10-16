//! Deterministic backward compiler from one browser execution to a task recipe.

pub mod llm_fallback;
pub mod locate;
pub mod resolve;
pub mod shape;
pub mod values;

use crate::magician_v2::api_mining::capability::{classify_side_effects_for_request, SideEffects};
use crate::magician_v2::api_mining::recipe::*;
use crate::magician_v2::api_mining::router::extract_origin;
use crate::magician_v2::api_mining::types::NetworkTraceEvent;
use crate::magician_v2::api_mining::workflow::{BrowserFallbackStep, InferenceMethod};
use resolve::{ResolveContext, ResolvedRequest, TokenLocation, TokenResolution};
use std::collections::{BTreeMap, HashMap};
use values::ReportedValues;

pub struct RecipeCompileInput {
    pub task_id: String,
    pub execution_id: String,
    pub monitor_revision: Option<u32>,
    pub agent_id: String,
    pub principal: String,
    pub workspace: String,
    pub task_title: String,
    pub task_text: String,
    pub reported: ReportedValues,
    pub traces: Vec<NetworkTraceEvent>,
    pub typed_inputs: Vec<String>,
    pub trace_files: Vec<String>,
    pub sequences: Vec<crate::magician_v2::api_mining::sequence::CapabilitySequence>,
}

/// Ephemeral proof material for validating an optional model refinement
/// against the exact sorted traces used by the deterministic compiler. It is
/// never serialized into the durable recipe.
pub struct RecipeCompileEvidence {
    trace_index_by_step: HashMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    NoTraces,
    NoAnswerBearingResponse,
    IncompleteAnswerCoverage,
    /// The task names a field a captured response carries, and no located
    /// answer is that field: the agent reported something else (a title for
    /// an author). A recipe built from it would replay a confident wrong
    /// answer, so the task keeps the browser.
    AnswerFieldNotReported {
        asked: Vec<String>,
    },
    NoResolvableSteps,
    /// A step's capture is missing the request body its own Content-Type
    /// implies. Some browser engines export a HAR without `postData`, so the
    /// body never reaches the miner; replaying such a step sends nothing and
    /// the server rejects it. Failing here beats publishing a recipe that
    /// cannot work.
    CaptureMissingRequestBody {
        method: String,
        content_type: String,
    },
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTraces => write!(formatter, "no usable network traces"),
            Self::NoAnswerBearingResponse => {
                write!(formatter, "no response carried a reported value")
            },
            Self::IncompleteAnswerCoverage => {
                write!(
                    formatter,
                    "captured responses do not cover every reported answer value"
                )
            },
            Self::AnswerFieldNotReported { asked } => write!(
                formatter,
                "the task asks for {} and no reported answer is that field",
                asked.join(", ")
            ),
            Self::NoResolvableSteps => write!(formatter, "no step could be resolved"),
            Self::CaptureMissingRequestBody {
                method,
                content_type,
            } => write!(
                formatter,
                "the capture has no request body for a {method} declaring {content_type}; \
                 this browser engine did not record it"
            ),
        }
    }
}

impl std::error::Error for CompileError {}

impl TaskRecipe {
    pub fn compiled_from(&self) -> Option<&CompiledFrom> {
        self.current().map(|version| &version.compiled_from)
    }
}

fn usable(trace: &NetworkTraceEvent) -> bool {
    if trace.method.eq_ignore_ascii_case("OPTIONS") || !(200..400).contains(&trace.status) {
        return false;
    }
    // Empty successful POSTs can be real writes (204 acknowledgements).
    // Keep them available to causal/resource analysis; without an answer or
    // dependency edge, background beacons will not enter the compiled DAG.
    matches!(
        trace.resource_type.as_deref(),
        Some("XHR" | "Fetch" | "Document") | None
    )
}

/// Response bodies larger than this are not scanned for quoted page chrome.
const MAX_PAGE_TEXT_BYTES: usize = 512 * 1024;

pub fn compile_task_recipe(input: &RecipeCompileInput) -> Result<TaskRecipe, CompileError> {
    compile_task_recipe_with_evidence(input).map(|(recipe, _)| recipe)
}

pub fn compile_task_recipe_with_evidence(
    input: &RecipeCompileInput,
) -> Result<(TaskRecipe, RecipeCompileEvidence), CompileError> {
    let mut traces: Vec<_> = input
        .traces
        .iter()
        .filter(|trace| usable(trace))
        .cloned()
        .collect();
    if traces.is_empty() {
        return Err(CompileError::NoTraces);
    }
    traces.sort_by_key(|trace| trace.timestamp);

    let hits = locate::locate_answer_traces(&traces, &input.reported);
    if hits.is_empty() {
        return Err(CompileError::NoAnswerBearingResponse);
    }
    // Rail 1 ends the entire task when this answer spec replays successfully.
    // A partial match is useful mining evidence, but is not authority to omit
    // the rest of the task's reported answer on its next execution. Two kinds
    // of reported value are not part of that answer, though: one the task text
    // itself contains (a summary restating its goal, URL or query — an input,
    // which may still locate a response when a write echoes it back), and one
    // contained in a value a response does carry (a number inside a title the
    // recipe already extracts whole).
    // The task names the field it wants. When a captured response carries a
    // key by that name and none of the located answers is that key, the
    // agent reported a different value (the title, asked for the author); a
    // recipe answering it would replay wrong with full confidence. This runs
    // before coverage so the answer's own fate is decided on its own terms.
    let asked = locate::asked_response_keys(&traces, &input.task_title, &input.task_text);
    // A label the prose invented is not evidence that the agent answered the
    // wrong question. When the value it reported is exactly what the asked key
    // carries, the agent answered correctly and simply called the field
    // something else — and refusing there loses a working recipe over a
    // synonym. Measured: the same task on the same site compiled when the model
    // wrote "points" and refused nine hours later when it wrote "score".
    if !asked.is_empty()
        && !hits.iter().any(|hit| {
            hit.field
                .as_deref()
                .map(|field| asked.contains(&field.to_ascii_lowercase()))
                .unwrap_or(false)
        })
        && !hits
            .iter()
            .any(|hit| locate::asked_key_carries_value(&traces, &asked, &hit.value))
    {
        tracing::info!(
            task_id = %input.task_id,
            asked = ?asked,
            reported_fields = ?hits.iter().filter_map(|hit| hit.field.clone()).collect::<Vec<_>>(),
            "[API_MINING] recipe compile: the task's field was not among the reported answers"
        );
        return Err(CompileError::AnswerFieldNotReported { asked });
    }
    // What the summary quotes around the answer — the page's own chrome — is
    // narration when a captured response actually contains it.
    let page_text: Vec<String> = traces
        .iter()
        .filter(|trace| (200..300).contains(&trace.status))
        .filter_map(|trace| trace.response_body.as_deref())
        .filter(|body| !body.is_empty() && body.len() <= MAX_PAGE_TEXT_BYTES)
        .map(|body| values::normalize_value(body))
        .collect();
    if let Some(index) = values::first_uncovered(
        &input.reported,
        &hits,
        &input.task_title,
        &input.task_text,
        &page_text,
    ) {
        tracing::info!(
            task_id = %input.task_id,
            value = %input.reported.values[index].normalized,
            field = ?input.reported.values[index].field,
            "[API_MINING] recipe compile: reported value not carried by any response"
        );
        return Err(CompileError::IncompleteAnswerCoverage);
    }
    let mut targets: Vec<_> = hits.iter().map(|hit| hit.trace_index).collect();
    let causal_writes = causal_write_targets(&traces, &targets);
    targets.extend(causal_writes);
    targets.sort_unstable();
    targets.dedup();
    // A write cannot publish without a read that proves it landed — replay
    // preflight rejects "write step s0 has no verification read". When the
    // answer was located in the write's own echo (a create that returns the
    // created row), the capture's follow-up read is still in the trace, so
    // bring it along for `assign_verify_with` to bind.
    let verification_reads = verification_read_targets(&traces, &targets);
    targets.extend(verification_reads);
    targets.sort_unstable();
    targets.dedup();

    let task_context = format!("{} {}", input.task_title, input.task_text);
    let context = ResolveContext {
        traces: &traces,
        typed_inputs: &input.typed_inputs,
        task_text: &task_context,
    };
    let resolved = resolve::resolve_closure(&context, &targets);
    if resolved.is_empty() {
        return Err(CompileError::NoResolvableSteps);
    }
    // A request that declares a body content-type but carries no body was not
    // captured completely. Measured live: one engine's HAR export omits
    // `postData` entirely, which compiled a search POST with an empty body
    // that the API answered 400. Refuse instead of publishing that.
    for request in &resolved {
        let trace = &traces[request.trace_index];
        let declares_body = trace
            .request_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.clone());
        let has_body = trace
            .request_body
            .as_deref()
            .is_some_and(|body| !body.trim().is_empty());
        if let Some(content_type) = declares_body {
            if !has_body && !trace.method.eq_ignore_ascii_case("GET") {
                tracing::info!(
                    task_id = %input.task_id,
                    method = %trace.method,
                    %content_type,
                    "[API_MINING] recipe compile: the capture has no request body for a step that declares one"
                );
                return Err(CompileError::CaptureMissingRequestBody {
                    method: trace.method.clone(),
                    content_type,
                });
            }
        }
    }

    let step_id_for: HashMap<usize, String> = resolved
        .iter()
        .enumerate()
        .map(|(index, request)| (request.trace_index, format!("s{index}")))
        .collect();
    let mut steps = Vec::with_capacity(resolved.len());
    let mut flows = Vec::new();
    let mut inputs = BTreeMap::<String, TaskInput>::new();
    let mut needs_session = false;

    for request in &resolved {
        let trace = &traces[request.trace_index];
        let step_id = step_id_for[&request.trace_index].clone();
        let mut param_sources = HashMap::with_capacity(request.params.len());
        let mut body_param_types = HashMap::new();
        for (token, resolution) in &request.params {
            // Stable literals already remain in the concrete template. They
            // are not runtime parameters and retaining them would duplicate
            // values in durable metadata while doing needless replay work.
            if matches!(resolution, TokenResolution::Literal { volatile: false }) {
                continue;
            }
            if matches!(
                &token.location,
                TokenLocation::BodyKey { .. }
                    | TokenLocation::JsonBodyPath { .. }
                    | TokenLocation::OpaqueBody
            ) {
                body_param_types.insert(token.name.clone(), token.value_type);
            }
            let source = match resolution {
                TokenResolution::FromResponse {
                    source_trace,
                    extractor,
                } => {
                    let Some(source_step) = step_id_for.get(source_trace) else {
                        // A dependency beyond the depth cap cannot safely be
                        // replayed. Preserve only the unresolved shape: a
                        // captured volatile may be a signature or nonce and
                        // must never become durable recipe material.
                        param_sources.insert(
                            token.name.clone(),
                            RecipeParamSource::Literal {
                                value: String::new(),
                                volatile: true,
                            },
                        );
                        continue;
                    };
                    let flow_id = format!("df_{}", ulid::Ulid::new());
                    flows.push(RecipeDataFlow {
                        id: flow_id.clone(),
                        source_step: source_step.clone(),
                        extractor: extractor.clone(),
                        target_step: step_id.clone(),
                        target_param: token.name.clone(),
                        confidence: 0.9,
                        inference: InferenceMethod::AutoMatch,
                    });
                    RecipeParamSource::DataFlow { flow_id }
                },
                TokenResolution::FromTypedInput { name }
                | TokenResolution::FromTaskText { name } => {
                    // One task value often appears in several request
                    // locations (query + JSON variable + custom header). Bind
                    // those locations to one canonical input; otherwise only
                    // the first name reaches the task template and later
                    // matching can never populate the duplicates.
                    let canonical_name = inputs
                        .values()
                        .find(|input| {
                            input.example_value == token.value && input.schema == token.value_type
                        })
                        .map(|input| input.name.clone())
                        .unwrap_or_else(|| {
                            // Token names are unique within one request, not
                            // across the task (e.g. two searches both use q).
                            let mut candidate = name.clone();
                            let mut suffix = 2;
                            while inputs.contains_key(&candidate) {
                                candidate = format!("{name}_{suffix}");
                                suffix += 1;
                            }
                            candidate
                        });
                    inputs
                        .entry(canonical_name.clone())
                        .or_insert_with(|| TaskInput {
                            name: canonical_name.clone(),
                            schema: token.value_type,
                            example_value: token.value.clone(),
                            source: if matches!(resolution, TokenResolution::FromTypedInput { .. })
                            {
                                TaskInputSource::BrowserTyped
                            } else {
                                TaskInputSource::TaskText
                            },
                        });
                    RecipeParamSource::TaskInput {
                        name: canonical_name,
                    }
                },
                TokenResolution::FromSession { scheme } => {
                    needs_session = true;
                    RecipeParamSource::SessionAuth {
                        scheme: scheme.clone(),
                    }
                },
                TokenResolution::Now { unit } => RecipeParamSource::Now { unit: *unit },
                TokenResolution::Literal { volatile } => RecipeParamSource::Literal {
                    value: if *volatile {
                        String::new()
                    } else {
                        token.value.clone()
                    },
                    volatile: *volatile,
                },
            };
            param_sources.insert(token.name.clone(), source);
        }

        if trace
            .request_headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("cookie"))
        {
            needs_session = true;
            // Cookie values stay outside the recipe, but their presence is an
            // execution requirement. Without this marker a lost capture could
            // turn an authenticated read into a successful anonymous answer.
            param_sources.insert(
                "__session_cookies".into(),
                RecipeParamSource::SessionAuth {
                    scheme: "cookie_header".into(),
                },
            );
        }
        let content_type = trace
            .request_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.as_str());
        let graphql_kind = super::miner::detect_graphql_request_info_standalone(
            trace.request_body.as_deref(),
            content_type,
        )
        .map(|info| info.operation_kind);
        let side_effects = classify_side_effects_for_request(
            &trace.method,
            &request.url_template,
            request.body_template.as_deref(),
            graphql_kind,
        );
        steps.push(RecipeStep {
            id: step_id,
            origin: extract_origin(&trace.url),
            method: trace.method.to_ascii_uppercase(),
            url_template: request.url_template.clone(),
            headers_template: request.headers_template.clone(),
            body_template: request.body_template.clone(),
            capability_id: None,
            param_sources,
            body_param_types,
            side_effects,
            request_shape_fingerprint: request_shape_fingerprint_with_headers(
                &trace.method,
                &request.url_template,
                &request.headers_template,
                request.body_template.as_deref(),
            ),
            verify_with: None,
            browser_fallback: None,
            transport_hint: None,
        });
    }
    assign_verify_with(&mut steps);
    attach_sequence_context(&mut steps, &traces, &resolved, &input.sequences);

    let mut answer_name_counts = HashMap::<String, usize>::new();
    let answer_spec = hits
        .iter()
        .enumerate()
        .filter_map(|(index, hit)| {
            step_id_for.get(&hit.trace_index).map(|step_id| {
                // A scalar reported without a semantic key must not turn its
                // private value into durable recipe metadata.
                let base = hit
                    .field
                    .clone()
                    .filter(|field| {
                        !field.trim().is_empty()
                            && field.len() <= 128
                            && !field.chars().any(char::is_control)
                    })
                    .unwrap_or_else(|| format!("answer_{}", index + 1));
                let count = answer_name_counts.entry(base.clone()).or_default();
                *count += 1;
                let field = if *count == 1 {
                    base
                } else {
                    format!("{base}_{}", *count)
                };
                AnswerField {
                    field,
                    step_id: step_id.clone(),
                    extractor: hit.extractor.clone(),
                }
            })
        })
        .collect();

    let input_pairs: Vec<_> = inputs
        .values()
        .map(|input| (input.name.clone(), input.example_value.clone()))
        .collect();
    let template = shape::deterministic_template(&input.task_title, &input_pairs);
    let description_template = shape::deterministic_template(&input.task_text, &input_pairs);
    let fingerprint = shape::contextual_shape_fingerprint(
        &template,
        Some(&description_template),
        &input.agent_id,
        &input.principal,
        &input.workspace,
    );
    let mut origins: Vec<_> = steps.iter().map(|step| step.origin.clone()).collect();
    origins.sort_unstable();
    origins.dedup();
    let sequence_ids = input
        .sequences
        .iter()
        .map(|sequence| sequence.id.clone())
        .collect();

    let trace_index_by_step = step_id_for
        .iter()
        .map(|(trace_index, step_id)| (step_id.clone(), *trace_index))
        .collect();
    let recipe = TaskRecipe {
        id: format!("rcp_{}", ulid::Ulid::new()),
        scope_principal: input.principal.clone(),
        scope_workspace: input.workspace.clone(),
        agent_id: input.agent_id.clone(),
        shape: RecipeShape {
            template,
            description_template: Some(description_template),
            fingerprint,
            inputs: inputs.into_values().collect(),
        },
        current_version: 1,
        versions: vec![RecipeVersion {
            version: 1,
            origins: origins.clone(),
            steps,
            data_flows: flows,
            answer_spec,
            auth: RecipeAuth {
                requires_session: needs_session,
                origins_needing_auth: if needs_session { origins } else { Vec::new() },
                login_step_id: None,
            },
            maturity: RecipeMaturity::Draft,
            replay_stats: Default::default(),
            compiled_from: CompiledFrom {
                task_id: input.task_id.clone(),
                execution_id: input.execution_id.clone(),
                task_text_fingerprint: Some(shape::source_task_fingerprint(
                    &input.task_title,
                    &input.task_text,
                )),
                monitor_revision: input.monitor_revision,
                sequence_ids,
                trace_files: input.trace_files.clone(),
            },
            compiled_at_ms: chrono::Utc::now().timestamp_millis(),
            last_replayed_at_ms: None,
        }],
    };
    Ok((
        recipe,
        RecipeCompileEvidence {
            trace_index_by_step,
        },
    ))
}

/// Include successful mutations that immediately precede an answer-bearing
/// read in the same API resource family. Some sites acknowledge a write with
/// an empty response and then issue a static GET/GraphQL read, leaving no value
/// edge for the normal backward resolver to follow. The narrow origin/path and
/// time window avoids sweeping unrelated task traffic into the recipe; every
/// retained write remains protected by the replay approval gate.
/// The earliest later read of the same resource for every write already in
/// `targets` that has none. Without it a create-shaped recipe is unpublishable.
fn verification_read_targets(traces: &[NetworkTraceEvent], targets: &[usize]) -> Vec<usize> {
    let is_verifying_read = |index: usize, family: &ResourceFamily| {
        traces.get(index).is_some_and(|trace| {
            (200..300).contains(&trace.status)
                && trace_side_effects(trace) == SideEffects::ReadOnly
                && resource_family(&trace.url)
                    .is_some_and(|candidate| same_resource_family(family, &candidate))
        })
    };
    let mut found = Vec::new();
    for write_index in targets.iter().copied() {
        let Some(write) = traces.get(write_index) else {
            continue;
        };
        if trace_side_effects(write) != SideEffects::Write {
            continue;
        }
        let Some(family) = resource_family(&write.url) else {
            continue;
        };
        let already_verified = targets
            .iter()
            .copied()
            .any(|candidate| candidate > write_index && is_verifying_read(candidate, &family));
        if already_verified {
            continue;
        }
        if let Some(read) =
            (write_index + 1..traces.len()).find(|candidate| is_verifying_read(*candidate, &family))
        {
            found.push(read);
        }
    }
    found
}

fn causal_write_targets(traces: &[NetworkTraceEvent], answer_targets: &[usize]) -> Vec<usize> {
    const MAX_CAUSAL_WRITE_WINDOW_MS: i64 = 15_000;
    let answer_families: Vec<_> = answer_targets
        .iter()
        .filter_map(|index| {
            traces
                .get(*index)
                .map(|trace| (*index, resource_family(&trace.url)))
        })
        .collect();
    traces
        .iter()
        .enumerate()
        .filter(|(_, trace)| (200..300).contains(&trace.status))
        .filter(|(_, trace)| trace_side_effects(trace) == SideEffects::Write)
        .filter(|(write_index, write)| {
            let family = resource_family(&write.url);
            answer_families.iter().any(|(answer_index, answer_family)| {
                write_index < answer_index
                    && family
                        .as_ref()
                        .zip(answer_family.as_ref())
                        .is_some_and(|(write, answer)| same_resource_family(write, answer))
                    && traces[*answer_index]
                        .timestamp
                        .saturating_sub(write.timestamp)
                        <= MAX_CAUSAL_WRITE_WINDOW_MS
            })
        })
        .map(|(index, _)| index)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResourceFamily {
    origin: String,
    segments: Vec<String>,
}

fn resource_family(value: &str) -> Option<ResourceFamily> {
    let url = url::Url::parse(value).ok()?;
    let segments: Vec<_> = url
        .path_segments()?
        .map(|segment| {
            urlencoding::decode(segment)
                .map(|value| value.into_owned())
                .unwrap_or_else(|_| segment.to_owned())
                .to_ascii_lowercase()
        })
        .collect();
    if segments.is_empty() {
        return None;
    }
    Some(ResourceFamily {
        origin: url.origin().ascii_serialization(),
        segments,
    })
}

fn same_resource_family(left: &ResourceFamily, right: &ResourceFamily) -> bool {
    if left.origin != right.origin {
        return false;
    }
    if equivalent_resource_segments(&left.segments, &right.segments) {
        return true;
    }
    let (shorter, longer) = if left.segments.len() < right.segments.len() {
        (&left.segments, &right.segments)
    } else {
        (&right.segments, &left.segments)
    };
    longer.len() == shorter.len().saturating_add(1)
        && equivalent_resource_segments(shorter, &longer[..shorter.len()])
        && longer.last().is_some_and(|segment| {
            !is_api_version_segment(segment) && looks_dynamic_resource_segment(segment)
        })
}

fn equivalent_resource_segments(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left == right || is_placeholder(left) && is_placeholder(right))
}

fn is_placeholder(segment: &str) -> bool {
    segment.starts_with('{') && segment.ends_with('}') && segment.len() > 2
}

fn is_api_version_segment(segment: &str) -> bool {
    segment.strip_prefix('v').is_some_and(|version| {
        !version.is_empty() && version.chars().all(|value| value.is_ascii_digit())
    })
}

fn looks_dynamic_resource_segment(segment: &str) -> bool {
    is_placeholder(segment)
        || segment.chars().any(|character| character.is_ascii_digit())
        || segment.len() >= 16
}

fn trace_side_effects(trace: &NetworkTraceEvent) -> SideEffects {
    let content_type = trace
        .request_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.as_str());
    let graphql_kind = super::miner::detect_graphql_request_info_standalone(
        trace.request_body.as_deref(),
        content_type,
    )
    .map(|info| info.operation_kind);
    classify_side_effects_for_request(
        &trace.method,
        &trace.url,
        trace.request_body.as_deref(),
        graphql_kind,
    )
}

fn attach_sequence_context(
    steps: &mut [RecipeStep],
    traces: &[NetworkTraceEvent],
    resolved: &[ResolvedRequest],
    sequences: &[crate::magician_v2::api_mining::sequence::CapabilitySequence],
) {
    use crate::magician_v2::api_mining::sequence::SequenceStep;
    let sequence_steps: Vec<&SequenceStep> = sequences
        .iter()
        .flat_map(|sequence| sequence.steps.iter())
        .collect();
    if sequence_steps.is_empty() {
        return;
    }
    let decode = |url: &str| {
        urlencoding::decode(url)
            .map(|value| value.into_owned())
            .unwrap_or_else(|_| url.to_owned())
    };
    for (step, request) in steps.iter_mut().zip(resolved) {
        let trace = &traces[request.trace_index];
        let exact = sequence_steps.iter().find(|sequence_step| {
            sequence_step.method.eq_ignore_ascii_case(&trace.method)
                && decode(&sequence_step.concrete_url) == decode(&trace.url)
        });
        let nearest = sequence_steps
            .iter()
            .filter(|sequence_step| {
                sequence_step.browser_action.is_some()
                    && sequence_step.timestamp_ms <= trace.timestamp
                    && trace.timestamp - sequence_step.timestamp_ms <= 2_000
            })
            .max_by_key(|sequence_step| sequence_step.timestamp_ms);
        if let Some(sequence_step) = exact.or(nearest) {
            if step.capability_id.is_none() {
                step.capability_id = sequence_step.capability_id.clone();
            }
            if let (Some(action), Some(arguments)) = (
                &sequence_step.browser_action,
                &sequence_step.browser_arguments,
            ) {
                step.browser_fallback = Some(BrowserFallbackStep {
                    action: action.clone(),
                    // Task Recipes are durable. Browser action values can
                    // contain typed credentials or account data; only their
                    // argument shape is retained for the agent handoff.
                    arguments: browser_argument_shape(arguments),
                    description: None,
                });
            }
        }
    }
}

fn browser_argument_shape(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Null => serde_json::Value::Null,
        serde_json::Value::Bool(_) => serde_json::Value::Bool(false),
        serde_json::Value::Number(_) => serde_json::Value::Number(0.into()),
        serde_json::Value::String(_) => serde_json::Value::String("<redacted>".into()),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(browser_argument_shape).collect())
        },
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), browser_argument_shape(value)))
                .collect(),
        ),
    }
}

fn assign_verify_with(steps: &mut [RecipeStep]) {
    fn resource_prefix(template: &str) -> Option<ResourceFamily> {
        resource_family(template)
    }

    let snapshot: Vec<_> = steps
        .iter()
        .map(|step| {
            (
                step.id.clone(),
                step.side_effects.clone(),
                resource_prefix(&step.url_template),
            )
        })
        .collect();
    for (index, step) in steps.iter_mut().enumerate() {
        if step.side_effects != SideEffects::Write {
            continue;
        }
        let Some(prefix) = resource_prefix(&step.url_template) else {
            continue;
        };
        step.verify_with = snapshot
            .iter()
            .skip(index + 1)
            .find(|(_, side_effects, candidate)| {
                *side_effects == SideEffects::ReadOnly
                    && candidate
                        .as_ref()
                        .is_some_and(|candidate| same_resource_family(&prefix, candidate))
            })
            .map(|(id, ..)| id.clone());
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::types::{RequestInitiator, RequestTiming};

    fn trace(method: &str, url: &str, timestamp: i64) -> NetworkTraceEvent {
        NetworkTraceEvent {
            request_id: format!("{method}-{timestamp}"),
            timestamp,
            method: method.into(),
            url: url.into(),
            resource_type: Some("Fetch".into()),
            frame_id: None,
            request_headers: HashMap::new(),
            request_body: (method != "GET").then(|| r#"{"name":"new"}"#.into()),
            tab_id: None,
            thread_id: None,
            status: 200,
            response_headers: HashMap::new(),
            response_body: Some("{}".into()),
            body_unavailable_reason: None,
            failure_error_text: None,
            failure_blocked_reason: None,
            failure_canceled: None,
            timing: RequestTiming {
                request_time: 0.0,
                dns_duration: None,
                connect_duration: None,
                ssl_duration: None,
                ttfb: None,
                total_duration: 1.0,
            },
            initiator: RequestInitiator {
                initiator_type: "script".into(),
                stack: None,
                url: None,
            },
            request_size: 0,
            response_size: 2,
            capture_source: Some("fixture".into()),
        }
    }

    #[test]
    fn separate_requests_with_the_same_parameter_name_keep_distinct_inputs() {
        let mut first = trace("GET", "https://example.test/search?q=blue", 1_000);
        first.response_body = Some(r#"{"name":"blue result"}"#.into());
        let mut second = trace("GET", "https://example.test/search?q=red", 2_000);
        second.response_body = Some(r#"{"name":"red result"}"#.into());
        let input = RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "execution".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Compare blue with red".into(),
            task_text: String::new(),
            reported: values::collect_reported_values(
                "",
                &[serde_json::json!({
                    "first": "blue result", "second": "red result"
                })],
                &[],
            ),
            traces: vec![first, second],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        };
        let recipe = compile_task_recipe(&input).unwrap();
        assert_eq!(recipe.shape.inputs.len(), 2);
        assert_eq!(recipe.shape.template, "compare {q} with {q_2}");
        let version = recipe.current().unwrap();
        assert_eq!(
            version.steps[0].param_sources["q"],
            RecipeParamSource::TaskInput { name: "q".into() }
        );
        assert_eq!(
            version.steps[1].param_sources["q"],
            RecipeParamSource::TaskInput { name: "q_2".into() }
        );
        assert_eq!(recipe.shape.inputs[0].example_value, "blue");
        assert_eq!(recipe.shape.inputs[1].example_value, "red");
    }

    #[test]
    fn answer_behind_a_literal_dotted_key_cannot_publish_a_wrong_nested_extractor() {
        let mut response = trace("GET", "https://example.test/api/profile", 1_000);
        response.response_body =
            Some(r#"{"profile.name":"intended name","profile":{"name":"different name"}}"#.into());
        let input = RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "cold".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Read profile name".into(),
            task_text: String::new(),
            reported: values::collect_reported_values(
                "",
                &[serde_json::json!({"name":"intended name"})],
                &[],
            ),
            traces: vec![response],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        };
        assert!(matches!(
            compile_task_recipe(&input),
            Err(CompileError::NoAnswerBearingResponse)
        ));
    }

    #[test]
    fn captured_cookie_requirement_survives_without_persisting_cookie_values() {
        let mut response = trace("GET", "https://example.test/api/profile", 1_000);
        response
            .request_headers
            .insert("Cookie".into(), "sid=private-cookie-value".into());
        response.response_body = Some(r#"{"name":"current name"}"#.into());
        let recipe = compile_task_recipe(&RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "cold".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Read profile name".into(),
            task_text: String::new(),
            reported: values::collect_reported_values(
                "",
                &[serde_json::json!({"name":"current name"})],
                &[],
            ),
            traces: vec![response],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        })
        .unwrap();
        let version = recipe.current().unwrap();
        assert!(version.auth.requires_session);
        assert!(
            matches!(version.steps[0].param_sources.get("__session_cookies"), Some(RecipeParamSource::SessionAuth { scheme }) if scheme == "cookie_header")
        );
        assert!(!serde_json::to_string(&recipe)
            .unwrap()
            .contains("private-cookie-value"));
    }

    #[tokio::test]
    async fn compiled_text_dependency_uses_current_value_and_stops_on_warm_ambiguity() {
        use crate::magician_v2::api_mining::{
            origin_policy::OriginPolicyStore,
            recipe_runner::{
                FailureClass, RecipeRunInputs, RecipeRunner, StepTransport, TransportRequest,
                TransportResponse,
            },
            replay_grants::ReplayGrantStore,
        };
        struct TextSource {
            ambiguous: bool,
        }
        #[async_trait::async_trait]
        impl StepTransport for TextSource {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, request: &TransportRequest) -> Result<TransportResponse, String> {
                assert_eq!(request.method, "GET");
                let url = url::Url::parse(&request.url).unwrap();
                let body = if url.path() == "/bootstrap" {
                    if self.ambiguous {
                        "<span>freshNonce87654321</span><span>unrelated</span>"
                    } else {
                        "<span>freshNonce87654321</span>"
                    }
                } else {
                    assert!(
                        !self.ambiguous,
                        "ambiguous source must not dispatch its dependent request"
                    );
                    assert_eq!(url.path(), "/api/items");
                    assert_eq!(request.headers["cookie"], "sid=fresh-session");
                    assert_eq!(
                        url.query_pairs()
                            .find(|(name, _)| name == "nonce")
                            .unwrap()
                            .1,
                        "freshNonce87654321"
                    );
                    r#"{"price":43}"#
                };
                Ok(TransportResponse {
                    status: 200,
                    headers: if url.path() == "/bootstrap" {
                        HashMap::from([(
                            "set-cookie".into(),
                            "sid=fresh-session; Path=/; Secure".into(),
                        )])
                    } else {
                        HashMap::new()
                    },
                    body: body.into(),
                })
            }
        }
        let mut source = trace("GET", "https://example.test/bootstrap", 1_000);
        source.response_body = Some("<span>coldNonce12345678</span>".into());
        let mut target = trace(
            "GET",
            "https://example.test/api/items?nonce=coldNonce12345678",
            2_000,
        );
        target
            .request_headers
            .insert("cookie".into(), "sid=cold-session".into());
        target.response_body = Some(r#"{"price":42}"#.into());
        let recipe = compile_task_recipe(&RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "cold".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Read fixture result".into(),
            task_text: String::new(),
            reported: values::collect_reported_values("", &[serde_json::json!({"price":42})], &[]),
            traces: vec![source, target],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        })
        .unwrap();
        assert_eq!(recipe.current().unwrap().steps.len(), 2);
        assert_eq!(recipe.current().unwrap().data_flows.len(), 1);
        assert!(!serde_json::to_string(&recipe)
            .unwrap()
            .contains("coldNonce12345678"));
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        for ambiguous in [false, true] {
            let mut replay_recipe = recipe.clone();
            let result = RecipeRunner {
                transports: vec![Box::new(TextSource { ambiguous })],
                grants: &grants,
                origin_policy: &policy,
                can_continue: None,
                session_lookup: &|_, _| None,
                auth_healer: None,
                max_auth_heals: 0,
                step_feedback: None,
                observer: None,
            }
            .run(&mut replay_recipe, &RecipeRunInputs::default())
            .await;
            if ambiguous {
                assert!(!result.success);
                assert_eq!(result.steps.len(), 1);
                assert_eq!(result.fallback.unwrap().class, FailureClass::SchemaDrift);
            } else {
                assert!(result.success, "{result:?}");
                assert_eq!(result.steps.len(), 2);
                assert_eq!(
                    serde_json::Value::Object(result.answer),
                    serde_json::json!({"price":43})
                );
            }
        }
    }

    #[test]
    fn partial_answer_evidence_cannot_compile_a_whole_task_replacement() {
        let mut response = trace("GET", "https://example.test/product?q=blue", 1_000);
        response.response_body = Some(r#"{"price":42}"#.into());
        let mut input = RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "execution".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Find price and availability for blue".into(),
            task_text: String::new(),
            reported: values::collect_reported_values(
                "",
                &[serde_json::json!({"price": 42, "availability": "in stock"})],
                &[],
            ),
            traces: vec![response],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        };
        assert!(matches!(
            compile_task_recipe(&input),
            Err(CompileError::IncompleteAnswerCoverage)
        ));

        input.traces[0].response_body = Some(r#"{"price":42,"availability":"in stock"}"#.into());
        assert_eq!(
            compile_task_recipe(&input)
                .unwrap()
                .current()
                .unwrap()
                .answer_spec
                .len(),
            2
        );

        // Unlabelled reported values are not permission to silently omit part
        // of the answer either (e.g. a second price only visible in the DOM).
        input.reported = values::collect_reported_values("42 99", &[], &[]);
        assert!(matches!(
            compile_task_recipe(&input),
            Err(CompileError::IncompleteAnswerCoverage)
        ));
    }

    #[test]
    fn values_restated_from_the_task_text_do_not_gate_coverage() {
        // The agent's partial-success summary quoted the goal (URL, port,
        // query) and its answer. Only the answer must be response-backed.
        let mut response = trace("GET", "http://127.0.0.1:51430/api/search?q=rust", 1_000);
        response.response_body = Some(
            r#"{"query":"rust","hits":[{"id":7,"title":"Rust 2031 roadmap","points":3119}]}"#
                .into(),
        );
        let input = RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "execution".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Points of the top result for rust".into(),
            task_text: "Open http://127.0.0.1:51430/?q=rust and report the points of the first result.".into(),
            reported: values::collect_reported_values(
                "Goal partially achieved: Open http://127.0.0.1:51430/?q=rust and report the points of the first result.\n\n3119\n\n- Opened the search and verified the first result, \"Rust 2031 roadmap\", has 3119 points.",
                &[],
                &[],
            ),
            traces: vec![response],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        };
        let recipe = compile_task_recipe(&input).expect("task echoes must not block the compile");
        let answers = &recipe.current().unwrap().answer_spec;
        assert!(!answers.is_empty());
        // A genuinely uncovered answer value still fails closed.
        let mut uncovered = input;
        uncovered.reported = values::collect_reported_values("3119 and 4242 comments", &[], &[]);
        assert!(matches!(
            compile_task_recipe(&uncovered),
            Err(CompileError::IncompleteAnswerCoverage)
        ));
    }

    #[test]
    fn a_recipe_answering_the_wrong_field_is_refused() {
        let mut search = trace("GET", "http://127.0.0.1:49244/api/search?q=rust", 1_000);
        search.response_body =
            Some(r#"{"hits":[{"id":7,"title":"Rust 2031 roadmap","points":3119}]}"#.into());
        let mut detail = trace("GET", "http://127.0.0.1:49244/api/items/7", 2_000);
        detail.response_body = Some(
            r#"{"id":7,"title":"Rust 2031 roadmap","author":"Ingrid Solvang","points":3119}"#
                .into(),
        );
        let task_title = "Author of the top result for rust";
        let task_text =
            "Open http://127.0.0.1:49244/?q=rust, open the first result, and report its author.";
        let summary = "Opened the current Rust search results, selected the first result (\u{201c}Rust 2031 roadmap\u{201d}), and read its author as Ingrid Solvang.";
        let input = |reported: values::ReportedValues| RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "execution".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: task_title.into(),
            task_text: task_text.into(),
            reported,
            traces: vec![search.clone(), detail.clone()],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        };
        // Title only (the old extractor's view of this summary): refused.
        let title_only = values::collect_reported_values(summary, &[], &[]);
        assert!(matches!(
            compile_task_recipe(&input(title_only)),
            Err(CompileError::AnswerFieldNotReported { asked }) if asked == vec!["author".to_string()]
        ));
        // With the task-directed author value the chain compiles and the
        // answer is the author from the detail step.
        let directed = values::collect_reported_values_for_task(summary, &[], &[], task_text);
        let recipe = compile_task_recipe(&input(directed)).expect("author recipe compiles");
        let version = recipe.current().unwrap();
        assert_eq!(version.steps.len(), 2, "search then detail");
        assert!(
            version
                .answer_spec
                .iter()
                .any(|answer| answer.field == "author"),
            "{:?}",
            version.answer_spec
        );
    }

    #[test]
    fn a_write_that_echoes_the_created_note_compiles_from_the_agents_quoted_summary() {
        let mut login = trace("POST", "http://127.0.0.1:63975/api/session", 500);
        login.status = 200;
        login.response_body = Some(r#"{"ok":true,"username":"eval"}"#.into());
        let mut list_before = trace("GET", "http://127.0.0.1:63975/api/notes", 1_000);
        list_before.response_body = Some(r#"{"notes":[{"id":1,"text":"water plants"}]}"#.into());
        let mut create = trace("POST", "http://127.0.0.1:63975/api/notes", 2_000);
        create.status = 201;
        create.request_body = Some(r#"{"text":"buy milk"}"#.into());
        create.response_body = Some(r#"{"id":2,"text":"buy milk"}"#.into());
        let mut list_after = trace("GET", "http://127.0.0.1:63975/api/notes", 3_000);
        list_after.response_body =
            Some(r#"{"notes":[{"id":1,"text":"water plants"},{"id":2,"text":"buy milk"}]}"#.into());
        let task_text =
            "Open http://127.0.0.1:63975/login, log in with username eval and password \
                         eval-pass, open the notes page, add a note that says \"buy milk\", and \
                         confirm it appears in the list.";
        // Exactly what the agent reported, curly quotes and all.
        let summary = "Added \u{201c}buy milk.\u{201d} Confirmed the rendered notes list contains \
                       \u{201c}buy milk.\u{201d}";
        let reported = values::collect_reported_values_for_task(summary, &[], &[], task_text);
        assert!(
            reported
                .values
                .iter()
                .any(|value| value.normalized == "buy milk"),
            "the quoted note must be a reported value: {:?}",
            reported.values
        );
        let recipe = compile_task_recipe(&RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "execution".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Add the note buy milk".into(),
            task_text: task_text.into(),
            reported,
            traces: vec![login, list_before, create, list_after],
            typed_inputs: vec!["buy milk".into()],
            trace_files: vec![],
            sequences: vec![],
        })
        .expect("the note-writing recipe compiles");
        let version = recipe.current().unwrap();
        let write = version
            .steps
            .iter()
            .find(|step| step.method == "POST" && step.url_template.ends_with("/api/notes"))
            .unwrap_or_else(|| panic!("the create step survives: {:?}", version.steps));
        // Replay preflight refuses a write that nothing reads back, so the
        // capture's follow-up list read must ride along and be bound to it.
        let verify_id = write
            .verify_with
            .as_deref()
            .unwrap_or_else(|| panic!("the write is verified: {:?}", version.steps));
        let verifier = version
            .steps
            .iter()
            .find(|step| step.id == verify_id)
            .expect("the verification step is in the recipe");
        assert_eq!(verifier.method, "GET");
        assert_eq!(verifier.side_effects, SideEffects::ReadOnly);
    }

    #[test]
    fn quoted_page_chrome_does_not_refuse_a_recipe_that_answers_the_asked_field() {
        let mut orders = trace("GET", "http://127.0.0.1:63974/api/me/orders", 1_000);
        orders.response_body = Some(
            r#"{"orders":[{"id":"ORD-8841","placed_at":"2026-09-10","status":"shipped","total":1284.5}]}"#
                .into(),
        );
        // The page the agent read carries the caption it quotes back; the API
        // does not. Without this document the caption is a claim no response
        // supports, and refusing the recipe would be correct.
        let mut page = trace("GET", "http://127.0.0.1:63974/orders", 500);
        page.response_body = Some(
            "<!doctype html><html><body><h1>Your orders</h1>\
             <p class=\"muted\">Most recent first.</p></body></html>"
                .into(),
        );
        let task_text =
            "Open http://127.0.0.1:63974/login, log in with username eval and password \
                         eval-pass, then open the orders page and report the total of the most \
                         recent order.";
        // The agent quotes the page's own sort-order caption next to the answer.
        let summary = "The orders page states \u{201c}Most recent first\u{201d}; the first listed \
                       order, ORD-8841, has total USD 1284.50.";
        let recipe = compile_task_recipe(&RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "execution".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Total of my most recent order".into(),
            task_text: task_text.into(),
            reported: values::collect_reported_values_for_task(summary, &[], &[], task_text),
            traces: vec![page, orders],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        })
        .expect("page chrome must not refuse the recipe");
        let version = recipe.current().unwrap();
        assert!(
            version
                .answer_spec
                .iter()
                .any(|answer| answer.field == "total"),
            "{:?}",
            version.answer_spec
        );
    }

    #[test]
    fn a_post_whose_body_the_engine_never_captured_refuses_to_compile() {
        // One browser engine's HAR export carries no `postData` at all, so a
        // search POST reached the miner with an empty body and compiled into a
        // recipe the API answered 400. Fail closed instead.
        let mut query = trace("POST", "https://dsn.example/1/indexes/Item/query", 1_000);
        query.request_headers.insert(
            "content-type".into(),
            "application/x-www-form-urlencoded".into(),
        );
        query.request_body = None;
        query.response_body = Some(r#"{"hits":[{"points":3119}]}"#.into());
        let input = RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "execution".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Points of the top result for rust".into(),
            task_text: "Open the search page and report the points of the first result.".into(),
            reported: values::collect_reported_values("points: 3119", &[], &[]),
            traces: vec![query.clone()],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        };
        assert!(matches!(
            compile_task_recipe(&input),
            Err(CompileError::CaptureMissingRequestBody { .. })
        ));
        // With the body captured, the same shape compiles.
        let mut complete = input;
        complete.traces[0].request_body = Some(r#"{"query":"rust"}"#.into());
        assert!(compile_task_recipe(&complete).is_ok());
    }

    #[test]
    fn a_decimal_reported_with_trailing_zeros_matches_the_json_number() {
        let mut orders = trace("GET", "http://127.0.0.1:57450/api/me/orders", 1_000);
        orders.response_body = Some(
            r#"{"username":"eval","orders":[{"currency":"USD","id":"ORD-8841","items":3,"placed_at":"2026-09-10","status":"shipped","total":1284.5},{"currency":"USD","id":"ORD-8790","items":1,"placed_at":"2026-09-02","status":"delivered","total":312.0}]}"#
                .into(),
        );
        let task_text = "Open http://127.0.0.1:57450/login, log in with username eval and password eval-pass, then open the orders page and report the total of the most recent order.";
        let summary = "Logged in to the supplied fixture account. Opened the orders page. Verified the most recent order, ORD-8841 (2026-09-10), has total USD 1284.50.";
        let recipe = compile_task_recipe(&RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "execution".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Total of my most recent order".into(),
            task_text: task_text.into(),
            reported: values::collect_reported_values_for_task(summary, &[], &[], task_text),
            traces: vec![orders],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        })
        .expect("the orders recipe compiles");
        let version = recipe.current().unwrap();
        assert_eq!(version.steps.len(), 1);
        assert!(
            version
                .answer_spec
                .iter()
                .any(|answer| answer.field == "total"),
            "{:?}",
            version.answer_spec
        );
    }

    #[test]
    fn compilation_retains_an_empty_post_that_precedes_its_verification_read() {
        let mut write = trace("POST", "https://example.test/api/profile", 1_000);
        write.status = 204;
        write.request_body = Some(r#"{"name":"Alice"}"#.into());
        write.response_body = None;
        write.response_size = 0;
        let mut read = trace("GET", "https://example.test/api/profile", 2_000);
        read.response_body = Some(r#"{"name":"Alice"}"#.into());
        let mut beacon = trace("POST", "https://example.test/telemetry", 900);
        beacon.status = 204;
        beacon.response_body = None;
        beacon.response_size = 0;
        let recipe = compile_task_recipe(&RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "execution".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Set profile name to Alice".into(),
            task_text: String::new(),
            reported: values::collect_reported_values(
                "",
                &[serde_json::json!({"name":"Alice"})],
                &[],
            ),
            traces: vec![beacon, write, read],
            typed_inputs: vec!["Alice".into()],
            trace_files: vec![],
            sequences: vec![],
        })
        .unwrap();
        let version = recipe.current().unwrap();
        assert_eq!(version.steps.len(), 2);
        assert_eq!(version.steps[0].side_effects, SideEffects::Write);
        assert_eq!(
            version.steps[0].verify_with.as_deref(),
            Some(version.steps[1].id.as_str())
        );
    }

    #[tokio::test]
    async fn compiled_json_and_form_replays_preserve_wire_representation_with_current_inputs() {
        use crate::magician_v2::api_mining::{
            origin_policy::OriginPolicyStore,
            recipe_matcher::{RecipeMatcher, TaskShapeQuery},
            recipe_runner::{
                RecipeRunInputs, RecipeRunner, StepTransport, TransportRequest, TransportResponse,
            },
            recipe_store::RecipeStore,
            replay_grants::ReplayGrantStore,
        };
        struct RepresentationServer {
            form: bool,
        }
        #[async_trait::async_trait]
        impl StepTransport for RepresentationServer {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, request: &TransportRequest) -> Result<TransportResponse, String> {
                // A real representation-sensitive API would reject missing
                // Content-Type or negotiate HTML without the recorded Accept.
                assert_eq!(
                    request.headers.get("accept").map(String::as_str),
                    Some("application/vnd.fixture+json; version=2")
                );
                assert!(!request.headers.contains_key("content-length"));
                assert!(!request.headers.contains_key("host"));
                let (status, body) = if self.form && request.method == "POST" {
                    assert_eq!(
                        request.headers["content-type"],
                        "application/x-www-form-urlencoded; charset=UTF-8"
                    );
                    let pairs: Vec<_> =
                        url::form_urlencoded::parse(request.body.as_ref().unwrap().as_bytes())
                            .collect();
                    assert!(pairs
                        .iter()
                        .any(|(key, value)| key == "name" && value == "Bob"));
                    assert!(pairs
                        .iter()
                        .any(|(key, value)| key == "mode" && value == "replace"));
                    (204, "")
                } else if self.form {
                    assert_eq!(request.method, "GET");
                    (200, r#"{"name":"Bob"}"#)
                } else {
                    assert_eq!(request.method, "POST");
                    assert_eq!(
                        request.headers["content-type"],
                        "application/json; charset=UTF-8"
                    );
                    let body: serde_json::Value =
                        serde_json::from_str(request.body.as_ref().unwrap()).unwrap();
                    assert_eq!(body["variables"]["q"], "red");
                    assert_eq!(body["query"], "query Price($q: String!) { price(q: $q) }");
                    (200, r#"{"price":43}"#)
                };
                Ok(TransportResponse {
                    status,
                    headers: HashMap::new(),
                    body: body.into(),
                })
            }
        }

        for form in [false, true] {
            let (cold_title, warm_title, content_type) = if form {
                (
                    "Set profile name to Alice",
                    "Set profile name to Bob",
                    "application/x-www-form-urlencoded; charset=UTF-8",
                )
            } else {
                ("Find blue", "Find red", "application/json; charset=UTF-8")
            };
            let mut post = trace(
                "POST",
                if form {
                    "https://example.test/api/profile"
                } else {
                    "https://example.test/graphql"
                },
                1_000,
            );
            post.request_headers = HashMap::from([
                ("Content-Type".into(), content_type.into()),
                (
                    "Accept".into(),
                    "application/vnd.fixture+json; version=2".into(),
                ),
                ("Content-Length".into(), "999".into()),
                ("Host".into(), "example.test".into()),
            ]);
            post.request_body = Some(if form { "name=Alice&mode=replace" } else {
                r#"{"query":"query Price($q: String!) { price(q: $q) }","variables":{"q":"blue"}}"#
            }.into());
            post.status = if form { 204 } else { 200 };
            post.response_body = (!form).then(|| r#"{"price":42}"#.into());
            let mut traces = vec![post];
            if form {
                let mut get = trace("GET", "https://example.test/api/profile", 2_000);
                get.request_headers.insert(
                    "Accept".into(),
                    "application/vnd.fixture+json; version=2".into(),
                );
                get.response_body = Some(r#"{"name":"Alice"}"#.into());
                traces.push(get);
            }
            let recipe = compile_task_recipe(&RecipeCompileInput {
                task_id: "task".into(),
                execution_id: "cold".into(),
                monitor_revision: None,
                agent_id: "agent".into(),
                principal: "owner".into(),
                workspace: "default".into(),
                task_title: cold_title.into(),
                task_text: String::new(),
                reported: values::collect_reported_values(
                    "",
                    &[if form {
                        serde_json::json!({"name":"Alice"})
                    } else {
                        serde_json::json!({"price":42})
                    }],
                    &[],
                ),
                traces,
                typed_inputs: Vec::new(),
                trace_files: Vec::new(),
                sequences: Vec::new(),
            })
            .unwrap();
            assert_eq!(recipe.has_write_steps(), form);
            let temp = tempfile::tempdir().unwrap();
            let store = RecipeStore::new(temp.path().to_path_buf());
            store.save_and_bind(&recipe, "task").unwrap();
            let matched = RecipeMatcher::deterministic(&store)
                .find(&TaskShapeQuery {
                    task_id: "task",
                    title: warm_title,
                    description: "",
                    agent_id: "agent",
                    principal: "owner",
                    workspace: "default",
                })
                .await
                .unwrap();
            let mut recipe = matched.recipe;
            let approved_write_steps = recipe
                .current()
                .unwrap()
                .steps
                .iter()
                .filter(|step| step.side_effects == SideEffects::Write)
                .map(|step| step.id.clone())
                .collect();
            let grants = ReplayGrantStore::open(temp.path());
            let policy = OriginPolicyStore::open(temp.path());
            let result = RecipeRunner {
                transports: vec![Box::new(RepresentationServer { form })],
                grants: &grants,
                origin_policy: &policy,
                can_continue: None,
                session_lookup: &|_, _| None,
                auth_healer: None,
                max_auth_heals: 0,
                step_feedback: None,
                observer: None,
            }
            .run(
                &mut recipe,
                &RecipeRunInputs {
                    inputs: matched.inputs,
                    approved_write_steps,
                    timeout_ms: None,
                },
            )
            .await;
            assert!(result.success, "{result:?}");
            assert_eq!(
                serde_json::Value::Object(result.answer),
                if form {
                    serde_json::json!({"name":"Bob"})
                } else {
                    serde_json::json!({"price":43})
                }
            );
        }
    }

    #[tokio::test]
    async fn cold_learning_to_warm_replay_preserves_equal_zero_and_boolean_answers() {
        use crate::magician_v2::api_mining::{
            origin_policy::OriginPolicyStore,
            recipe_matcher::{MatchKind, RecipeMatcher, TaskShapeQuery},
            recipe_runner::{
                RecipeRunInputs, RecipeRunner, StepTransport, TransportRequest, TransportResponse,
            },
            recipe_store::RecipeStore,
            replay_grants::ReplayGrantStore,
        };
        let mut buy = trace("GET", "https://example.test/buy?q=blue", 1_000);
        buy.response_body = Some(r#"{"buy":42}"#.into());
        let mut sell = trace("GET", "https://example.test/sell?q=blue", 2_000);
        sell.response_body = Some(r#"{"sell":42,"count":0,"enabled":false}"#.into());
        let recipe = compile_task_recipe(&RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "cold".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Compare blue".into(),
            task_text: "Return prices.\nInclude count and enabled.".into(),
            reported: values::collect_reported_values(
                "",
                &[serde_json::json!({
                    "buy": 42, "sell": 42, "count": 0, "enabled": false
                })],
                &[],
            ),
            traces: vec![buy, sell],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        })
        .unwrap();
        assert_eq!(recipe.current().unwrap().steps.len(), 2);
        assert_eq!(recipe.current().unwrap().answer_spec.len(), 4);
        let temp = tempfile::tempdir().unwrap();
        let store = RecipeStore::new(temp.path().to_path_buf());
        store.save_and_bind(&recipe, "task").unwrap();
        let matched = RecipeMatcher::deterministic(&store)
            .find(&TaskShapeQuery {
                task_id: "task",
                title: "Compare red",
                description: "Return prices.\n\nInclude count and enabled.",
                agent_id: "agent",
                principal: "owner",
                workspace: "default",
            })
            .await
            .unwrap();
        assert_eq!(matched.kind, MatchKind::TaskId);
        assert!(matched.inputs.values().all(|value| value == "red"));
        struct CurrentResponses;
        #[async_trait::async_trait]
        impl StepTransport for CurrentResponses {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, request: &TransportRequest) -> Result<TransportResponse, String> {
                let url = url::Url::parse(&request.url).unwrap();
                assert!(url
                    .query_pairs()
                    .any(|(key, value)| key == "q" && value == "red"));
                let body = match url.path() {
                    "/buy" => r#"{"buy":41}"#,
                    "/sell" => r#"{"sell":43,"count":1,"enabled":true}"#,
                    other => panic!("unexpected request path {other}"),
                };
                Ok(TransportResponse {
                    status: 200,
                    headers: HashMap::new(),
                    body: body.into(),
                })
            }
        }
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = matched.recipe;
        let result = RecipeRunner {
            transports: vec![Box::new(CurrentResponses)],
            grants: &grants,
            origin_policy: &policy,
            can_continue: None,
            session_lookup: &|_, _| None,
            auth_healer: None,
            max_auth_heals: 0,
            step_feedback: None,
            observer: None,
        }
        .run(
            &mut recipe,
            &RecipeRunInputs {
                inputs: matched.inputs,
                ..Default::default()
            },
        )
        .await;
        assert!(result.success, "{result:?}");
        assert_eq!(
            serde_json::Value::Object(result.answer),
            serde_json::json!({
                "buy": 41, "sell": 43, "count": 1, "enabled": true
            })
        );
    }

    #[tokio::test]
    async fn shared_request_inputs_replay_only_when_repeated_task_values_agree() {
        use crate::magician_v2::api_mining::{
            origin_policy::OriginPolicyStore,
            recipe_matcher::{eligible_for_fuzzy, RecipeMatcher, TaskShapeQuery},
            recipe_runner::{
                RecipeRunInputs, RecipeRunner, StepTransport, TransportRequest, TransportResponse,
            },
            recipe_store::RecipeStore,
            replay_grants::ReplayGrantStore,
        };
        let mut buy = trace("GET", "https://example.test/buy?q=BTC", 1_000);
        buy.response_body = Some(r#"{"buy":42}"#.into());
        let mut sell = trace("GET", "https://example.test/sell?q=BTC", 2_000);
        sell.response_body = Some(r#"{"sell":43}"#.into());
        let recipe = compile_task_recipe(&RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "cold".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Compare BTC with BTC".into(),
            task_text: "Return both prices".into(),
            reported: values::collect_reported_values(
                "",
                &[serde_json::json!({"buy": 42, "sell": 43})],
                &[],
            ),
            traces: vec![buy, sell],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        })
        .unwrap();
        assert_eq!(recipe.shape.inputs.len(), 1);
        assert_eq!(recipe.shape.template, "compare {q} with {q}");
        assert_eq!(recipe.current().unwrap().steps.len(), 2);
        assert!(!eligible_for_fuzzy(&recipe));
        let mut single = recipe.clone();
        single.shape.template = "compare {q}".into();
        assert!(eligible_for_fuzzy(&single));
        single.shape.description_template = Some("also compare {q}".into());
        assert!(!eligible_for_fuzzy(&single));
        single.shape.template = "compare prices".into();
        single.shape.description_template = Some("compare {q} with {q}".into());
        assert!(!eligible_for_fuzzy(&single));

        let temp = tempfile::tempdir().unwrap();
        let store = RecipeStore::new(temp.path().to_path_buf());
        store.save_and_bind(&recipe, "task").unwrap();
        let matcher = RecipeMatcher::deterministic(&store);
        for task_id in ["task", "new-task"] {
            assert!(matcher
                .find(&TaskShapeQuery {
                    task_id,
                    title: "Compare ETH with BTC",
                    description: "Return both prices",
                    agent_id: "agent",
                    principal: "owner",
                    workspace: "default",
                })
                .await
                .is_none());
        }
        let matched = matcher
            .find(&TaskShapeQuery {
                task_id: "new-task",
                title: "Compare ETH with ETH",
                description: "Return both prices",
                agent_id: "agent",
                principal: "owner",
                workspace: "default",
            })
            .await
            .unwrap();
        assert_eq!(matched.inputs["q"], "ETH");
        struct CurrentResponses;
        #[async_trait::async_trait]
        impl StepTransport for CurrentResponses {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, request: &TransportRequest) -> Result<TransportResponse, String> {
                let url = url::Url::parse(&request.url).unwrap();
                assert!(url
                    .query_pairs()
                    .any(|(key, value)| key == "q" && value == "ETH"));
                let body = match url.path() {
                    "/buy" => r#"{"buy":51}"#,
                    "/sell" => r#"{"sell":52}"#,
                    other => panic!("unexpected request path {other}"),
                };
                Ok(TransportResponse {
                    status: 200,
                    headers: HashMap::new(),
                    body: body.into(),
                })
            }
        }
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut replay = matched.recipe;
        let result = RecipeRunner {
            transports: vec![Box::new(CurrentResponses)],
            grants: &grants,
            origin_policy: &policy,
            can_continue: None,
            session_lookup: &|_, _| None,
            auth_healer: None,
            max_auth_heals: 0,
            step_feedback: None,
            observer: None,
        }
        .run(
            &mut replay,
            &RecipeRunInputs {
                inputs: matched.inputs,
                ..Default::default()
            },
        )
        .await;
        assert!(result.success, "{result:?}");
        assert_eq!(result.steps.len(), 2);
        assert_eq!(
            serde_json::Value::Object(result.answer),
            serde_json::json!({"buy": 51, "sell": 52})
        );
    }

    #[tokio::test]
    async fn learned_headers_replay_placeholder_shaped_inputs_as_literal_values() {
        use crate::magician_v2::api_mining::{
            origin_policy::OriginPolicyStore,
            recipe_matcher::{RecipeMatcher, TaskShapeQuery},
            recipe_runner::{
                RecipeRunInputs, RecipeRunner, StepTransport, TransportRequest, TransportResponse,
            },
            recipe_store::RecipeStore,
            replay_grants::ReplayGrantStore,
        };
        let mut response = trace("GET", "https://example.test/api/compare", 1_000);
        response.request_headers = HashMap::from([
            ("x-left".into(), "blue".into()),
            ("x-right".into(), "red".into()),
        ]);
        response.response_body = Some(r#"{"result":"cold result"}"#.into());
        let recipe = compile_task_recipe(&RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "cold".into(),
            monitor_revision: None,
            agent_id: "agent".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "Compare blue with red".into(),
            task_text: "Return the comparison".into(),
            reported: values::collect_reported_values(
                "",
                &[serde_json::json!({"result":"cold result"})],
                &[],
            ),
            traces: vec![response],
            typed_inputs: vec![],
            trace_files: vec![],
            sequences: vec![],
        })
        .unwrap();
        assert_eq!(recipe.shape.template, "compare {x_left} with {x_right}");
        let temp = tempfile::tempdir().unwrap();
        let store = RecipeStore::new(temp.path().to_path_buf());
        store.save_and_bind(&recipe, "task").unwrap();
        let matched = RecipeMatcher::deterministic(&store)
            .find(&TaskShapeQuery {
                task_id: "task",
                title: "Compare {x_right} with {x_left}",
                description: "Return the comparison",
                agent_id: "agent",
                principal: "owner",
                workspace: "default",
            })
            .await
            .unwrap();
        struct ExactHeaders;
        #[async_trait::async_trait]
        impl StepTransport for ExactHeaders {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, request: &TransportRequest) -> Result<TransportResponse, String> {
                assert_eq!(request.headers["x-left"], "{x_right}");
                assert_eq!(request.headers["x-right"], "{x_left}");
                Ok(TransportResponse {
                    status: 200,
                    headers: HashMap::new(),
                    body: r#"{"result":"warm result"}"#.into(),
                })
            }
        }
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut replay = matched.recipe;
        let result = RecipeRunner {
            transports: vec![Box::new(ExactHeaders)],
            grants: &grants,
            origin_policy: &policy,
            can_continue: None,
            session_lookup: &|_, _| None,
            auth_healer: None,
            max_auth_heals: 0,
            step_feedback: None,
            observer: None,
        }
        .run(
            &mut replay,
            &RecipeRunInputs {
                inputs: matched.inputs,
                ..Default::default()
            },
        )
        .await;
        assert!(result.success, "{result:?}");
        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.answer["result"], "warm result");
    }

    #[test]
    fn static_verification_read_pulls_in_a_nearby_same_resource_write() {
        let traces = vec![
            trace("PATCH", "https://example.test/api/profile", 1_000),
            trace("POST", "https://example.test/api/unrelated", 1_001),
            trace("GET", "https://example.test/api/profile", 2_000),
        ];

        assert_eq!(causal_write_targets(&traces, &[2]), vec![0]);
    }

    #[test]
    fn causal_write_inference_is_time_bounded() {
        let traces = vec![
            trace("PATCH", "https://example.test/api/profile", 1_000),
            trace("GET", "https://example.test/api/profile", 20_001),
        ];

        assert!(causal_write_targets(&traces, &[1]).is_empty());
    }

    #[test]
    fn causal_write_inference_does_not_group_unrelated_api_v1_resources() {
        let traces = vec![
            trace("PATCH", "https://example.test/api/v1/profile", 1_000),
            trace("GET", "https://example.test/api/v1/orders", 2_000),
        ];

        assert!(causal_write_targets(&traces, &[1]).is_empty());
    }

    #[test]
    fn resource_family_allows_collection_to_dynamic_item_verification() {
        let collection = resource_family("https://example.test/api/v1/items").unwrap();
        let item = resource_family("https://example.test/api/v1/items/12345").unwrap();
        let different_item = resource_family("https://example.test/api/v1/items/67890").unwrap();
        let other = resource_family("https://example.test/api/v1/items/export").unwrap();

        assert!(same_resource_family(&collection, &item));
        assert!(!same_resource_family(&item, &different_item));
        assert!(!same_resource_family(&collection, &other));
    }

    #[test]
    fn browser_fallback_arguments_retain_shape_without_values() {
        let shaped = browser_argument_shape(&serde_json::json!({
            "selector": "#password",
            "text": "private-value",
            "flags": [true, 7]
        }));

        assert_eq!(
            shaped,
            serde_json::json!({
                "selector": "<redacted>",
                "text": "<redacted>",
                "flags": [false, 0]
            })
        );
        assert!(!shaped.to_string().contains("private-value"));
    }
}
