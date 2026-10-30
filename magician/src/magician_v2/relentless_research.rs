//! Bounded public reads for the existing native task actor. No new planner or
//! browser driver: compiled web tools use the existing pinned public fetcher.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use magician_relentless::{
    contract::CollectedSourceLimits,
    store::{CollectedReadClaim, RunStore},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::content_sources::{
    ConditionalHttpRequest, PublicHttpFetch, PublicHttpFetchPolicy, PublicHttpFetcher,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicResearchPolicy {
    pub maximum_requests: u32,
    pub maximum_response_bytes: u64,
    pub maximum_redirects: usize,
    pub timeout_secs: u64,
    #[serde(default)]
    pub allow_search: bool,
}

impl PublicResearchPolicy {
    fn fetch_policy(&self) -> Result<PublicHttpFetchPolicy> {
        Ok(PublicHttpFetchPolicy {
            max_response_bytes: usize::try_from(self.maximum_response_bytes)?,
            max_redirects: self.maximum_redirects,
            timeout: Duration::from_secs(self.timeout_secs),
            user_agent: "Magician public research".into(),
            accepted_media_types: BTreeSet::from([
                "text/html".into(),
                "text/plain".into(),
                "application/json".into(),
                "application/xhtml+xml".into(),
            ]),
            allow_missing_media_type: false,
        })
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.maximum_requests > 0,
            "public research requires a request ceiling"
        );
        self.fetch_policy()?.validate()?;
        self.limits()?.validate()
    }
    pub fn limits(&self) -> Result<CollectedSourceLimits> {
        Ok(CollectedSourceLimits {
            maximum_sources: self.maximum_requests,
            maximum_bytes: u64::from(self.maximum_requests)
                .checked_mul(self.maximum_response_bytes)
                .context("research byte ceiling overflow")?,
        })
    }
    pub fn hash(&self) -> Result<String> {
        self.validate()?;
        Ok(
            blake3::hash(&serde_json::to_vec(&("public-research-v1", self))?)
                .to_hex()
                .to_string(),
        )
    }
}

/// Typed runtime capability. It is cloned into this actor's scoped compiled
/// registry only; model arguments cannot create or replace it.
pub struct ResearchBoundary {
    owner: String,
    identity: Uuid,
    store: Arc<RunStore>,
    run_id: Uuid,
    holder: String,
    policy: PublicResearchPolicy,
    policy_hash: String,
    deadline: DateTime<Utc>,
    cancel: CancellationToken,
    open: AtomicBool,
    gate: tokio::sync::Mutex<()>,
}

fn boundaries() -> &'static Mutex<BTreeMap<Uuid, Weak<ResearchBoundary>>> {
    static BOUNDARIES: OnceLock<Mutex<BTreeMap<Uuid, Weak<ResearchBoundary>>>> = OnceLock::new();
    BOUNDARIES.get_or_init(Default::default)
}

pub async fn quiesce(owner: &str) {
    let weak: Vec<_> = boundaries()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect();
    let active: Vec<_> = weak
        .into_iter()
        .filter_map(|boundary| boundary.upgrade())
        .filter(|boundary| boundary.owner == owner)
        .collect();
    for boundary in active {
        boundary.close_and_drain().await;
    }
}

impl Drop for ResearchBoundary {
    fn drop(&mut self) {
        self.open.store(false, Ordering::Release);
        self.cancel.cancel();
        boundaries()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.identity);
    }
}

impl ResearchBoundary {
    pub fn admit(
        owner: String,
        store: Arc<RunStore>,
        run_id: Uuid,
        holder: String,
        policy: PublicResearchPolicy,
        deadline: DateTime<Utc>,
        cancel: CancellationToken,
    ) -> Result<Arc<Self>> {
        let policy_hash = policy.hash()?;
        ensure!(
            !cancel.is_cancelled() && deadline > Utc::now(),
            "research admission window closed"
        );
        let boundary = Arc::new(Self {
            owner,
            identity: Uuid::new_v4(),
            store,
            run_id,
            holder,
            policy,
            policy_hash,
            deadline,
            cancel,
            open: AtomicBool::new(true),
            gate: tokio::sync::Mutex::new(()),
        });
        boundaries()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(boundary.identity, Arc::downgrade(&boundary));
        Ok(boundary)
    }

    pub async fn close_and_drain(&self) {
        self.open.store(false, Ordering::Release);
        self.cancel.cancel();
        let _joined = self.gate.lock().await;
    }

    pub async fn sources(&self) -> Result<std::collections::BTreeMap<String, Vec<u8>>> {
        let store = self.store.clone();
        let id = self.run_id;
        let hash = self.policy_hash.clone();
        tokio::task::spawn_blocking(move || store.collected_read_sources(id, &hash)).await?
    }

    pub async fn read(&self, args: Value, search: bool) -> Result<Value> {
        let _serial = self.gate.lock().await;
        ensure!(
            self.open.load(Ordering::Acquire)
                && !self.cancel.is_cancelled()
                && self.deadline > Utc::now(),
            "research window closed"
        );
        ensure!(
            !search || self.policy.allow_search,
            "search was not admitted"
        );
        let input = args
            .get(if search { "query" } else { "url" })
            .and_then(Value::as_str)
            .context("research needs a URL or query")?;
        ensure!(!input.trim().is_empty(), "research input is empty");
        let url = if search {
            format!(
                "https://html.duckduckgo.com/html/?q={}",
                urlencoding::encode(input)
            )
        } else {
            input.to_owned()
        };
        let url = super::content_sources::validate_public_http_url(&url)?.to_string();
        let allowed = if search {
            string_list(&args, "allowed_domains")?
        } else {
            vec![]
        };
        let blocked = if search {
            string_list(&args, "blocked_domains")?
        } else {
            vec![]
        };
        let request_hash = blake3::hash(&serde_json::to_vec(&(
            "public-read-v1",
            &self.policy_hash,
            search,
            &url,
            &allowed,
            &blocked,
        ))?)
        .to_hex()
        .to_string();
        let store = self.store.clone();
        let id = self.run_id;
        let holder = self.holder.clone();
        let policy_hash = self.policy_hash.clone();
        let request = request_hash.clone();
        let policy = self.policy.clone();
        let claim = tokio::task::spawn_blocking(move || {
            store.claim_collected_read(
                id,
                &holder,
                &policy_hash,
                &request,
                policy.maximum_requests,
                policy.maximum_response_bytes,
                Utc::now(),
            )
        })
        .await??;
        match claim {
            CollectedReadClaim::Completed(bytes) => return Ok(serde_json::from_slice(&bytes)?),
            CollectedReadClaim::Unknown => {
                return Ok(
                    json!({"status":"unknown", "reason":"Prior public read has no sealed receipt; it will not be resent", "request_hash":request_hash}),
                )
            },
            CollectedReadClaim::Reserved => {},
        }
        let fetcher = PublicHttpFetcher::new(self.policy.fetch_policy()?)?;
        let conditional_request = ConditionalHttpRequest::default();
        let fetched = tokio::select! {
            _ = self.cancel.cancelled() => return Err(anyhow::anyhow!("public read cancelled; reservation retained")),
            _ = magician_relentless::controller::wait_for_deadline(self.deadline) => return Err(anyhow::anyhow!("public read deadline reached")),
            fetched = fetcher.fetch(&url, &conditional_request) => fetched,
        };
        let (result, source) = match fetched {
            Ok(PublicHttpFetch::Modified {
                final_url,
                media_type,
                body,
                ..
            }) => {
                let raw_hash = blake3::hash(&body).to_hex().to_string();
                let writer = self.store.clone();
                let id = self.run_id;
                let raw = body.clone();
                tokio::task::spawn_blocking(move || writer.put_blob(id, &raw)).await??;
                let text = std::str::from_utf8(&body).context("public read is not UTF-8")?;
                if search {
                    let results =
                        super::execution::compiled_handlers::web_search::parse_public_web_results(
                            text, &allowed, &blocked, 10,
                        )?;
                    (
                        json!({"status":"ok","query":input,"results":results.into_iter().map(|r| json!({"title":r.title,"url":r.url,"snippet":r.snippet})).collect::<Vec<_>>(),
                        "provenance":{"requested_url":url,"final_url":final_url,"body_hash":raw_hash,"request_hash":request_hash}, "claim_eligible":false}),
                        None,
                    )
                } else {
                    let content = if matches!(
                        media_type.as_deref(),
                        Some("text/html" | "application/xhtml+xml")
                    ) {
                        super::execution::compiled_handlers::web_fetch::strip_html_to_text(text)
                    } else {
                        text.to_owned()
                    };
                    let source_id = format!("collected:{request_hash}");
                    let evidence = format!("Public page\nRequested URL: {url}\nFinal URL: {final_url}\nFetched bytes hash: {raw_hash}\nContent:\n{content}").into_bytes();
                    ensure!(
                        evidence.len() as u64 <= self.policy.maximum_response_bytes,
                        "prepared public source exceeds its byte ceiling"
                    );
                    (
                        json!({"status":"ok","url":final_url,"content":content,"source_id":source_id,
                        "provenance":{"requested_url":url,"body_hash":raw_hash,"request_hash":request_hash},"claim_eligible":true}),
                        Some(evidence),
                    )
                }
            },
            Ok(PublicHttpFetch::NotModified { .. }) => (
                json!({"status":"unknown","reason":"Public read returned no representation"}),
                None,
            ),
            Err(error) => (json!({"status":"error","reason":error.to_string()}), None),
        };
        let bytes = serde_json::to_vec(&result)?;
        ensure!(
            bytes.len() as u64 <= self.policy.maximum_response_bytes,
            "public tool result exceeds its byte ceiling"
        );
        let store = self.store.clone();
        let id = self.run_id;
        let policy_hash = self.policy_hash.clone();
        tokio::task::spawn_blocking(move || {
            store.finish_collected_read(id, &policy_hash, &request_hash, &bytes, source.as_deref())
        })
        .await??;
        Ok(result)
    }
}

fn string_list(args: &Value, key: &str) -> Result<Vec<String>> {
    args.get(key)
        .map(|value| {
            value
                .as_array()
                .context("domain filter must be an array")?
                .iter()
                .map(|v| {
                    Ok(v.as_str()
                        .context("domain filter must contain strings")?
                        .to_owned())
                })
                .collect()
        })
        .unwrap_or_else(|| Ok(vec![]))
}
