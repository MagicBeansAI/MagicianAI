//! Owner admission and inspection of generic local text improvement runs.
use std::collections::BTreeMap;

use actix_web::{
    http::header::{CACHE_CONTROL, X_CONTENT_TYPE_OPTIONS},
    web, HttpMessage, HttpRequest, HttpResponse,
};
use anyhow::{Context, Result};
use chrono::Utc;
use magician::magician_v2::{
    api_scope::ResolvedScope,
    artifact_v2::workspace::scope_hosts_user_subsystems,
    cloudflare_access::VerifiedRequestIdentity,
    relentless::store::{RunEvent, RunScope, RunState, RunStatus},
    relentless_runtime::{
        text_profile_catalog, ExistingTaskAdmission, PreparedTextDraft, TextDraftProposal,
    },
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

pub use magician::magician_v2::relentless_service::RelentlessService as RelentlessApi;
#[path = "relentless_catalog.rs"]
mod catalog;

pub(crate) fn owner_scope(req: &HttpRequest) -> std::result::Result<RunScope, HttpResponse> {
    let identity = req
        .extensions()
        .get::<VerifiedRequestIdentity>()
        .cloned()
        .ok_or_else(|| {
            HttpResponse::Unauthorized().json(json!({"error":"Verified owner session required"}))
        })?;
    let now = Utc::now();
    if now < identity.verified_at() || now - identity.verified_at() > chrono::Duration::seconds(5) {
        return Err(
            HttpResponse::Unauthorized().json(json!({"error":"Owner request identity expired"}))
        );
    }
    let asserted = |name| req.headers().get(name).and_then(|v| v.to_str().ok());
    if asserted("X-Principal").is_some_and(|p| p != identity.principal()) {
        return Err(HttpResponse::Forbidden().json(json!({"error":"Owner scope mismatch"})));
    }
    let workspace = identity.workspace().ok_or_else(|| {
        HttpResponse::BadRequest().json(json!({"error":"Verified workspace binding required"}))
    })?;
    if asserted("X-Workspace").is_some_and(|w| w != workspace)
        || !scope_hosts_user_subsystems(identity.principal(), workspace)
    {
        return Err(HttpResponse::Forbidden().json(json!({"error":"Owner scope mismatch"})));
    }
    let resolved = ResolvedScope::from_labels(identity.principal(), workspace)?;
    Ok(RunScope {
        principal: resolved.principal().into(),
        workspace: resolved.workspace().into(),
    })
}

fn failure(error: anyhow::Error) -> HttpResponse {
    HttpResponse::Conflict().json(json!({"error": format!("{error:#}")}))
}

fn run_view(state: &RunState) -> Result<serde_json::Value> {
    let mut view = serde_json::to_value(state)?;
    view["spend"] = serde_json::to_value(state.budget.spend()?)?;
    view["contract_hash"] = json!(state.contract.hash()?);
    view["effective_budget"] = json!(state.effective_budget()?);
    view["budget_usage"] = state.budget_usage(Utc::now())?;
    if state.status == RunStatus::BudgetExhausted {
        view["exhausted_limits"] = json!(state.exhausted_limits(Utc::now())?);
    }
    Ok(view)
}

fn run_response(state: &RunState, accepted: bool) -> HttpResponse {
    match run_view(state) {
        Ok(view) if accepted => HttpResponse::Accepted().json(view),
        Ok(view) => HttpResponse::Ok().json(view),
        Err(error) => failure(error),
    }
}

async fn io<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .context("Relentless storage worker")?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    run_id: Uuid,
    proposal: TextDraftProposal,
    ui_thread_id: String,
    agent_id: String,
    expected_contract_hash: String,
    #[serde(default)]
    proposal_request_id: Option<String>,
    #[serde(default)]
    existing_task: Option<ExistingTaskAdmission>,
    #[serde(default)]
    capture_handoff:
        Option<magician::magician_v2::approval_envelopes::campaign::CaptureHandoffTarget>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeRequest {
    attention_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerReviewRequest {
    artifact_hash: String,
    approved: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetResetRequest {
    reset_id: Uuid,
    expected_generation: u64,
    expected_contract_hash: String,
    reset: magician::magician_v2::relentless::budget_round::BudgetReset,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskPolicyRequest {
    policy_id: Uuid,
    expected_policy_id: Option<Uuid>,
    expected_context_hash: String,
    expected_contract_hash: String,
    proposal: Option<TextDraftProposal>,
    max_runs: u32,
    expires_at: chrono::DateTime<Utc>,
    #[serde(default)]
    capture_handoff:
        Option<magician::magician_v2::approval_envelopes::campaign::CaptureHandoffTarget>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignRequest {
    expected_version: Option<String>,
    spec: Option<magician::magician_v2::approval_envelopes::campaign::CampaignSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignPreviewRequest {
    spec: magician::magician_v2::approval_envelopes::campaign::CampaignSpec,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuidancePreviewRequest {
    policy_id: Uuid,
    spec: magician::magician_v2::learning::relentless_promotion::GuidancePromotionSpec,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuidanceActivationRequest {
    expected_version_hash: String,
    spec: magician::magician_v2::learning::relentless_promotion::GuidancePromotionSpec,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuidanceVersionRequest {
    expected_version_hash: String,
}

fn guidance_view(
    state: &magician::magician_v2::learning::relentless_promotion::GuidancePolicyState,
) -> Result<serde_json::Value> {
    let mut view = serde_json::to_value(state)?;
    view["evaluation_spend"] = serde_json::to_value(state.evaluation_budget.spend()?)?;
    view["canary_spend"] = serde_json::to_value(state.canary_budget.spend()?)?;
    view["adoption_enabled"] = json!(state.adoption_enabled());
    view["expired"] = json!(state.policy.spec.expires_at <= Utc::now());
    Ok(view)
}
async fn guidance_preview(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    body: web::Json<GuidancePreviewRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let body = body.into_inner();
    match api
        .preview_guidance_policy(scope, body.policy_id, body.spec)
        .await
    {
        Ok(policy) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store"))
            .json(json!({"policy":policy,"authority_activated":false})),
        Err(error) => failure(error),
    }
}
async fn guidance_list(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    query: web::Query<catalog::PageQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let params = match query.params() {
        Ok(p) => p,
        Err(r) => return r,
    };
    match api
        .owner_guidance_policies(&scope)
        .await
        .and_then(|mut states| {
            states.retain(|s| {
                catalog::matches(
                    query.search(),
                    &[
                        &s.policy.spec.name,
                        &s.policy.guidance.title,
                        &s.policy.id.to_string(),
                        serde_json::to_value(s.status)
                            .ok()
                            .as_ref()
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(""),
                    ],
                )
            });
            states.sort_by(|a, b| {
                a.policy
                    .spec
                    .name
                    .to_lowercase()
                    .cmp(&b.policy.spec.name.to_lowercase())
                    .then_with(|| a.policy.id.cmp(&b.policy.id))
            });
            let (states, pagination) = catalog::page(states, &params);
            Ok((
                states
                    .iter()
                    .map(guidance_view)
                    .collect::<Result<Vec<_>>>()?,
                pagination,
            ))
        }) {
        Ok((policies, pagination)) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store"))
            .json(json!({"policies":policies,"pagination":pagination})),
        Err(error) => failure(error),
    }
}
async fn guidance_get(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let id = id.into_inner();
    match api
        .owner_guidance_policies(&scope)
        .await
        .and_then(|states| {
            guidance_view(
                &states
                    .into_iter()
                    .find(|state| state.policy.id == id)
                    .context("guidance policy missing")?,
            )
        }) {
        Ok(view) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store"))
            .json(view),
        Err(error) => failure(error),
    }
}
async fn guidance_proposals(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    query: web::Query<catalog::PageQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let params = match query.params() {
        Ok(p) => p,
        Err(r) => return r,
    };
    match api.pending_guidance_proposals(scope).await {
        Ok(mut proposals) => {
            proposals.retain(|p| {
                catalog::matches(
                    query.search(),
                    &[
                        p["guidance"]["title"].as_str().unwrap_or(""),
                        p["candidate_id"].as_str().unwrap_or(""),
                    ],
                )
            });
            proposals.sort_by(|a, b| a["candidate_id"].as_str().cmp(&b["candidate_id"].as_str()));
            let (proposals, pagination) = catalog::page(proposals, &params);
            HttpResponse::Ok()
                .insert_header((CACHE_CONTROL, "no-store"))
                .json(json!({"proposals":proposals,"pagination":pagination}))
        },
        Err(error) => failure(error),
    }
}
async fn guidance_evidence(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (id, run_id) = path.into_inner();
    match api.owner_guidance_evidence(scope, id, run_id).await {
        Ok(evidence) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store"))
            .json(evidence),
        Err(error) => failure(error),
    }
}
async fn guidance_activate(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
    body: web::Json<GuidanceActivationRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let body = body.into_inner();
    match api
        .activate_guidance_policy(
            scope,
            id.into_inner(),
            body.spec,
            &body.expected_version_hash,
        )
        .await
        .and_then(|state| guidance_view(&state))
    {
        Ok(view) => HttpResponse::Accepted()
            .insert_header(("Cache-Control", "no-store"))
            .json(view),
        Err(error) => failure(error),
    }
}
async fn guidance_revoke(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
    body: web::Json<GuidanceVersionRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .revoke_guidance_policy(
            scope,
            id.into_inner(),
            body.into_inner().expected_version_hash,
        )
        .await
        .and_then(|state| guidance_view(&state))
    {
        Ok(view) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store"))
            .json(view),
        Err(error) => failure(error),
    }
}

async fn guidance_resume(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
    body: web::Json<GuidanceVersionRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .resume_guidance_policy(
            scope,
            id.into_inner(),
            body.into_inner().expected_version_hash,
        )
        .await
        .and_then(|state| guidance_view(&state))
    {
        Ok(view) => HttpResponse::Accepted()
            .insert_header(("Cache-Control", "no-store"))
            .json(view),
        Err(error) => failure(error),
    }
}

async fn campaign_preview(
    req: HttpRequest,
    body: web::Json<CampaignPreviewRequest>,
) -> HttpResponse {
    if let Err(response) = owner_scope(&req) {
        return response;
    }
    let mut spec = body.into_inner().spec;
    let resolved = (|| -> Result<()> {
        if let Some(template) = &spec.content_template {
            let hash = template.hash()?;
            anyhow::ensure!(
                spec.content_contract_hash.is_empty() || spec.content_contract_hash == hash,
                "content hash does not match the approved template"
            );
            spec.content_contract_hash = hash;
        }
        spec.validate()
    })();
    match resolved {
        Ok(()) => HttpResponse::Ok()
            .json(json!({"spec":spec,"authority_activated":false,"live_dispatch_enabled":false})),
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignCaptureRequest {
    expected_version: String,
    run_id: Uuid,
    artifact_hash: String,
    identity_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignReviewRequest {
    binding_hash: String,
    approved: bool,
}

async fn campaigns(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    query: web::Query<catalog::PageQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let params = match query.params() {
        Ok(p) => p,
        Err(r) => return r,
    };
    match api.owner_campaigns(&scope, &params, query.search()).await {
        Ok(view) => HttpResponse::Ok().json(view),
        Err(error) => failure(error),
    }
}

async fn campaign_configure(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
    body: web::Json<CampaignRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let body = body.into_inner();
    match api
        .configure_owner_campaign(scope, id.into_inner(), body.expected_version, body.spec)
        .await
    {
        Ok(view) => HttpResponse::Ok().json(view),
        Err(error) => failure(error),
    }
}

async fn campaign_capture(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
    body: web::Json<CampaignCaptureRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let body = body.into_inner();
    match api
        .capture_owner_campaign_run(
            scope,
            id.into_inner(),
            body.expected_version,
            body.run_id,
            body.artifact_hash,
            body.identity_id,
        )
        .await
    {
        Ok(view) => HttpResponse::Ok().json(view),
        Err(error) => failure(error),
    }
}

async fn campaign_review(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    path: web::Path<(Uuid, String)>,
    body: web::Json<CampaignReviewRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (campaign_id, id) = path.into_inner();
    let body = body.into_inner();
    match api
        .review_owner_campaign_capture(scope, campaign_id, id, body.binding_hash, body.approved)
        .await
    {
        Ok(view) => HttpResponse::Ok().json(view),
        Err(error) => failure(error),
    }
}

async fn work(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    query: web::Query<catalog::PageQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let params = match query.params() {
        Ok(p) => p,
        Err(r) => return r,
    };
    match api
        .owner_work_view(&scope, query.goal_id.as_deref(), &params, query.search())
        .await
    {
        Ok(view) => HttpResponse::Ok().json(view),
        Err(error) => failure(error),
    }
}

async fn task_policy(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    task_id: web::Path<String>,
    body: web::Json<TaskPolicyRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .configure_owner_task_policy_with_capture(
            scope,
            &task_id,
            &body.expected_context_hash,
            body.policy_id,
            body.expected_policy_id,
            body.proposal.clone(),
            &body.expected_contract_hash,
            body.max_runs,
            body.expires_at,
            body.capture_handoff.clone(),
        )
        .await
    {
        Ok(task) => HttpResponse::Ok().json(task),
        Err(error) => failure(error),
    }
}

async fn preview(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    proposal: web::Json<TextDraftProposal>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api.prepare_owner_draft(&scope, &proposal) {
        Ok(p) => {
            let mut resolved = proposal.into_inner();
            resolved.native_harness = p.setup.native_harness.clone();
            let quote = PreparedTextDraft::maximum_attempt_microusd(api.router(), &resolved);
            match quote { Ok(quote) => match p.contract.hash() {
                Ok(hash) => HttpResponse::Ok().json(json!({"contract": p.contract, "contract_hash":hash, "maximum_attempt_microusd": quote, "budget_can_admit_attempt": quote <= resolved.budget.max_cost_microusd, "native_harness":p.setup.native_harness,"artifact_review":p.setup.artifact_review,"resolved_specifications":p.setup.specifications,"delivery":"local_draft", "communication":"capture_only"})), Err(e) => failure(e)
            }, Err(e) => failure(e) }
        },
        Err(e) => failure(e),
    }
}

async fn profiles(req: HttpRequest, api: web::Data<RelentlessApi>) -> HttpResponse {
    if let Err(response) = owner_scope(&req) {
        return response;
    }
    match text_profile_catalog(api.router()) {
        Ok(catalog) => HttpResponse::Ok().json(catalog),
        Err(error) => failure(error),
    }
}

async fn start(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    body: web::Json<StartRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match api
        .start_owner_text_draft_with_capture(
            scope,
            body.run_id,
            &body.proposal,
            &body.ui_thread_id,
            &body.agent_id,
            &body.expected_contract_hash,
            body.proposal_request_id.as_deref(),
            body.existing_task.as_ref(),
            body.capture_handoff.as_ref(),
        )
        .await
    {
        Ok((state, admitted)) => run_response(&state, admitted),
        Err(error) => failure(error),
    }
}

async fn task_context(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    task_id: web::Path<String>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api.owner_task_context(&scope, &task_id).await {
        Ok(context) => HttpResponse::Ok().json(context),
        Err(error) => failure(error),
    }
}

async fn proposals(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    query: web::Query<catalog::PageQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let params = match query.params() {
        Ok(p) => p,
        Err(r) => return r,
    };
    match api.pending_proposals(&scope).await {
        Ok(mut proposals) => {
            proposals.retain(|p| {
                query
                    .agent_id
                    .as_deref()
                    .is_none_or(|id| id == p.origin.agent_id)
                    && catalog::matches(
                        query.search(),
                        &[
                            &p.proposal.goal,
                            &p.origin.agent_id,
                            &p.origin.task_id,
                            &p.origin.request_id,
                        ],
                    )
            });
            proposals.sort_by(|a, b| a.origin.request_id.cmp(&b.origin.request_id));
            let (proposals, pagination) = catalog::page(proposals, &params);
            HttpResponse::Ok()
                .insert_header((CACHE_CONTROL, "no-store"))
                .json(json!({"proposals":proposals,"pagination":pagination}))
        },
        Err(error) => failure(error),
    }
}

async fn list(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    query: web::Query<catalog::PageQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let params = match query.params() {
        Ok(p) => p,
        Err(r) => return r,
    };
    let s = api.store(&scope);
    match io(move || s.list()).await {
        Ok(mut runs) => {
            runs.retain(|r| {
                catalog::matches(
                    query.search(),
                    &[
                        &r.contract.definition().goal,
                        &r.id.to_string(),
                        &r.admission.task_id,
                    ],
                )
            });
            let (runs, pagination) = catalog::page(runs, &params);
            let mut views = Vec::new();
            for run in &runs {
                match run_view_with_handoff(&api, run).await {
                    Ok(view) => views.push(view),
                    Err(error) => return failure(error),
                }
            }
            HttpResponse::Ok().json(json!({"runs": views,"pagination":pagination}))
        },
        Err(e) => failure(e),
    }
}

async fn get(req: HttpRequest, api: web::Data<RelentlessApi>, id: web::Path<Uuid>) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let s = api.store(&scope);
    let id = *id;
    match io(move || s.load(id)).await {
        Ok(state) => match run_view_with_handoff(&api, &state).await {
            Ok(view) => HttpResponse::Ok().json(view),
            Err(error) => failure(error),
        },
        Err(e) => failure(e),
    }
}

async fn run_view_with_handoff(api: &RelentlessApi, state: &RunState) -> Result<serde_json::Value> {
    let mut view = run_view(state)?;
    if state.status == RunStatus::BudgetExhausted {
        view["budget_continuation"] = match api.budget_continuation_view(state).await {
            Ok(view) => view,
            Err(error) => json!({"available":false,"reason":format!("{error:#}")}),
        };
    }
    view["capture_handoff"] = match api.capture_handoff_view(state).await {
        Ok(handoff) => handoff,
        Err(error) => json!({"available":false,"unavailable_reason":format!("{error:#}")}),
    };
    Ok(view)
}

async fn reconcile_costs(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api.reconcile_owner_costs(scope, id.into_inner()).await {
        Ok(state) => run_response(&state, false),
        Err(error) => failure(error),
    }
}

async fn report(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api.run_report(&scope, *id).await {
        Ok(report) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store"))
            .json(report),
        Err(error) => failure(error),
    }
}

async fn lessons(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api.run_lessons(&scope, *id).await {
        Ok(lessons) => HttpResponse::Ok().json(lessons),
        Err(error) => failure(error),
    }
}

async fn artifact_evidence(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = api.store(&scope);
    let (id, hash) = path.into_inner();
    match io(move || {
        use magician::magician_v2::{relentless_artifact::ArtifactPresentation, relentless_runtime::TextDraftSetup};
        let state = store.load(id)?;
        let artifact = state.attempts.iter().filter_map(|attempt| attempt.artifact.as_ref()).find(|artifact| artifact.hash == hash).context("Artifact is not part of this run")?;
        let setup:TextDraftSetup = serde_json::from_slice(&store.read_blob(id, &state.admission.source_hash)?)?;
        // Original references stay bound to their admitted bytes. Do not use
        // mutable note paths or the current owner-input epoch for old artifacts.
        let admitted_sources = state.contract.definition().sources.iter().map(|pin| {
            let bytes = store.read_blob(id, &pin.content_hash)?;
            let byte_length = bytes.len();
            Ok(json!({"source_id":pin.id,"content_hash":pin.content_hash,
                "byte_length":byte_length,"text":String::from_utf8(bytes).ok()}))
        }).collect::<Result<Vec<_>>>()?;
        let collected_sources = artifact.collected_sources.iter().map(|(source_id, content_hash)|
            Ok(json!({"source_id":source_id,"content_hash":content_hash,"text":String::from_utf8(store.read_blob(id, content_hash)?)?}))).collect::<Result<Vec<_>>>()?;
        let mut audio = Vec::new(); let mut code = None;
        if let Some(recipe) = &setup.artifact_review {
            if let Some(verification) = &recipe.verification { code = magician::magician_v2::relentless_code::sealed_evidence(verification, &hash, &store, id)?; }
            for (kind, file, presentation) in recipe.files.iter().map(|(file, presentation)| ("artifact", file, presentation))
                .chain(recipe.sources.iter().map(|(file, presentation)| ("source", file, presentation))) {
                let audio_recipe = match presentation { ArtifactPresentation::Audio { audio, .. } | ArtifactPresentation::Video { audio:Some(audio), .. } => audio, _ => continue };
                let content_hash = if kind == "artifact" { artifact.files.get(file).context("Audio artifact missing")? }
                    else { &state.contract.definition().sources.iter().find(|pin| &pin.id == file).context("Audio source pin missing")?.content_hash };
                let bytes = store.read_blob(id, content_hash)?;
                audio.push(magician::magician_v2::relentless_audio::sealed_evidence(audio_recipe, &bytes, &hash, &format!("{kind}:{file}"), &store, id)?);
            }
        }
        let evidence = json!({"run_id":id,"artifact_hash":hash,"admitted_sources":admitted_sources,"collected_sources":collected_sources,"code":code,"audio":audio});
        let content_hash = blake3::hash(&serde_json::to_vec(&evidence)?).to_hex().to_string();
        Ok(json!({"evidence":evidence,"content_hash":content_hash,"sealed":true}))
    }).await {
        Ok(evidence) => HttpResponse::Ok().insert_header((CACHE_CONTROL, "no-store"))
            .insert_header((X_CONTENT_TYPE_OPTIONS, "nosniff")).json(evidence), Err(error) => failure(error),
    }
}

async fn artifact(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let s = api.store(&scope);
    let (id, hash) = path.into_inner();
    match io(move || {
        let state = s.load(id)?;
        let manifest = state
            .attempts
            .iter()
            .filter_map(|a| a.artifact.as_ref())
            .find(|a| a.hash == hash)
            .context("Artifact is not part of this run")?;
        let setup: magician::magician_v2::relentless_runtime::TextDraftSetup =
            serde_json::from_slice(&s.read_blob(id, &state.admission.source_hash)?)?;
        let mut files = BTreeMap::new();
        let mut binary_files = BTreeMap::new();
        for (name, hash) in &manifest.files {
            let bytes = s.read_blob(id, hash)?;
            match setup
                .artifact_review
                .as_ref()
                .and_then(|recipe| recipe.files.get(name))
            {
                Some(
                    magician::magician_v2::relentless_artifact::ArtifactPresentation::Image {
                        media_type,
                    }
                    | magician::magician_v2::relentless_artifact::ArtifactPresentation::Video {
                        media_type,
                        ..
                    }
                    | magician::magician_v2::relentless_artifact::ArtifactPresentation::Audio {
                        media_type,
                        ..
                    },
                ) => {
                    binary_files.insert(
                        name.clone(),
                        json!({"media_type":media_type,"bytes":bytes.len(),"content_hash":hash}),
                    );
                },
                _ => {
                    files.insert(name.clone(), String::from_utf8(bytes)?);
                },
            }
        }
        Ok(json!({"files":files,"binary_files":binary_files}))
    })
    .await
    {
        Ok(view) => HttpResponse::Ok().json(view),
        Err(e) => failure(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactFileQuery {
    name: String,
}

async fn artifact_file(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    path: web::Path<(Uuid, String)>,
    query: web::Query<ArtifactFileQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = api.store(&scope);
    let (id, hash) = path.into_inner();
    let name = query.into_inner().name;
    match io(move || {
        let state = store.load(id)?;
        let artifact = state
            .attempts
            .iter()
            .filter_map(|attempt| attempt.artifact.as_ref())
            .find(|artifact| artifact.hash == hash)
            .context("Artifact is not part of this run")?;
        let content = artifact
            .files
            .get(&name)
            .context("File is not part of this sealed artifact")?;
        let setup: magician::magician_v2::relentless_runtime::TextDraftSetup =
            serde_json::from_slice(&store.read_blob(id, &state.admission.source_hash)?)?;
        let media_type = match setup
            .artifact_review
            .as_ref()
            .and_then(|recipe| recipe.files.get(&name))
        {
            Some(
                magician::magician_v2::relentless_artifact::ArtifactPresentation::Image {
                    media_type,
                }
                | magician::magician_v2::relentless_artifact::ArtifactPresentation::Video {
                    media_type,
                    ..
                }
                | magician::magician_v2::relentless_artifact::ArtifactPresentation::Audio {
                    media_type,
                    ..
                },
            ) => media_type.clone(),
            _ => "text/plain; charset=utf-8".into(),
        };
        Ok((media_type, store.read_blob(id, content)?))
    })
    .await
    {
        Ok((media_type, bytes)) => HttpResponse::Ok()
            .content_type(media_type)
            .insert_header(("Content-Disposition", "attachment"))
            .insert_header(("Cache-Control", "private, no-store"))
            .insert_header(("X-Content-Type-Options", "nosniff"))
            .body(bytes),
        Err(error) => failure(error),
    }
}

async fn cancel(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let s = api.store(&scope);
    let id = *id;
    let service = api.clone();
    match io(move || {
        let state = s.load(id)?;
        let state = if state.status.terminal()
            && state.status != RunStatus::BudgetExhausted
            && !(state.status == RunStatus::Accepted
                && state.handoff.is_none()
                && state.owner_approved != Some(false))
        {
            state
        } else {
            s.commit(id, state.generation + 1, RunEvent::Cancelled, Utc::now())?
        };
        service.cancel_worker(&scope, id);
        service.launch(scope, id);
        Ok(state)
    })
    .await
    {
        Ok(state) => run_response(&state, false),
        Err(e) => failure(e),
    }
}

async fn resume(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
    body: web::Json<ResumeRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let s = api.store(&scope);
    let id = *id;
    let attention_id = body.attention_id.clone();
    let service = api.clone();
    match io(move || {
        let state = s.load(id)?;
        anyhow::ensure!(
            state
                .attempts
                .last()
                .filter(|attempt| !attempt.decided)
                .and_then(|attempt| attempt.checkpoint.as_ref())
                .filter(|checkpoint| {
                    state.attention_id.as_deref() == Some(checkpoint.attention_id.as_str())
                })
                .is_none(),
            "answer the exact controller question through HITL before continuing"
        );
        let state = s.commit(
            id,
            state.generation,
            RunEvent::OwnerResumed { attention_id },
            Utc::now(),
        )?;
        service.launch(scope, id);
        Ok(state)
    })
    .await
    {
        Ok(state) => run_response(&state, true),
        Err(e) => failure(e),
    }
}

async fn review(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
    body: web::Json<OwnerReviewRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let store = api.store(&scope);
    let id = *id;
    let artifact_hash = body.artifact_hash.clone();
    let approved = body.approved;
    let service = api.clone();
    match io(move || {
        let state = store.load(id)?;
        let state = if state.owner_approved == Some(approved)
            && state
                .routing
                .as_ref()
                .is_some_and(|r| r.artifact_hash == artifact_hash)
        {
            state
        } else {
            store.commit(
                id,
                state.generation,
                RunEvent::OwnerReviewed {
                    artifact_hash,
                    approved,
                },
                Utc::now(),
            )?
        };
        service.launch(scope, id);
        Ok(state)
    })
    .await
    {
        Ok(state) => run_response(&state, true),
        Err(error) => failure(error),
    }
}

async fn approve_budget_reset(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
    body: web::Json<BudgetResetRequest>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let body = body.into_inner();
    let id = *id;
    // Detached durability owner completes the journal grant and wake even if
    // the HTTP connection disappears; retries reuse the exact reset identity.
    let result = tokio::spawn(async move {
        api.approve_owner_budget_reset(
            scope,
            id,
            body.reset_id,
            body.expected_generation,
            body.expected_contract_hash,
            body.reset,
        )
        .await
    })
    .await
    .context("budget approval worker")
    .and_then(|r| r);
    match result {
        Ok(state) => run_response(&state, true),
        Err(error) => failure(error),
    }
}

pub fn configure_relentless_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/relentless")
            .app_data(web::JsonConfig::default().limit(1024 * 1024))
            .route("/preview", web::post().to(preview))
            .route("/profiles", web::get().to(profiles))
            .route("/work", web::get().to(work))
            .route("/guidance-policies", web::get().to(guidance_list))
            .route("/guidance-proposals", web::get().to(guidance_proposals))
            .route(
                "/guidance-policies/preview",
                web::post().to(guidance_preview),
            )
            .route("/guidance-policies/{id}", web::get().to(guidance_get))
            .route(
                "/guidance-policies/{id}/evidence/{run_id}",
                web::get().to(guidance_evidence),
            )
            .route("/guidance-policies/{id}", web::post().to(guidance_activate))
            .route(
                "/guidance-policies/{id}/revoke",
                web::post().to(guidance_revoke),
            )
            .route(
                "/guidance-policies/{id}/resume",
                web::post().to(guidance_resume),
            )
            .route("/campaigns", web::get().to(campaigns))
            .route("/campaigns/preview", web::post().to(campaign_preview))
            .route("/campaigns/{id}", web::post().to(campaign_configure))
            .route("/campaigns/{id}/captures", web::get().to(catalog::captures))
            .route("/campaigns/{id}/captures", web::post().to(campaign_capture))
            .route(
                "/campaigns/{id}/captures/{capture}/review",
                web::post().to(campaign_review),
            )
            .route("/catalog", web::get().to(catalog::catalog))
            .route("/proposals/{id}", web::get().to(catalog::proposal_get))
            .route("/proposals", web::get().to(proposals))
            .route("/task-context/{task_id}", web::get().to(task_context))
            .route("/task-policy/{task_id}", web::post().to(task_policy))
            .route("/runs", web::post().to(start))
            .route("/runs", web::get().to(list))
            .route("/runs/{id}", web::get().to(get))
            .route(
                "/runs/{id}/budget-resets",
                web::post().to(approve_budget_reset),
            )
            .route(
                "/runs/{id}/reconcile-costs",
                web::post().to(reconcile_costs),
            )
            .route("/runs/{id}/lessons", web::get().to(lessons))
            .route("/runs/{id}/report", web::get().to(report))
            .route("/runs/{id}/artifacts/{hash}", web::get().to(artifact))
            .route(
                "/runs/{id}/artifacts/{hash}/evidence",
                web::get().to(artifact_evidence),
            )
            .route(
                "/runs/{id}/artifacts/{hash}/file",
                web::get().to(artifact_file),
            )
            .route("/runs/{id}/cancel", web::post().to(cancel))
            .route("/runs/{id}/resume", web::post().to(resume))
            .route("/runs/{id}/review", web::post().to(review)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{http::StatusCode, test::TestRequest};
    use magician::magician_v2::artifact_v2::V3ReadApi;
    use magician::magician_v2::cloudflare_access::VerifiedRequestAuthentication;
    use magician::magician_v2::{
        artifact_v2::{models::ExecutionOutcomeSnapshot, ArtifactV2Service, ScopeRef},
        orchestrator::MagicianV2Orchestrator,
        query_analysis::operation_llm_router::OperationLlmRouter,
        storage::WaitingState,
        task_run_factory::create_task_backed_direct_root_run_with_output_owner,
    };
    use std::sync::Arc;

    #[tokio::test]
    async fn artifact_evidence_keeps_exact_admitted_references_owner_scoped() {
        use magician::magician_v2::relentless::budget::{
            Allowance, OperationClass, OperationIntent,
        };
        let temp = tempfile::tempdir().unwrap();
        let counter = Arc::new(NoModelCalls(Default::default()));
        let (artifacts, orchestrator) =
            magician::magician_v2::test_support::build_test_artifact_v2_harness_with_llm(
                temp.path(),
                counter.clone(),
            );
        let router = Arc::new(OperationLlmRouter::new(Some(text_routes())));
        let api = web::Data::new(RelentlessApi::new(
            artifacts.workspace().clone(),
            router.clone(),
            artifacts,
            orchestrator,
            None,
        ));
        let scope = RunScope {
            principal: "owner".into(),
            workspace: "work".into(),
        };
        let store = api.store(&scope);
        let id = Uuid::new_v4();
        let mut proposal = text_proposal();
        let source = "Reference: facts.txt\nOrigin: uploaded file\n\nNine completed; three stopped.\nPreserve café and \"quotes\".\n";
        proposal.sources = BTreeMap::from([("file:reference".into(), source.into())]);
        let state = PreparedTextDraft::resolve(&router, &proposal)
            .unwrap()
            .admit(&store, id, "owner".into(), "reference-task".into())
            .unwrap();
        // Later edits to a source input must not alter the reviewed evidence.
        proposal
            .sources
            .insert("file:reference".into(), "Changed after admission".into());
        let claimed = store
            .claim(id, "evidence-fixture".into(), state.started_at)
            .unwrap();
        store
            .commit(
                id,
                claimed.generation,
                RunEvent::AttemptStarted {
                    actor: OperationIntent {
                        id: "actor".into(),
                        input_hash: "a".repeat(64),
                        class: OperationClass::Actor,
                        maximum_cost_microusd: 1,
                    },
                    review: Allowance {
                        id: "reviews".into(),
                        input_hash: "b".repeat(64),
                        maximum_microusd: 1,
                        remaining_microusd: 1,
                        closed: false,
                    },
                },
                state.started_at,
            )
            .unwrap();
        let artifact = store
            .prepare_artifact(
                id,
                &BTreeMap::from([("draft.md".into(), b"Local draft".to_vec())]),
            )
            .unwrap();
        store
            .commit(
                id,
                claimed.generation,
                RunEvent::ArtifactSealed {
                    artifact: artifact.clone(),
                },
                state.started_at,
            )
            .unwrap();
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(api)
                .service(web::scope("/api/magician/v2").configure(configure_relentless_routes)),
        )
        .await;
        let uri = format!(
            "/api/magician/v2/relentless/runs/{id}/artifacts/{}/evidence",
            artifact.hash
        );
        let owned = |path: &str, workspace: &str| {
            let req = TestRequest::get().uri(path).to_request();
            req.extensions_mut()
                .insert(VerifiedRequestIdentity::for_test(
                    "owner",
                    Some(workspace),
                    VerifiedRequestAuthentication::MagicianBearer,
                ));
            req
        };
        let response = actix_web::test::call_service(&app, owned(&uri, "work")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(CACHE_CONTROL).unwrap(), "no-store");
        let body: serde_json::Value = actix_web::test::read_body_json(response).await;
        assert_eq!(body["evidence"]["artifact_hash"], artifact.hash);
        assert_eq!(
            body["evidence"]["admitted_sources"],
            json!([{
                "source_id":"file:reference", "content_hash":blake3::hash(source.as_bytes()).to_hex().to_string(),
                "byte_length":source.len(), "text":source,
            }])
        );
        assert_eq!(
            body["content_hash"],
            blake3::hash(&serde_json::to_vec(&body["evidence"]).unwrap())
                .to_hex()
                .to_string()
        );
        assert_eq!(
            actix_web::test::call_service(&app, TestRequest::get().uri(&uri).to_request())
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            actix_web::test::call_service(&app, owned(&uri, "other"))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        let unknown = format!(
            "/api/magician/v2/relentless/runs/{id}/artifacts/{}/evidence",
            "c".repeat(64)
        );
        assert_eq!(
            actix_web::test::call_service(&app, owned(&unknown, "work"))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn budget_reset_route_requires_verified_owner_and_cannot_spoof_scope() {
        let temp = tempfile::tempdir().unwrap();
        let (artifacts, orchestrator) =
            magician::magician_v2::test_support::build_test_artifact_v2_harness_with_llm(
                temp.path(),
                Arc::new(NoModelCalls(Default::default())),
            );
        let api = web::Data::new(RelentlessApi::new(
            artifacts.workspace().clone(),
            Arc::new(OperationLlmRouter::new(Some(text_routes()))),
            artifacts,
            orchestrator,
            None,
        ));
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(api)
                .service(web::scope("/api/magician/v2").configure(configure_relentless_routes)),
        )
        .await;
        let uri = format!(
            "/api/magician/v2/relentless/runs/{}/budget-resets",
            Uuid::new_v4()
        );
        let body = json!({"reset_id":Uuid::new_v4(),"expected_generation":7,"expected_contract_hash":"frozen","reset":{"attempts":25}});
        let request = TestRequest::post()
            .uri(&uri)
            .set_json(&body)
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "work"))
            .to_request();
        assert_eq!(
            actix_web::test::call_service(&app, request).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let request = TestRequest::post()
            .uri(&uri)
            .set_json(&body)
            .insert_header(("X-Workspace", "other"))
            .to_request();
        request
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test(
                "owner",
                Some("work"),
                VerifiedRequestAuthentication::MagicianBearer,
            ));
        assert_eq!(
            actix_web::test::call_service(&app, request).await.status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn closed_budget_history_does_not_return_to_the_review_inbox() {
        let temp = tempfile::tempdir().unwrap();
        let (artifacts, orchestrator) =
            magician::magician_v2::test_support::build_test_artifact_v2_harness_with_llm(
                temp.path(),
                Arc::new(NoModelCalls(Default::default())),
            );
        let router = Arc::new(OperationLlmRouter::new(Some(text_routes())));
        let api = RelentlessApi::new(
            artifacts.workspace().clone(),
            router.clone(),
            artifacts,
            orchestrator,
            None,
        );
        let scope = RunScope {
            principal: "owner".into(),
            workspace: "work".into(),
        };
        let id = Uuid::new_v4();
        let store = api.store(&scope);
        let state = PreparedTextDraft::resolve(&router, &text_proposal())
            .unwrap()
            .admit(&store, id, "owner".into(), "retained-closed-task".into())
            .unwrap();
        let at = state.deadline_at;
        let claimed = store.claim(id, "deadline-fixture".into(), at).unwrap();
        store
            .commit(id, claimed.generation, RunEvent::DeadlineExpired, at)
            .unwrap();
        store
            .commit(id, claimed.generation, RunEvent::LeaseReleased, at)
            .unwrap();
        let query = catalog::PageQuery::default();
        let page = runtime_core::storage::PaginationParams {
            limit: 20,
            offset: 0,
        };
        let inbox = catalog::catalog_view(&api, &scope, &query, &page, "inbox")
            .await
            .unwrap();
        assert!(inbox["items"].as_array().unwrap().is_empty());
        assert_eq!(inbox["counts"]["attention"], 0);
        let history = catalog::catalog_view(&api, &scope, &query, &page, "runs")
            .await
            .unwrap();
        assert_eq!(history["items"][0]["item"]["id"], id.to_string());
        assert_eq!(history["items"][0]["item"]["group"], "stopped");

        // The owner can close a paused run without granting any allowance.
        // BudgetExhausted is terminal to dispatch, but remains cancellable.
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(api))
                .service(web::scope("/api/magician/v2").configure(configure_relentless_routes)),
        )
        .await;
        for _ in 0..2 {
            let request = TestRequest::post()
                .uri(&format!("/api/magician/v2/relentless/runs/{id}/cancel"))
                .to_request();
            request
                .extensions_mut()
                .insert(VerifiedRequestIdentity::for_test(
                    "owner",
                    Some("work"),
                    VerifiedRequestAuthentication::MagicianBearer,
                ));
            let response = actix_web::test::call_service(&app, request).await;
            assert_eq!(response.status(), StatusCode::OK);
            let cancelled = store.load(id).unwrap();
            assert_eq!(cancelled.status, RunStatus::Cancelled);
            assert!(cancelled.budget_resets.is_empty());
            assert_eq!(
                cancelled.budget.spend().unwrap(),
                state.budget.spend().unwrap()
            );
        }
    }

    #[tokio::test]
    async fn server_catalog_filters_before_paging_and_keeps_scoped_details_separate() {
        let temp = tempfile::tempdir().unwrap();
        let (artifacts, orchestrator) =
            magician::magician_v2::test_support::build_test_artifact_v2_harness_with_llm(
                temp.path(),
                Arc::new(NoModelCalls(Default::default())),
            );
        let workspace = artifacts.workspace().clone();
        let router = Arc::new(OperationLlmRouter::new(Some(text_routes())));
        let api = web::Data::new(RelentlessApi::new(
            artifacts.workspace().clone(),
            router.clone(),
            artifacts,
            orchestrator,
            None,
        ));
        let scope = RunScope {
            principal: "owner".into(),
            workspace: "workspace".into(),
        };
        for i in 0..37 {
            let mut proposal = text_proposal();
            proposal.goal = format!(
                "Catalog row {} {i}",
                if i % 2 == 0 { "even" } else { "odd" }
            );
            PreparedTextDraft::resolve(&router, &proposal)
                .unwrap()
                .admit(
                    &api.store(&scope),
                    Uuid::new_v4(),
                    "owner".into(),
                    format!("task-{i}"),
                )
                .unwrap();
        }
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(api.clone())
                .service(web::scope("/api/magician/v2").configure(configure_relentless_routes)),
        )
        .await;
        let owned = |path: &str, workspace: &str| {
            let req = TestRequest::get().uri(path).to_request();
            req.extensions_mut()
                .insert(VerifiedRequestIdentity::for_test(
                    "owner",
                    Some(workspace),
                    VerifiedRequestAuthentication::MagicianBearer,
                ));
            req
        };
        let first: serde_json::Value = actix_web::test::call_and_read_body_json(
            &app,
            owned(
                "/api/magician/v2/relentless/catalog?view=runs&q=odd&limit=3&offset=0",
                "workspace",
            ),
        )
        .await;
        let second: serde_json::Value = actix_web::test::call_and_read_body_json(
            &app,
            owned(
                "/api/magician/v2/relentless/catalog?view=runs&q=odd&limit=3&offset=3",
                "workspace",
            ),
        )
        .await;
        assert_eq!(first["pagination"]["total"], 18);
        assert_eq!(second["pagination"]["offset"], 3);
        assert_eq!(second["items"].as_array().unwrap().len(), 3);
        for row in first["items"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["items"].as_array().unwrap())
        {
            assert!(row["item"]["goal"].as_str().unwrap().contains("odd"));
            assert!(row["item"].get("attempts").is_none());
            assert!(row["item"].get("contract").is_none());
        }
        assert_ne!(
            first["items"][0]["item"]["id"],
            second["items"][0]["item"]["id"]
        );
        let id = first["items"][0]["item"]["id"].as_str().unwrap();
        let full: serde_json::Value = actix_web::test::call_and_read_body_json(
            &app,
            owned(
                &format!("/api/magician/v2/relentless/runs/{id}"),
                "workspace",
            ),
        )
        .await;
        assert_eq!(full["id"], id);
        assert!(full.get("attempts").is_some());
        let private: serde_json::Value = actix_web::test::call_and_read_body_json(
            &app,
            owned(
                "/api/magician/v2/relentless/catalog?view=runs&limit=3",
                "other",
            ),
        )
        .await;
        assert_eq!(private["pagination"]["total"], 0);
        let response = actix_web::test::call_service(
            &app,
            owned(&format!("/api/magician/v2/relentless/runs/{id}"), "other"),
        )
        .await;
        assert!(!response.status().is_success());
        for query in [
            "limit=0",
            "limit=101",
            "offset=-1",
            "view=unknown",
            "status=unknown",
            "principal=other",
        ] {
            let response = actix_web::test::call_service(
                &app,
                owned(
                    &format!("/api/magician/v2/relentless/catalog?{query}"),
                    "workspace",
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        }
        let beyond: serde_json::Value = actix_web::test::call_and_read_body_json(
            &app,
            owned(
                "/api/magician/v2/relentless/catalog?view=runs&offset=99999",
                "workspace",
            ),
        )
        .await;
        assert!(beyond["items"].as_array().unwrap().is_empty());
        assert_eq!(beyond["pagination"]["total"], 37);
        for endpoint in [
            "runs",
            "proposals",
            "work",
            "campaigns",
            "guidance-policies",
            "guidance-proposals",
        ] {
            let response = actix_web::test::call_service(
                &app,
                owned(
                    &format!("/api/magician/v2/relentless/{endpoint}?limit=0"),
                    "workspace",
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{endpoint}");
            let result: serde_json::Value = actix_web::test::call_and_read_body_json(
                &app,
                owned(
                    &format!("/api/magician/v2/relentless/{endpoint}?limit=3&offset=2"),
                    "workspace",
                ),
            )
            .await;
            assert_eq!(result["pagination"]["limit"], 3, "{endpoint}: {result}");
            assert_eq!(result["pagination"]["offset"], 2, "{endpoint}");
        }
        let unauthenticated = actix_web::test::call_service(
            &app,
            TestRequest::get()
                .uri("/api/magician/v2/relentless/catalog?limit=3")
                .to_request(),
        )
        .await;
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        let broken = Uuid::new_v4();
        let journal = workspace
            .scope_root("owner", "workspace")
            .join("relentless/runs")
            .join(broken.to_string())
            .join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        std::fs::write(journal.join("00000000000000000000.json"), b"{broken").unwrap();
        let mixed: serde_json::Value = actix_web::test::call_and_read_body_json(
            &app,
            owned(
                "/api/magician/v2/relentless/catalog?view=runs&limit=100",
                "workspace",
            ),
        )
        .await;
        assert_eq!(mixed["pagination"]["total"], 38);
        assert_eq!(
            mixed["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["kind"] == "run")
                .count(),
            37
        );
        let damaged = mixed["items"].as_array().unwrap().last().unwrap();
        assert_eq!(damaged["kind"], "unavailable");
        assert_eq!(damaged["item"]["id"], broken.to_string());
        assert!(damaged["item"].get("spend").is_none());
        assert_eq!(mixed["counts"]["attention"], 1);
        let private: serde_json::Value = actix_web::test::call_and_read_body_json(
            &app,
            owned(
                "/api/magician/v2/relentless/catalog?view=runs",
                "other-workspace",
            ),
        )
        .await;
        assert_eq!(private["pagination"]["total"], 0);
        let detail = actix_web::test::call_service(
            &app,
            owned(
                &format!("/api/magician/v2/relentless/runs/{broken}"),
                "workspace",
            ),
        )
        .await;
        assert!(!detail.status().is_success());
    }

    #[test]
    fn accepted_runs_without_delivery_policy_are_complete_but_pending_routes_stay_active() {
        use magician::magician_v2::relentless::routing::{ReviewRoute, RoutingDecision};
        let temp = tempfile::tempdir().unwrap();
        let scope = RunScope {
            principal: "owner".into(),
            workspace: "workspace".into(),
        };
        let store = magician::magician_v2::relentless::store::RunStore::new(temp.path(), scope);
        let prepared = PreparedTextDraft::resolve(
            &OperationLlmRouter::new(Some(text_routes())),
            &text_proposal(),
        )
        .unwrap();
        let mut run = prepared
            .admit(&store, Uuid::new_v4(), "owner".into(), "task".into())
            .unwrap();
        run.status = RunStatus::Accepted;
        assert_eq!(
            catalog::group(&run, false, false),
            "active",
            "an admitted delivery route still needs settlement"
        );
        assert_eq!(catalog::label(&run, "active", false), "Preparing handoff");
        assert_eq!(
            catalog::group(&run, false, true),
            "complete",
            "sealed held-out trials intentionally skip the admitted delivery route"
        );
        assert_eq!(catalog::label(&run, "complete", false), "Accepted");
        assert_eq!(catalog::group(&run, true, true), "review");
        run.routing = Some(RoutingDecision {
            artifact_hash: "a".repeat(64),
            policy_hash: "b".repeat(64),
            route: ReviewRoute::HumanReview,
            reason: "owner review required".into(),
        });
        assert_eq!(catalog::group(&run, false, false), "review");
        assert_eq!(catalog::label(&run, "review", false), "Review draft");
        run.routing = None;
        run.admission.routing_policy = None;
        assert_eq!(
            catalog::group(&run, false, false),
            "complete",
            "internal evaluations have no delivery work remaining"
        );
        assert_eq!(catalog::label(&run, "complete", false), "Accepted");
        assert_eq!(
            catalog::group(&run, true, false),
            "review",
            "capture review remains authoritative"
        );
        run.status = RunStatus::Reviewing;
        assert_eq!(catalog::group(&run, false, false), "active");
    }

    #[test]
    fn catalog_trial_identity_comes_from_sealed_typed_origin() {
        use magician::magician_v2::learning::relentless_promotion::{
            GuidanceRunOrigin, GuidanceVariant, QualificationPhase,
        };
        let temp = tempfile::tempdir().unwrap();
        let store = magician::magician_v2::relentless::store::RunStore::new(
            temp.path(),
            RunScope {
                principal: "owner".into(),
                workspace: "workspace".into(),
            },
        );
        let prepared = PreparedTextDraft::resolve(
            &OperationLlmRouter::new(Some(text_routes())),
            &text_proposal(),
        )
        .unwrap();
        let mut run = prepared
            .admit(&store, Uuid::new_v4(), "owner".into(), "task".into())
            .unwrap();
        run.admission.source_id = "guidance-trial".into();
        assert!(
            !catalog::internal_trial(&store, &run).unwrap(),
            "a name is not trial authority"
        );
        let mut setup: magician::magician_v2::relentless_runtime::TextDraftSetup =
            serde_json::from_slice(&store.read_blob(run.id, &run.admission.source_hash).unwrap())
                .unwrap();
        setup.guidance_origin = Some(GuidanceRunOrigin::Trial {
            policy_id: Uuid::new_v4(),
            version_hash: "a".repeat(64),
            phase: QualificationPhase::Evaluation,
            case_id: "held-out".into(),
            variant: GuidanceVariant::Baseline,
        });
        run.admission.source_id = "ordinary-name".into();
        run.admission.source_hash = store
            .put_blob(run.id, &serde_json::to_vec(&setup).unwrap())
            .unwrap();
        assert!(catalog::internal_trial(&store, &run).unwrap());
        setup.guidance_origin = Some(GuidanceRunOrigin::Adoption {
            policy_id: Uuid::new_v4(),
            version_hash: "a".repeat(64),
            task_id: "task".into(),
            baseline_contract_hash: "b".repeat(64),
        });
        run.admission.source_hash = store
            .put_blob(run.id, &serde_json::to_vec(&setup).unwrap())
            .unwrap();
        assert!(
            !catalog::internal_trial(&store, &run).unwrap(),
            "production adoption still requires delivery"
        );
        run.admission.source_hash = "c".repeat(64);
        assert!(
            catalog::internal_trial(&store, &run).is_err(),
            "unverifiable source fails closed"
        );
    }

    fn text_routes() -> magicllm::config::LLMRouterConfig {
        let mut config = magicllm::config::LLMRouterConfig::default();
        config.default_profile = "actor".into();
        for (name, provider, model, family, context, output) in [
            (
                "actor",
                "anthropic",
                "claude-sonnet-4-6",
                "claude",
                1_000_000,
                8192,
            ),
            (
                "critic",
                "gemini",
                "gemini-3.1-pro-preview",
                "gemini",
                1_114_112,
                16384,
            ),
        ] {
            config.profiles.insert(
                name.into(),
                serde_json::from_value(json!({
                    "provider":provider, "model":model,
                    "api_key_env":"__RELENTLESS_TEST_KEY_ABSENT__",
                    "supports_tool_calling":true,
                    "context_window_tokens":context, "max_output_tokens":output,
                    "timeout_secs":300, "metadata":{"model_family":family}
                }))
                .unwrap(),
            );
        }
        config
    }

    #[test]
    fn bounded_native_route_keeps_its_config_generation_after_reload() {
        let mut config = text_routes();
        let router = OperationLlmRouter::new(Some(config.clone()));
        let frozen = router.bounded_route("actor").unwrap();
        let pin = frozen.model_pin();
        config.profiles.get_mut("actor").unwrap().temperature = Some(0.7);
        // This fixture has intentionally absent provider keys. Reload's bool
        // reports live transport availability, not whether config was updated.
        router.reload_from_config(Some(config));
        assert_eq!(
            router.router_config_snapshot().unwrap().profiles["actor"].temperature,
            Some(0.7)
        );
        assert_eq!(frozen.model_pin(), pin);
        assert_ne!(router.bounded_route("actor").unwrap().model_pin(), pin);
        assert!(frozen.maximum_microusd() > 0);
    }

    #[test]
    fn bounded_native_route_refuses_missing_physical_bounds_and_independence() {
        for missing in ["context", "output", "timeout", "family"] {
            let mut config = text_routes();
            let profile = config.profiles.get_mut("actor").unwrap();
            match missing {
                "context" => profile.context_window_tokens = None,
                "output" => profile.max_output_tokens = None,
                "timeout" => profile.timeout_secs = None,
                _ => profile.metadata = None,
            }
            assert!(
                OperationLlmRouter::new(Some(config))
                    .bounded_route("actor")
                    .is_err(),
                "missing {missing}"
            );
        }
        let mut config = text_routes();
        config.profiles.get_mut("actor").unwrap().chunking =
            Some(magicllm::config::ChunkingConfig {
                enabled: true,
                ..Default::default()
            });
        assert!(OperationLlmRouter::new(Some(config))
            .bounded_route("actor")
            .is_err());
        for (key, value) in [
            ("server_web_search", json!(true)),
            ("candidateCount", json!(2)),
            ("gemini_api_mode", json!("interactions")),
            ("future_provider_parameter", json!({"unknown":true})),
        ] {
            let mut config = text_routes();
            config
                .profiles
                .get_mut("critic")
                .unwrap()
                .metadata
                .as_mut()
                .unwrap()
                .insert(key.into(), value);
            assert!(
                OperationLlmRouter::new(Some(config))
                    .bounded_route("critic")
                    .is_err(),
                "opaque parameter {key}"
            );
        }
    }

    fn text_proposal() -> TextDraftProposal {
        serde_json::from_value(json!({
            "goal":"Create a grounded local draft", "artifact_file":"draft.md", "maximum_artifact_bytes":65536,
            "actor_profile":"actor", "critic_profiles":["critic","critic","critic"], "sources":{"brief":"Fictional project"},
            "criteria":[{"id":"quality", "specification":{"kind":"rubric", "instructions":"Complete and clear", "anchors":[{"score":0,"description":"Incomplete"},{"score":1,"description":"Complete"}]}, "weight":1,"must_pass":false,"pass_score":null}],
            "score_at_least":0.8, "review":{"panel_size":3,"score_quorum":3,"min_confidence":0.8,"max_score_spread":0.2,"noise_margin":0.02,"max_retries_per_critic":1},
            "budget":{"max_attempts":3,"max_wall_clock_secs":600,"max_cost_microusd":40000000,"plateau_attempts":3},
            "human_review":"required", "handoff_kind":"capture"
        })).unwrap()
    }

    #[test]
    fn draft_policy_and_reviewer_identity_are_frozen_before_admission() {
        let router = OperationLlmRouter::new(Some(text_routes()));
        let mut proposal = text_proposal();
        let required = PreparedTextDraft::resolve(&router, &proposal).unwrap();
        proposal.human_review =
            magician::magician_v2::relentless_runtime::HumanReviewMode::Independent;
        let independent = PreparedTextDraft::resolve(&router, &proposal).unwrap();
        let policy = independent.setup.routing_policy.as_ref().unwrap();
        assert!(!policy.owner_review_required);
        assert!(policy.allow_automatic_capture);
        assert!(!policy.allow_automatic_internal);
        assert_ne!(
            required.contract.hash().unwrap(),
            independent.contract.hash().unwrap()
        );
        proposal.critic_profiles = vec!["actor".into(); 3];
        assert!(PreparedTextDraft::resolve(&router, &proposal).is_err());
    }

    #[tokio::test]
    async fn ordinary_task_proposal_is_scoped_idempotent_and_cannot_admit_work() {
        use magician::magician_v2::{
            agents::{definition_store::AgentDefinitionStore, memory::AgentMemoryResolver},
            execution::{agent_resources::AgentResources, compiled_handlers::propose_relentless},
            realtime_events::RuntimeTransportBroadcaster,
            relentless_runtime::ProposedTextDraft,
            user_requests::UserRequestService,
        };
        let temp = tempfile::tempdir().unwrap();
        let counter = Arc::new(NoModelCalls(Default::default()));
        let (service, orchestrator) =
            magician::magician_v2::test_support::build_test_artifact_v2_harness_with_llm(
                temp.path(),
                counter.clone(),
            );
        let (task, execution, _) =
            owned_root(&service, &orchestrator, Uuid::new_v4(), "proposal-test")
                .await
                .unwrap();
        let router = Arc::new(OperationLlmRouter::new(Some(text_routes())));
        let requests = Arc::new(
            UserRequestService::new(Arc::new(RuntimeTransportBroadcaster::new(64)))
                .with_workspace_layout(service.workspace().clone())
                .with_history_persist_path(
                    service
                        .workspace()
                        .base_root()
                        .join("user_request_history.json"),
                )
                .with_pending_persist_path(
                    service
                        .workspace()
                        .base_root()
                        .join("user_request_pending.json"),
                )
                .await,
        );
        let resources = Arc::new(AgentResources {
            magician_config: Arc::new(std::sync::RwLock::new(Default::default())),
            memory_resolver: Arc::new(AgentMemoryResolver::with_workspace_layout(
                service.workspace().clone(),
            )),
            agent_definition_store: Arc::new(AgentDefinitionStore::with_base_path(
                temp.path().join("agents"),
            )),
            artifact_workspace: service.workspace().clone(),
            artifact_v2_service: Some(service.clone()),
            event_broadcaster: None,
            operation_llm_router: Some(router.clone()),
            secret_store_resolver: None,
            content_acquisition_resolver: Default::default(),
            file_sandbox: Default::default(),
            relentless_research: None,
            tool_index: Default::default(),
            user_request_service: Some(requests.clone()),
            agent_runtime: None,
        });
        let proposal = text_proposal();
        let args = json!({"proposal":proposal,"__principal":"owner","__workspace":"workspace","__agent_id":"engineer","__task_id":task,"__execution_id":execution});
        let mut catalog_args = args.clone();
        catalog_args["operation"] = json!("profiles");
        catalog_args.as_object_mut().unwrap().remove("proposal");
        let catalog = propose_relentless::handle(resources.clone(), catalog_args)
            .await
            .unwrap();
        assert_eq!(catalog["profiles"].as_array().unwrap().len(), 2);
        assert!(requests
            .list_pending_for_scope("owner", "workspace")
            .await
            .is_empty());
        let first = propose_relentless::handle(resources.clone(), args.clone())
            .await
            .unwrap();
        let retry = propose_relentless::handle(resources.clone(), args.clone())
            .await
            .unwrap();
        assert_eq!(first["request_id"], retry["request_id"]);
        assert_eq!(first["admitted"], false);
        let api = RelentlessApi::new(
            service.workspace().clone(),
            router.clone(),
            service.clone(),
            orchestrator.clone(),
            None,
        )
        .with_user_requests(requests.clone());
        let scope = RunScope {
            principal: "owner".into(),
            workspace: "workspace".into(),
        };
        let pending = api.pending_proposals(&scope).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].origin.task_id, task);
        let mut changed: ProposedTextDraft = pending[0].clone();
        changed.proposal.goal = "Changed bytes".into();
        assert!(changed.validate().is_err());
        assert!(api
            .pending_proposals(&RunScope {
                principal: "other".into(),
                workspace: "workspace".into()
            })
            .await
            .unwrap()
            .is_empty());
        let mut spoofed = args.clone();
        spoofed["__agent_id"] = json!("other-agent");
        assert!(propose_relentless::handle(resources.clone(), spoofed)
            .await
            .is_err());
        let mut cross_scope = args.clone();
        cross_scope["__workspace"] = json!("other");
        assert!(propose_relentless::handle(resources.clone(), cross_scope)
            .await
            .is_err());
        let hash = PreparedTextDraft::resolve(&router, &proposal)
            .unwrap()
            .contract
            .hash()
            .unwrap();
        let request_id = first["request_id"].as_str().unwrap();
        // Neither a different preview nor edited proposal bytes may create an
        // owning task. An owner must review the new exact contract first.
        assert!(api
            .start_owner_text_draft(
                scope.clone(),
                Uuid::new_v4(),
                &proposal,
                "general",
                "engineer",
                "wrong-reviewed-hash",
                Some(request_id)
            )
            .await
            .is_err());
        let changed_hash = PreparedTextDraft::resolve(&router, &changed.proposal)
            .unwrap()
            .contract
            .hash()
            .unwrap();
        assert!(api
            .start_owner_text_draft(
                scope.clone(),
                Uuid::new_v4(),
                &changed.proposal,
                "general",
                "engineer",
                &changed_hash,
                Some(request_id)
            )
            .await
            .is_err());
        assert!(api
            .start_owner_text_draft(
                scope.clone(),
                Uuid::new_v4(),
                &proposal,
                "general",
                "other-agent",
                &hash,
                Some(request_id)
            )
            .await
            .is_err());
        assert_eq!(
            service
                .list_tasks(&ScopeRef::system_internal_unauthenticated(
                    "owner",
                    "workspace"
                ))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(api.store(&scope).list().unwrap().is_empty());
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    struct NoModelCalls(std::sync::atomic::AtomicUsize);
    #[async_trait::async_trait]
    impl magician::magician_v2::query_analysis::operation_llm_router::QueryAnalysisLLM
        for NoModelCalls
    {
        async fn generate_analysis(
            &self,
            _: &str,
        ) -> Result<
            magician::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse,
        > {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("precomputed output must not invoke a model")
        }
    }

    #[async_trait::async_trait]
    impl magician::magician_v2::slot_graph::extraction::LlmService for NoModelCalls {
        async fn call_function(
            &self,
            _: magician::magician_v2::slot_graph::extraction::LlmFunctionCallRequest,
        ) -> Result<magician::magician_v2::slot_graph::extraction::LlmFunctionCallResponse>
        {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("ownership inspection must not invoke a model")
        }

        fn provider_name(&self) -> String {
            "no-dispatch-ownership-fixture".into()
        }
    }

    async fn owned_root(
        service: &ArtifactV2Service,
        orchestrator: &Arc<MagicianV2Orchestrator>,
        creation_id: Uuid,
        owner_id: &str,
    ) -> std::result::Result<(String, String, String), String> {
        create_task_backed_direct_root_run_with_output_owner(
            service,
            orchestrator,
            "owner",
            "workspace",
            "general",
            Some("Reviewed draft"),
            "Produce local text",
            "engineer",
            magician::magician_v2::storage::TaskCreatedBy::User,
            false,
            false,
            owner_id,
            creation_id,
        )
        .await
    }

    #[tokio::test]
    async fn controller_owned_root_refuses_ordinary_budget_reads_and_grants() {
        use crate::web_api::MagicianV2Api;
        use actix_web::middleware::from_fn;
        use magician::magician_v2::auth::{
            middleware::authenticate_request, workspace_registry, AuthMethod, AuthStore,
            CredentialKind,
        };
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(AuthStore::open(temp.path()).unwrap());
        // The first identity adopts anonymous; keep this fixture's task under
        // the independently owned owner/workspace scope used by owned_root.
        for name in ["bootstrap", "owner"] {
            store
                .create_identity(
                    name,
                    name,
                    CredentialKind::Password {
                        hash: "fixture".into(),
                    },
                )
                .unwrap();
        }
        workspace_registry::create(
            &temp.path().join("scopes"),
            &store.find_identity("owner").unwrap().unwrap(),
            "workspace",
            "Workspace",
            None,
        )
        .unwrap();
        let (_, token) = store
            .mint_session("owner", "workspace", AuthMethod::Password, 1)
            .unwrap();
        let auth = crate::auth_api::auth_runtime(store, Default::default());
        let counter = Arc::new(NoModelCalls(Default::default()));
        let (service, orchestrator) =
            magician::magician_v2::test_support::build_test_artifact_v2_harness_with_llm(
                temp.path(),
                counter.clone(),
            );
        let (task, execution, _) =
            owned_root(&service, &orchestrator, Uuid::new_v4(), "budget-controller")
                .await
                .unwrap();
        let api = web::Data::new(MagicianV2Api::with_shared_pause_store(
            orchestrator.clone(),
            orchestrator.full_pause_store_handle().unwrap(),
        ));
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(api)
                .app_data(auth)
                .wrap(from_fn(authenticate_request))
                .route(
                    "/tasks/{task}/budget",
                    web::get().to(crate::budgets_api::task_budget_get_handler),
                )
                .route(
                    "/tasks/{task}/budget",
                    web::post().to(crate::budgets_api::task_budget_post_handler),
                ),
        )
        .await;
        for method in [actix_web::http::Method::GET, actix_web::http::Method::POST] {
            let req = TestRequest::default()
                .method(method)
                .uri(&format!("/tasks/{task}/budget"))
                .insert_header(("Authorization", format!("Bearer {token}")))
                .set_json(json!({"unlimited": true}))
                .to_request();
            let response = actix_web::test::call_service(&app, req).await;
            assert_eq!(response.status(), actix_web::http::StatusCode::CONFLICT);
            let body: serde_json::Value = actix_web::test::read_body_json(response).await;
            assert_eq!(body["error"], "external_controller_budget_frozen");
        }
        let scope = ScopeRef::system_internal_unauthenticated("owner", "workspace");
        let grants = service
            .workspace()
            .execution_dir("owner", "workspace", &task, &execution)
            .join(magician::magician_v2::execution::agentic::run_budget::BUDGET_GRANTS_FILE);
        assert!(!grants.exists());
        assert!(service
            .execution_has_external_output_owner(&scope, &task, &execution)
            .await
            .unwrap());
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn controller_owned_ordinary_resume_preserves_pause_and_refuses_corrupt_owner() {
        use crate::web_api::{AgenticResumeRequest, AgenticResumeValue, MagicianV2Api};
        use magician::magician_v2::execution::agentic::{
            ActionExecutors, AgenticPauseState, EnvironmentState, FullPauseData, ShellState,
            UserInputType,
        };
        let temp = tempfile::tempdir().unwrap();
        let counter = Arc::new(NoModelCalls(Default::default()));
        let (service, orchestrator) =
            magician::magician_v2::test_support::build_test_artifact_v2_harness_with_llm(
                temp.path(),
                counter.clone(),
            );
        let (task_id, execution_id, _) =
            owned_root(&service, &orchestrator, Uuid::new_v4(), "test-controller")
                .await
                .unwrap();
        let api = MagicianV2Api::with_shared_pause_store(
            orchestrator.clone(),
            orchestrator.full_pause_store_handle().unwrap(),
        );
        let mut state = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Shell(ShellState::default()),
            "history",
            6,
            1,
        )
        .with_scope("owner", "workspace");
        state.task_id = Some(task_id.clone());
        state.execution_id = Some(execution_id.clone());
        state.root_execution_id = Some(execution_id.clone());
        state.controller_attempt_hash = Some("a".repeat(64));
        let pause_key = api.full_pause_store().store(FullPauseData::new(
            state,
            UserInputType::Text {
                placeholder: None,
                multiline: false,
            },
            ActionExecutors::new(
                counter.clone(),
                Arc::new(magician::magician_v2::prompts::PromptManager::new(
                    Arc::new(
                        magician::magician_v2::prompts::JsonPromptStorage::with_default_config()
                            .unwrap(),
                    ),
                )),
            ),
            Some("Which audience?".into()),
            None,
        ));
        let scope = ScopeRef::system_internal_unauthenticated("owner", "workspace");
        let owner_path = service
            .workspace()
            .execution_dir("owner", "workspace", &task_id, &execution_id)
            .join("external_output_owner.json");
        let owner_bytes = std::fs::read(&owner_path).unwrap();
        for case in ["owned", "wrong-scope", "corrupt-owner"] {
            if case == "corrupt-owner" {
                std::fs::write(&owner_path, b"corrupt owner fixture").unwrap();
            }
            let response = api
                .resume_agentic_execution_with_scope(
                    web::Path::from(execution_id.clone()),
                    web::Json(AgenticResumeRequest {
                        pause_state_id: Some(pause_key.clone()),
                        plan_id: None,
                        step_id: None,
                        input_type: "text".into(),
                        answer_channel: None,
                        value: AgenticResumeValue::Text {
                            value: "audience".into(),
                        },
                        agent_id: None,
                        goal_id: None,
                        cycle_id: None,
                    }),
                    Some((
                        if case == "wrong-scope" {
                            "another-owner"
                        } else {
                            "owner"
                        }
                        .into(),
                        "workspace".into(),
                    )),
                )
                .await
                .unwrap();
            if case == "wrong-scope" {
                assert!(response.status().is_client_error(), "{case}");
            } else {
                assert_eq!(
                    response.status(),
                    actix_web::http::StatusCode::CONFLICT,
                    "{case}"
                );
            }
            assert_eq!(
                api.full_pause_store()
                    .get_pending_for_execution(&execution_id)
                    .len(),
                1,
                "{case} cannot consume the exact source"
            );
            assert!(
                api.full_pause_store()
                    .load_serializable_from_disk()
                    .unwrap()
                    .iter()
                    .any(|pause| pause.pause_state.storage_key() == pause_key),
                "{case} must retain durable recovery"
            );
        }
        std::fs::write(owner_path, owner_bytes).unwrap();
        assert!(service
            .execution_has_external_output_owner(&scope, &task_id, &execution_id)
            .await
            .unwrap());
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn hitl_relentless_authentication_gate_applies_only_to_its_registered_owner() {
        use crate::web_api::{
            respond_hitl_handler, AgenticResumeValue, MagicianV2Api, RespondHitlRequest,
        };
        use magician::magician_v2::execution::agentic::{
            ActionExecutors, AgenticPauseState, EnvironmentState, FullPauseData, ShellState,
            UserInputType,
        };
        let _authority = magician::magician_v2::execution::trusted_store::install_process_authority(
            Arc::new(magician::magician_v2::execution::trusted_store::TrustAuthority::new()),
        );
        let temp = tempfile::tempdir().unwrap();
        let counter = Arc::new(NoModelCalls(Default::default()));
        let (artifacts, orchestrator) =
            magician::magician_v2::test_support::build_test_artifact_v2_harness_with_llm(
                temp.path(),
                counter.clone(),
            );
        let controller = web::Data::new(RelentlessApi::new(
            artifacts.workspace().clone(),
            Arc::new(OperationLlmRouter::new(Some(text_routes()))),
            artifacts.clone(),
            orchestrator.clone(),
            None,
        ));
        let api = web::Data::new(MagicianV2Api::with_shared_pause_store(
            orchestrator.clone(),
            orchestrator.full_pause_store_handle().unwrap(),
        ));
        for owned in [false, true] {
            let (task_id, execution_id, _) = if owned {
                owned_root(
                    &artifacts,
                    &orchestrator,
                    Uuid::new_v4(),
                    &format!("relentless:{}:{}", Uuid::new_v4(), "a".repeat(64)),
                )
                .await
                .unwrap()
            } else {
                magician::magician_v2::task_run_factory::create_task_backed_direct_root_run(
                    &artifacts,
                    &orchestrator,
                    "owner",
                    "workspace",
                    "ordinary-thread",
                    Some("Ordinary task"),
                    "Produce local text",
                    "engineer",
                    magician::magician_v2::storage::TaskCreatedBy::User,
                    false,
                    false,
                )
                .await
                .unwrap()
            };
            let mut state = AgenticPauseState::new(
                1,
                "goal",
                "criteria",
                EnvironmentState::Shell(ShellState::default()),
                "history",
                6,
                1,
            )
            .with_observability(&execution_id, "plan", "step")
            .with_scope("owner", "workspace");
            state.task_id = Some(task_id);
            // An ordinary task keeps its root owner even when it asks for text.
            // Stage that actual owner in this boot so this fixture reaches the
            // input-shape gate rather than presenting an owner-stripped root.
            if !owned {
                state.root_execution_id = Some(execution_id.clone());
                state.stateless_parked_segment = Some(execution_id.clone());
                state.agent_id = Some("engineer".into());
                state.active_owner_agent_id = state.agent_id.clone();
            }
            let key = api.full_pause_store().store(FullPauseData::new(
                state,
                UserInputType::Text {
                    placeholder: None,
                    multiline: false,
                },
                ActionExecutors::new(
                    counter.clone(),
                    Arc::new(magician::magician_v2::prompts::PromptManager::new(
                        Arc::new(
                            magician::magician_v2::prompts::JsonPromptStorage::with_default_config(
                            )
                            .unwrap(),
                        ),
                    )),
                ),
                Some("Which audience?".into()),
                None,
            ));
            let request = TestRequest::default()
                .app_data(controller.clone())
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "workspace"))
                .to_http_request();
            // Scope headers reproduce the existing ordinary handler fixture;
            // they deliberately carry no verified owner identity for Relentless.
            let response = Box::pin(respond_hitl_handler(
                api.clone(),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                request,
                web::Path::from(key.clone()),
                web::Json(RespondHitlRequest {
                    source: "agentic".into(),
                    value: AgenticResumeValue::Password {
                        value: "shape-mismatch-canary".into(),
                    },
                    input_type: Some("password".into()),
                    channel: "web".into(),
                    selected_paths: vec![],
                    execution_id: Some(execution_id.clone()),
                    task_id: None,
                }),
            ))
            .await
            .unwrap();
            assert_eq!(
                response.status(),
                if owned {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::BAD_REQUEST
                }
            );
            assert_eq!(api.full_pause_store().get_pending_for_execution(&execution_id).len(), 1,
                "neither an unauthenticated controller answer nor an ordinary type mismatch can consume the pause");
        }
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn existing_task_policy_preserves_history_and_recovers_its_exact_owned_root() {
        use magician::magician_v2::task_run_factory::{
            create_existing_task_direct_root_run_with_output_owner, task_policy_context_hash,
        };
        let temp = tempfile::tempdir().unwrap();
        let counter = Arc::new(NoModelCalls(Default::default()));
        let (service, orchestrator) =
            magician::magician_v2::test_support::build_test_artifact_v2_harness_with_llm(
                temp.path(),
                counter.clone(),
            );
        let scope = ScopeRef::system_internal_unauthenticated("owner", "workspace");
        let prior = owned_root(&service, &orchestrator, Uuid::new_v4(), "prior-controller")
            .await
            .unwrap();
        let context_hash =
            task_policy_context_hash(&service.get_task(&scope, &prior.0).await.unwrap().manifest)
                .unwrap();
        let id = Uuid::new_v4();
        let start = || {
            create_existing_task_direct_root_run_with_output_owner(
                &service,
                &orchestrator,
                "owner",
                "workspace",
                &prior.0,
                &context_hash,
                "policy-controller",
                id,
            )
        };
        assert!(
            start().await.is_err(),
            "an unsettled ordinary task cannot be replaced"
        );
        service
            .persist_precomputed_execution_outcome(
                &scope,
                &prior.0,
                &prior.1,
                "prior-controller",
                ExecutionOutcomeSnapshot {
                    execution_status: "completed".into(),
                    task_status: "completed".into(),
                    outcome_type: "reviewed".into(),
                    outcome_summary: "Prior exact result".into(),
                    iterations_used: Some(1),
                    is_terminal: true,
                    completion_kind: None,
                    open_items: vec![],
                },
                "Prior exact result".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            task_policy_context_hash(&service.get_task(&scope, &prior.0).await.unwrap().manifest)
                .unwrap(),
            context_hash
        );
        let admitted = start().await.unwrap();
        assert_eq!(admitted.0, prior.0);
        assert_ne!(admitted.1, prior.1);
        assert_eq!(
            service
                .get_execution(&scope, &prior.0, &prior.1)
                .await
                .unwrap()
                .state
                .status,
            "completed"
        );
        assert_eq!(
            start().await.unwrap(),
            admitted,
            "retry must not create another root"
        );
        let mut task = service.get_task(&scope, &prior.0).await.unwrap();
        task.state.latest_root_execution_id = Some(prior.1.clone());
        service
            .workspace()
            .write_json_atomic_path(
                service
                    .workspace()
                    .task_state_path("owner", "workspace", &prior.0),
                &task.state,
            )
            .await
            .unwrap();
        assert_eq!(
            start().await.unwrap(),
            admitted,
            "crash recovery must repair the exact new-root link"
        );
        assert_eq!(
            service
                .get_task(&scope, &prior.0)
                .await
                .unwrap()
                .state
                .latest_root_execution_id
                .as_deref(),
            Some(admitted.1.as_str())
        );
        assert!(create_existing_task_direct_root_run_with_output_owner(
            &service,
            &orchestrator,
            "owner",
            "workspace",
            &prior.0,
            "stale-context",
            "policy-controller",
            id,
        )
        .await
        .is_err());
        assert!(create_existing_task_direct_root_run_with_output_owner(
            &service,
            &orchestrator,
            "owner",
            "workspace",
            &prior.0,
            &context_hash,
            "different-controller",
            id,
        )
        .await
        .is_err());
        orchestrator
            .set_execution_status_with_side_effects(
                &admitted.2,
                WaitingState::Cancelled,
                "cancel policy before admission",
            )
            .await
            .unwrap();
        assert_eq!(start().await.unwrap(), admitted);
        assert_eq!(
            orchestrator
                .get_execution_status(&admitted.2)
                .await
                .unwrap(),
            WaitingState::Cancelled
        );
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn root_creation_retry_repairs_links_and_preserves_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let counter = Arc::new(NoModelCalls(Default::default()));
        let (service, orchestrator) =
            magician::magician_v2::test_support::build_test_artifact_v2_harness_with_llm(
                temp.path(),
                counter.clone(),
            );
        let scope = ScopeRef::system_internal_unauthenticated("owner", "workspace");
        let id = Uuid::new_v4();
        let first = owned_root(&service, &orchestrator, id, "test-controller")
            .await
            .unwrap();
        let original_runtime = orchestrator.get_execution(&first.2).await.unwrap();
        let mut task = service.get_task(&scope, &first.0).await.unwrap();
        // Model a stopped creation between execution publication and linking
        // that execution from its task. Recovery must reuse the existing root.
        task.state.latest_root_execution_id = None;
        service
            .workspace()
            .write_json_atomic_path(
                service
                    .workspace()
                    .task_state_path("owner", "workspace", &first.0),
                &task.state,
            )
            .await
            .unwrap();
        assert_eq!(
            owned_root(&service, &orchestrator, id, "test-controller")
                .await
                .unwrap(),
            first
        );
        assert_eq!(
            service
                .get_task(&scope, &first.0)
                .await
                .unwrap()
                .state
                .latest_root_execution_id
                .as_deref(),
            Some(first.1.as_str())
        );
        assert_eq!(
            orchestrator
                .get_execution(&first.2)
                .await
                .unwrap()
                .created_at,
            original_runtime.created_at
        );
        assert!(
            owned_root(&service, &orchestrator, id, "different-proposal")
                .await
                .is_err()
        );
        orchestrator
            .set_execution_status_with_side_effects(
                &first.2,
                WaitingState::Cancelled,
                "cancel before admission",
            )
            .await
            .unwrap();
        assert_eq!(
            owned_root(&service, &orchestrator, id, "test-controller")
                .await
                .unwrap(),
            first
        );
        assert_eq!(
            orchestrator.get_execution_status(&first.2).await.unwrap(),
            WaitingState::Cancelled
        );
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn owned_task_output_excludes_callbacks_and_recovery_before_and_after_publication() {
        let temp = tempfile::tempdir().unwrap();
        let counter = Arc::new(NoModelCalls(Default::default()));
        let (service, orchestrator) =
            magician::magician_v2::test_support::build_test_artifact_v2_harness_with_llm(
                temp.path(),
                counter.clone(),
            );
        let scope = ScopeRef::system_internal_unauthenticated("owner", "workspace");
        for status in ["completed", "cancelled"] {
            let (task, execution, _) =
                owned_root(&service, &orchestrator, Uuid::new_v4(), "test-controller")
                    .await
                    .unwrap();
            assert!(service
                .claim_external_execution_output(&scope, &task, &execution, "other-controller")
                .await
                .is_err());
            let outcome = ExecutionOutcomeSnapshot {
                execution_status: status.into(),
                task_status: status.into(),
                outcome_type: "reviewed".into(),
                outcome_summary: "Exact draft accepted".into(),
                iterations_used: Some(1),
                is_terminal: true,
                completion_kind: None,
                open_items: vec![],
            };
            // Long output exercises the ordinary auxiliary LLM-summary path,
            // which must also defer to the external controller's shared meter.
            let text =
                "Exact reviewed text.\n<script>This remains plain text.</script>\n".repeat(8);
            // Cancellation may arrive before the controller can publish bytes.
            // Startup must also leave this live, owned root to its controller.
            if status == "cancelled" {
                orchestrator
                    .set_execution_status_with_side_effects(
                        &execution,
                        WaitingState::Cancelled,
                        "owner cancellation",
                    )
                    .await
                    .unwrap();
            }
            service
                .persist_external_execution_outcome(&scope, &task, &execution, outcome.clone())
                .await
                .unwrap();
            assert_eq!(
                service
                    .reconcile_stale_synthesis_at_startup()
                    .await
                    .unwrap(),
                0
            );
            assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert!(service
                .persist_precomputed_execution_outcome(
                    &scope,
                    &task,
                    &execution,
                    "other-controller",
                    outcome.clone(),
                    text.clone(),
                )
                .await
                .is_err());
            service
                .persist_precomputed_execution_outcome(
                    &scope,
                    &task,
                    &execution,
                    "test-controller",
                    outcome.clone(),
                    text.clone(),
                )
                .await
                .unwrap();
            // An ordinary runtime callback and startup recovery must preserve the
            // finalizer's output and must not schedule another paid model call.
            service
                .persist_external_execution_outcome(&scope, &task, &execution, outcome.clone())
                .await
                .unwrap();
            assert_eq!(
                service
                    .reconcile_stale_synthesis_at_startup()
                    .await
                    .unwrap(),
                0
            );
            service
                .persist_precomputed_execution_outcome(
                    &scope,
                    &task,
                    &execution,
                    "test-controller",
                    outcome,
                    text.clone(),
                )
                .await
                .unwrap();
            let projected = service.get_task(&scope, &task).await.unwrap();
            let output = projected
                .refs
                .outputs
                .iter()
                .find(|r| projected.state.primary_user_output_id.as_ref() == Some(&r.output_id))
                .unwrap();
            let bytes = std::fs::read(
                service
                    .workspace()
                    .task_dir("owner", "workspace", &task)
                    .join(&output.relative_path),
            )
            .unwrap();
            assert_eq!(bytes, text.as_bytes());
            assert_eq!(
                service
                    .reconcile_stale_task_summaries_at_startup()
                    .await
                    .unwrap(),
                0
            );
            assert_eq!(projected.state.status, status);
            let e = service
                .get_execution(&scope, &task, &execution)
                .await
                .unwrap();
            assert!(!e.state.synthesis_pending);
            assert_eq!(
                orchestrator.get_execution_status(&execution).await.unwrap(),
                if status == "completed" {
                    WaitingState::Completed
                } else {
                    WaitingState::Cancelled
                }
            );
            assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn unverified_headers_never_admit_an_owner_scope() {
        let request = TestRequest::default()
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "work"))
            .to_http_request();
        assert_eq!(
            owner_scope(&request).unwrap_err().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn verified_bearer_scope_cannot_be_rewritten_by_headers() {
        let request = TestRequest::default()
            .insert_header(("X-Workspace", "other"))
            .to_http_request();
        request
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test(
                "owner",
                Some("work"),
                VerifiedRequestAuthentication::MagicianBearer,
            ));
        assert_eq!(
            owner_scope(&request).unwrap_err().status(),
            StatusCode::FORBIDDEN
        );
        let request = TestRequest::default().to_http_request();
        request
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test(
                "owner",
                Some("work"),
                VerifiedRequestAuthentication::MagicianBearer,
            ));
        assert_eq!(
            owner_scope(&request).unwrap(),
            RunScope {
                principal: "owner".into(),
                workspace: "work".into()
            }
        );
    }

    #[test]
    fn expired_and_reserved_identities_cannot_read_or_mutate_runs() {
        let request = TestRequest::default().to_http_request();
        request
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test_at(
                "owner",
                Some("work"),
                VerifiedRequestAuthentication::MagicianBearer,
                Utc::now() - chrono::Duration::seconds(6),
            ));
        assert_eq!(
            owner_scope(&request).unwrap_err().status(),
            StatusCode::UNAUTHORIZED
        );
        request
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test(
                "system",
                Some("system"),
                VerifiedRequestAuthentication::TrustedLoopbackSingleUser,
            ));
        assert_eq!(
            owner_scope(&request).unwrap_err().status(),
            StatusCode::FORBIDDEN
        );
        request
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test(
                "anonymous",
                Some("default"),
                VerifiedRequestAuthentication::MagicianBearer,
            ));
        assert!(
            owner_scope(&request).is_ok(),
            "an adopted first-owner root is a user scope"
        );
    }

    #[test]
    fn workspace_headers_cannot_complete_an_unbound_verified_identity() {
        let request = TestRequest::default()
            .insert_header(("X-Workspace", "work"))
            .to_http_request();
        request
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test(
                "owner",
                None,
                VerifiedRequestAuthentication::CloudflareAccess,
            ));
        assert_eq!(
            owner_scope(&request).unwrap_err().status(),
            StatusCode::BAD_REQUEST
        );
    }
}
