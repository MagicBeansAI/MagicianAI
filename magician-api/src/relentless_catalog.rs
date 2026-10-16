//! Read-only, bounded catalogs over the existing scoped journal authorities.
//! Offset pages are live views, not snapshots: concurrent inserts can move rows.
use super::*;
use runtime_core::storage::{PaginationInfo, PaginationParams};
use serde_json::Value;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub q: Option<String>,
    pub status: Option<String>,
    pub view: Option<String>,
    pub agent_id: Option<String>,
    pub goal_id: Option<String>,
}
impl PageQuery {
    pub fn params(&self) -> std::result::Result<PaginationParams, HttpResponse> {
        if self.limit.is_some_and(|n| n == 0 || n > 100)
            || self.q.as_ref().is_some_and(|q| q.len() > 512)
            || self.agent_id.as_ref().is_some_and(|id| id.len() > 256)
        {
            return Err(HttpResponse::BadRequest().json(
                json!({"error":"limit must be 1–100; search and agent filters must be bounded"}),
            ));
        }
        Ok(PaginationParams {
            limit: self.limit.unwrap_or(20),
            offset: self.offset.unwrap_or(0),
        })
    }
    pub fn search(&self) -> &str {
        self.q.as_deref().unwrap_or("")
    }
    pub fn check_status(&self, allowed: &[&str]) -> std::result::Result<(), HttpResponse> {
        if self
            .status
            .as_deref()
            .is_some_and(|s| !allowed.contains(&s))
        {
            return Err(HttpResponse::BadRequest().json(json!({"error":"Unknown catalog status"})));
        }
        Ok(())
    }
}
pub(super) fn matches(query: &str, fields: &[&str]) -> bool {
    let text = fields.join(" ").to_lowercase();
    query
        .split_whitespace()
        .all(|term| text.contains(&term.to_lowercase()))
}
pub(super) fn page<T>(items: Vec<T>, params: &PaginationParams) -> (Vec<T>, PaginationInfo) {
    let total = items.len();
    let items = items
        .into_iter()
        .skip(params.offset)
        .take(params.limit)
        .collect();
    let pagination = PaginationInfo {
        total,
        limit: params.limit,
        offset: params.offset,
        has_more: params.offset.saturating_add(params.limit) < total,
    };
    (items, pagination)
}

pub(super) fn internal_trial(
    store: &magician::magician_v2::relentless::store::RunStore,
    run: &RunState,
) -> Result<bool> {
    let setup: magician::magician_v2::relentless_runtime::TextDraftSetup =
        serde_json::from_slice(&store.read_blob(run.id, &run.admission.source_hash)?)?;
    Ok(setup
        .guidance_origin
        .as_ref()
        .is_some_and(|origin| origin.is_trial()))
}

pub(super) fn group(run: &RunState, capture_review: bool, internal_trial: bool) -> &'static str {
    use magician::magician_v2::relentless::routing::ReviewRoute;
    match run.status {
        RunStatus::WaitingOwner => "attention",
        _ if capture_review => "review",
        RunStatus::Accepted if internal_trial => "complete",
        RunStatus::Accepted if run.handoff.is_some() || run.owner_approved == Some(false) => {
            "complete"
        },
        RunStatus::Accepted if run.admission.routing_policy.is_none() && run.routing.is_none() => {
            "complete"
        },
        RunStatus::Accepted
            if run.owner_approved.is_none()
                && run
                    .routing
                    .as_ref()
                    .is_some_and(|r| r.route == ReviewRoute::HumanReview) =>
        {
            "review"
        },
        RunStatus::Admitted
        | RunStatus::Attempting
        | RunStatus::Reviewing
        | RunStatus::Accepted => "active",
        _ => "stopped",
    }
}
pub(super) fn label(run: &RunState, group: &str, capture_review: bool) -> String {
    use magician::magician_v2::relentless::routing::HandoffKind;
    match group {
        "attention" if run.status == RunStatus::BudgetExhausted => "Review continuation".into(),
        "attention" => "Needs input".into(),
        "review" if capture_review => "Review capture".into(),
        "review" => "Review draft".into(),
        _ if run
            .handoff
            .as_ref()
            .is_some_and(|h| h.kind == HandoffKind::Capture) =>
        {
            "Captured".into()
        },
        _ if run.handoff.is_some() => "Completed".into(),
        _ if run.owner_approved == Some(false) => "Handoff declined".into(),
        "complete" if run.status == RunStatus::Accepted => "Accepted".into(),
        _ if run.status == RunStatus::Accepted => "Preparing handoff".into(),
        _ => serde_json::to_value(run.status)
            .unwrap()
            .as_str()
            .unwrap()
            .replace('_', " "),
    }
}

pub(super) async fn catalog(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    query: web::Query<PageQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let params = match query.params() {
        Ok(p) => p,
        Err(r) => return r,
    };
    if let Err(r) = query.check_status(&[
        "all",
        "brief",
        "review",
        "attention",
        "active",
        "complete",
        "stopped",
    ]) {
        return r;
    }
    let view = query.view.as_deref().unwrap_or("inbox");
    if !["inbox", "runs"].contains(&view) {
        return HttpResponse::BadRequest().json(json!({"error":"Unknown catalog view"}));
    }
    match catalog_view(&api, &scope, &query, &params, view).await {
        Ok(view) => HttpResponse::Ok()
            .insert_header((CACHE_CONTROL, "no-store"))
            .json(view),
        Err(e) => failure(e),
    }
}
pub(super) async fn catalog_view(
    api: &RelentlessApi,
    scope: &RunScope,
    query: &PageQuery,
    params: &PaginationParams,
    view: &str,
) -> Result<Value> {
    let store = api.store(scope);
    // Keep only compact projections while scanning; full histories are loaded by
    // /runs/{id}, never included in a catalog page or retained for every row.
    let rows = io(move || {
        store.list_projected_results(|run| {
            Ok((
                run.id,
                run.started_at,
                run.contract.definition().goal.clone(),
                run.admission.task_id.clone(),
                run.status,
                run.attempts.len(),
                run.effective_budget()?.max_attempts,
                run.budget.spend()?,
                group(&run, false, false),
                label(&run, group(&run, false, false), false),
            ))
        })
    })
    .await?;
    let proposals = api.pending_proposals(scope).await?;
    let agents: std::collections::BTreeSet<_> = proposals
        .iter()
        .map(|p| p.origin.agent_id.clone())
        .collect();
    let mut counts = BTreeMap::from([
        ("review", 0usize),
        ("attention", 0),
        ("active", 0),
        ("complete", 0),
        ("stopped", 0),
        ("brief", proposals.len()),
    ]);
    let status = query.status.as_deref().unwrap_or("all");
    let mut items = Vec::new();
    // Proposals precede runs, each category has a stable order with an ID tie-break.
    let mut proposals = proposals;
    proposals.sort_by(|a, b| a.origin.request_id.cmp(&b.origin.request_id));
    if view == "inbox" && ["all", "brief"].contains(&status) {
        for p in proposals {
            if query
                .agent_id
                .as_deref()
                .is_some_and(|id| id != p.origin.agent_id)
                || !matches(
                    query.search(),
                    &[
                        &p.proposal.goal,
                        &p.origin.agent_id,
                        &p.origin.task_id,
                        &p.origin.request_id,
                    ],
                )
            {
                continue;
            }
            items.push(json!({"kind":"proposal","item":{"goal":p.proposal.goal,"origin":p.origin,
                "reference_count":p.proposal.sources.len(),"maximum_cost_microusd":p.proposal.budget.max_cost_microusd}}));
        }
    }
    let mut unavailable = Vec::new();
    for (id, row) in rows {
        let (
            id,
            started_at,
            goal,
            task_id,
            run_status,
            attempt_count,
            max_attempts,
            spend,
            mut run_group,
            mut run_label,
        ) = match row {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(?scope, run_id=%id, %error, "Relentless catalog cannot verify run");
                unavailable.push(id);
                continue;
            },
        };
        if run_status == RunStatus::BudgetExhausted {
            let reader = api.store(scope);
            let run = io(move || reader.load(id)).await?;
            match api.budget_continuation_view(&run).await {
                Ok(view) if view["available"] == true => {
                    run_group = "attention";
                    run_label = label(&run, run_group, false);
                },
                // Retained history grants no active ownership. Closed, archived
                // and qualification roots remain stopped in the review inbox.
                Ok(_) => {},
                Err(error) => {
                    tracing::warn!(?scope,run_id=%id,%error,"Budget history has no verified active continuation")
                },
            }
        }
        // A capture can require review after the run has already handed off.
        // Read the scoped capture receipt only for accepted runs; no artifact bytes escape.
        if run_status == RunStatus::Accepted {
            let store = api.store(scope);
            let (run, internal_trial) = match io(move || {
                let run = store.load(id)?;
                let trial = internal_trial(&store, &run)?;
                Ok((run, trial))
            })
            .await
            {
                Ok(run) => run,
                Err(error) => {
                    tracing::warn!(?scope, run_id=%id, %error, "Relentless catalog cannot reload accepted run");
                    unavailable.push(id);
                    continue;
                },
            };
            let capture = match api.capture_handoff_view(&run).await {
                Ok(view) => view,
                Err(error) => {
                    tracing::warn!(?scope, run_id=%id, %error, "Relentless catalog cannot verify capture receipt");
                    unavailable.push(id);
                    continue;
                },
            };
            let capture_review = Some(capture)
                .and_then(|v| {
                    v.pointer("/capture/status")
                        .and_then(Value::as_str)
                        .map(|s| s == "awaiting_review")
                })
                .unwrap_or(false);
            if capture_review || internal_trial {
                run_group = group(&run, capture_review, internal_trial);
                run_label = label(&run, run_group, capture_review);
            }
        }
        *counts.get_mut(run_group).expect("known run group") += 1;
        if (view == "inbox" && !["review", "attention"].contains(&run_group))
            || (status != "all" && status != run_group)
            || !matches(
                query.search(),
                &[&goal, &id.to_string(), &task_id, &run_label],
            )
        {
            continue;
        }
        items.push(json!({"kind":"run","item":{"id":id,"started_at":started_at,"goal":goal,"task_id":task_id,
            "status":run_status,"group":run_group,"label":run_label,"attempt_count":attempt_count,"max_attempts":max_attempts,"spend":spend}}));
    }
    unavailable.sort();
    for id in unavailable {
        *counts.get_mut("attention").unwrap() += 1;
        if ["all", "attention"].contains(&status)
            && matches(query.search(), &[&id.to_string(), "Stored run unavailable"])
        {
            items.push(json!({"kind":"unavailable","item":{"id":id,"reason":"Stored run could not be verified. Recovery and approval remain blocked."}}));
        }
    }
    let (items, pagination) = page(items, params);
    Ok(json!({"items":items,"pagination":pagination,"counts":counts,"agents":agents}))
}

pub(super) async fn proposal_get(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<String>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match api.pending_proposals(&scope).await {
        Ok(items) => match items.into_iter().find(|p| p.origin.request_id == *id) {
            Some(p) => HttpResponse::Ok()
                .insert_header((CACHE_CONTROL, "no-store"))
                .json(p),
            None => HttpResponse::NotFound().json(json!({"error":"Pending proposal missing"})),
        },
        Err(e) => failure(e),
    }
}
pub(super) async fn captures(
    req: HttpRequest,
    api: web::Data<RelentlessApi>,
    id: web::Path<Uuid>,
    query: web::Query<PageQuery>,
) -> HttpResponse {
    let scope = match owner_scope(&req) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let params = match query.params() {
        Ok(p) => p,
        Err(r) => return r,
    };
    if let Err(r) = query.check_status(&["all", "awaiting_review", "captured", "rejected"]) {
        return r;
    }
    match api
        .owner_campaign_captures(
            &scope,
            *id,
            &params,
            query.status.as_deref().unwrap_or("awaiting_review"),
        )
        .await
    {
        Ok(v) => HttpResponse::Ok()
            .insert_header((CACHE_CONTROL, "no-store"))
            .json(v),
        Err(e) => failure(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_pages_report_filtered_totals_and_saturate_offsets() {
        let params = PageQuery::default().params().unwrap();
        assert_eq!(params.limit, 20);
        let (rows, info) = page(
            vec![1, 2, 3],
            &PaginationParams {
                limit: 2,
                offset: 1,
            },
        );
        assert_eq!(rows, vec![2, 3]);
        assert_eq!(info.total, 3);
        assert!(!info.has_more);
        let (rows, info) = page(
            vec![1],
            &PaginationParams {
                limit: 100,
                offset: usize::MAX,
            },
        );
        assert!(rows.is_empty());
        assert_eq!(info.total, 1);
        assert!(!info.has_more);
        assert!(PageQuery {
            limit: Some(0),
            ..Default::default()
        }
        .params()
        .is_err());
        assert!(PageQuery {
            limit: Some(101),
            ..Default::default()
        }
        .params()
        .is_err());
        assert!(PageQuery {
            q: Some("a".repeat(513)),
            ..Default::default()
        }
        .params()
        .is_err());
        assert!(matches("CEO launch", &["launch brief", "ceo"]));
        assert!(!matches("CEO unknown", &["launch brief", "ceo"]));
    }
}
