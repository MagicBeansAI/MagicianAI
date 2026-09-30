//! One-time built-in UI credential prompts and destination-bound trusted fill.
//! The model supplies only field metadata and receives only closed status codes.
//!
//! Since P4 a field may name a reference (`value: "[REF:<key>]"`) to material
//! the run already holds — a password collected through HITL, or a one-time
//! code — instead of prompting for it: the fill is the typed, destination-bound
//! sink for that material. A code is reserved against this origin, consumed as
//! the fill starts, and never delivered twice; a value the model typed itself
//! is refused. Several fields naming the same reference are a segmented input
//! (one box per character): the material is resolved once and split across
//! them inside this one operation — the model never holds a per-digit
//! reference. What was delivered joins the run's scrub set, and image
//! captures are withheld until the page moves on (every observation, after a
//! split, until a navigation).
use super::{AgentBrowserSession, ConnectionMode};
use crate::magician_v2::{
    execution::{
        actions::ActionResult,
        primitive_dispatch::{
            exec_ctx::{CAPTURE_WITHHELD_PAGE, CAPTURE_WITHHELD_PIXELS},
            PrimitiveExecCtx,
        },
    },
    secrets::{sinks::placeholders, KnownSecretValues, OneTimeClaim, SecretStore},
    user_requests::{RequestOption, UserRequest, UserRequestService},
};
use magicvault_effect::{
    canonical_origin, cdp::CdpBrowser, BrowserAdapter, MaterialField, TargetFilter,
};
use magicvault_protocol::FieldState;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{atomic::AtomicU8, Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

const DEADLINE_SECS: u64 = 180;
/// The store's audit vocabulary for this operation's one-time claims.
const CLAIM_OPERATION: &str = "browser:secure_prompt_fill";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Field {
    field_name: String,
    css: String,
    /// A reference to material the run holds (`[REF:<key>]`), or absent to
    /// prompt the user for this field. Never a value.
    #[serde(default)]
    value: Option<String>,
}
impl Field {
    /// The referenced key, when `value` is exactly one reference marker.
    fn reference(&self) -> Option<String> {
        let value = self.value.as_deref()?.trim();
        match placeholders(value).as_slice() {
            [(marker, key)] if marker == value => Some(key.clone()),
            _ => None,
        }
    }
    /// A `value` that is not a lone reference is a typed value: refused.
    fn value_is_admissible(&self) -> bool {
        self.value.is_none() || self.reference().is_some()
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    top_origin: String,
    tab_id: Option<String>,
    fields: Vec<Field>,
    connection_mode: Option<String>,
}
impl Arguments {
    fn valid(&self) -> bool {
        canonical_origin(&self.top_origin).as_deref() == Ok(self.top_origin.as_str())
            && self.connection_mode.as_deref().is_none_or(|v| v == "cdp")
            && !self.fields.is_empty()
            && self.fields.len() <= 8
            && self.fields.iter().all(|f| {
                f.value_is_admissible()
                    && !f.field_name.is_empty()
                    && f.field_name.len() <= 64
                    && f.field_name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                    && !f.css.is_empty()
                    && f.css.len() <= 512
                    && !f.css.chars().any(char::is_control)
            })
            && self
                .fields
                .iter()
                .map(|f| &f.field_name)
                .collect::<HashSet<_>>()
                .len()
                == self.fields.len()
            && self
                .fields
                .iter()
                .map(|f| &f.css)
                .collect::<HashSet<_>>()
                .len()
                == self.fields.len()
            && magicvault_effect::valid_target_filter(&self.filter())
    }
    fn filter(&self) -> TargetFilter {
        TargetFilter {
            top_origin: Some(self.top_origin.clone()),
            tab_id: self.tab_id.clone(),
        }
    }
}
fn status(code: &str) -> Value {
    json!({"status":code,"saved":false,"submitted":false})
}
fn status_with_reason(code: &str, reason: &str) -> Value {
    json!({"status":code,"reason":reason,"saved":false,"submitted":false})
}

/// The run's material this fill may deliver (P4).
struct MaterialContext<'a> {
    store: Option<&'a SecretStore>,
    scope: Option<&'a str>,
    delivered: Option<&'a Arc<Mutex<KnownSecretValues>>>,
    capture_withheld: Option<&'a Arc<AtomicU8>>,
    /// The challenge the run has observed standing at a destination, if any.
    /// The fill names it in its claim when it matches this page's origin, so a
    /// code collected for the challenge this origin raised BEFORE is refused
    /// against the one standing now — the same rule the HTTP lane applies at
    /// dispatch (`DispatchResolver::observed_challenge`).
    pending_challenge: Option<
        &'a Arc<
            std::sync::Mutex<
                Option<crate::magician_v2::secrets::challenge::AuthenticationChallenge>,
            >,
        >,
    >,
}

/// One referenced field's material, held until the fill starts. An
/// unconsumed reservation is released when this is dropped — a cancel, a
/// deadline, a panic between reserve and fill all count as nothing sent.
struct Referenced<'a> {
    key: String,
    value: Zeroizing<String>,
    /// A one-time reservation to consume as the fill starts (a code), or
    /// `None` for a password read from the scope.
    reservation_id: Option<String>,
    /// Whether this is ONE-TIME material the store bound to exactly this
    /// origin, so the user's "use once" confirmation is already given by the
    /// challenge. False for anything the fill does not spend.
    bound_to_origin: bool,
    store: Option<&'a SecretStore>,
}

impl Drop for Referenced<'_> {
    fn drop(&mut self) {
        if let (Some(store), Some(reservation_id)) = (self.store, self.reservation_id.take()) {
            let _ = store.release_one_time(
                &reservation_id,
                crate::magician_v2::secrets::PreDispatchFailure {
                    reason: "the fill did not start".to_string(),
                },
            );
        }
    }
}

impl<'a> MaterialContext<'a> {
    /// The challenge the run can still SEE at this origin, when there is one.
    /// Never a copy of the registration, which would assert nothing.
    fn observed_challenge_for(&self, origin: &str) -> Option<String> {
        self.pending_challenge
            .and_then(|pending| pending.lock().ok())
            .and_then(|pending| {
                pending
                    .as_ref()
                    .filter(|challenge| challenge.destination == origin)
                    .map(|challenge| challenge.challenge_id.clone())
            })
    }

    fn resolve(&self, key: &str, origin: &str) -> Result<Referenced<'a>, String> {
        let (Some(store), Some(scope)) = (self.store, self.scope) else {
            return Err("no material is available to this run".to_string());
        };
        // Material a challenge bound is delivered only to that origin (P4).
        let bound = crate::magician_v2::secrets::sinks::bound_destination(store, scope, key);
        if !crate::magician_v2::secrets::sinks::destination_admits(bound.as_deref(), Some(origin)) {
            return Err(format!(
                "it is bound to {} and this page is {origin}",
                bound.unwrap_or_default()
            ));
        }
        // Only ONE-TIME material can stand in for the user's "use once": a
        // password bound to this origin is still an execution-scoped secret
        // that survives the fill, so consent for it is asked, not inferred.
        let bound_to_origin = bound.as_deref() == Some(origin);
        if store.one_time_state(scope, key).is_some() {
            let reservation = store
                .reserve_one_time(
                    scope,
                    key,
                    OneTimeClaim {
                        operation: CLAIM_OPERATION.to_string(),
                        destination: Some(origin.to_string()),
                        challenge_id: self.observed_challenge_for(origin),
                    },
                )
                .map_err(|error| error.to_string())?;
            let Some(reservation_id) = reservation.receipt.reservation_id.clone() else {
                return Err("the reservation is not held".to_string());
            };
            return Ok(Referenced {
                key: key.to_string(),
                value: reservation.value,
                reservation_id: Some(reservation_id),
                bound_to_origin,
                store: Some(store),
            });
        }
        let value = store.get_ephemeral_scoped(scope, key).ok_or_else(|| {
            "no material is registered for this reference in this run".to_string()
        })?;
        Ok(Referenced {
            key: key.to_string(),
            value: Zeroizing::new(value),
            reservation_id: None,
            bound_to_origin: false,
            store: None,
        })
    }

    /// Nothing was sent: the codes go back to available.
    fn release(&self, referenced: &mut [Referenced<'_>], reason: &str) {
        let Some(store) = self.store else { return };
        for item in referenced.iter_mut() {
            if let Some(reservation_id) = item.reservation_id.take() {
                let _ = store.release_one_time(
                    &reservation_id,
                    crate::magician_v2::secrets::PreDispatchFailure {
                        reason: reason.to_string(),
                    },
                );
            }
        }
    }

    /// The fill starts: every code is spent now, and every delivered value
    /// joins the run's scrub set before any observation can echo it. A value
    /// split across fields is recognisable in no later text, so every
    /// observation is withheld until a navigation, not only pixels.
    fn consume(&self, referenced: &mut [Referenced<'_>], split: bool) -> Result<(), String> {
        if let Some(store) = self.store {
            for item in referenced.iter_mut() {
                if let Some(reservation_id) = item.reservation_id.take() {
                    store
                        .consume_one_time(&reservation_id)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        if let Some(delivered) = self.delivered {
            if let Ok(mut delivered) = delivered.lock() {
                for item in referenced.iter() {
                    let delivery = format!("{}@{}", item.key, delivered.len());
                    delivered.insert(delivery, item.value.to_string());
                }
            }
        }
        if let Some(withheld) = self.capture_withheld {
            let level = if split {
                CAPTURE_WITHHELD_PAGE
            } else {
                CAPTURE_WITHHELD_PIXELS
            };
            withheld.fetch_max(level, std::sync::atomic::Ordering::AcqRel);
        }
        Ok(())
    }
}

/// The value one field of a segmented input receives: the character at its
/// position among the fields naming the same reference, or the whole value
/// when it is the only one. `None` when the material does not split across
/// that many fields.
fn segment(value: &str, position: usize, segments: usize) -> Option<String> {
    if segments == 1 {
        return Some(value.to_string());
    }
    let mut chars = value.chars();
    if chars.clone().count() != segments {
        return None;
    }
    chars.nth(position).map(String::from)
}

// Runtime-owned effect identity, not a model-authored idempotency key. Bounded,
// fail-closed process tombstones prevent replay after uncertain/partial fills.
fn claim(principal: &str, workspace: &str, effect: &str) -> bool {
    static ATTEMPTS: OnceLock<Mutex<HashSet<(String, String, String)>>> = OnceLock::new();
    let Ok(mut attempts) = ATTEMPTS.get_or_init(Default::default).lock() else {
        return false;
    };
    if attempts.len() >= 4096 {
        return false;
    }
    attempts.insert((principal.into(), workspace.into(), effect.into()))
}
struct ConnectionGuard(Arc<CdpBrowser>);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.disconnect();
    }
}

pub(crate) async fn dispatch(
    arguments: &Value,
    ctx: &PrimitiveExecCtx,
    session: &Arc<AgentBrowserSession>,
) -> ActionResult {
    let value = execute(arguments, ctx, session).await;
    ActionResult::Text {
        content: value.to_string(),
    }
}
async fn execute(
    arguments: &Value,
    ctx: &PrimitiveExecCtx,
    session: &Arc<AgentBrowserSession>,
) -> Value {
    let Ok(args) = serde_json::from_value::<Arguments>(arguments.clone()) else {
        return status("invalid_request");
    };
    if !args.valid() {
        return status("invalid_request");
    }
    let (Some(principal), Some(workspace), Some(effect), Some(service)) = (
        ctx.principal.as_deref(),
        ctx.workspace.as_deref(),
        ctx.effect_id.as_deref(),
        ctx.user_request_service.as_ref(),
    ) else {
        return status("secure_prompt_unavailable");
    };
    if principal.is_empty()
        || workspace.is_empty()
        || effect.is_empty()
        || ctx
            .execution_id
            .as_deref()
            .is_none_or(|id| id.trim().is_empty())
    {
        return status("secure_prompt_unavailable");
    }
    let ConnectionMode::Cdp { url } = session.mode() else {
        return status("unsupported_transport");
    };
    // Only the operator-configured endpoint. No model-selected CDP server may
    // impersonate the destination and solicit credentials.
    let configured = if ctx.browser_cdp_url == super::DEFAULT_MAGICUTOR_PROXY_URL {
        format!(
            "ws://127.0.0.1:3003/devtools/browser/{}",
            session.session_id()
        )
    } else {
        ctx.browser_cdp_url.clone()
    };
    if url != &configured {
        return status("unsupported_transport");
    }
    let endpoint = if ctx.browser_cdp_url == super::DEFAULT_MAGICUTOR_PROXY_URL {
        let Some(alias) = magicutor::server::cdp_scope_alias::encode(session.session_id()) else {
            return status("unsupported_transport");
        };
        format!("ws://127.0.0.1:3003/devtools/browser/{alias}")
    } else {
        configured
    };
    if magicvault_effect::cdp::validate_endpoint(&endpoint).is_err() {
        return status("unsupported_transport");
    }
    if !claim(principal, workspace, effect) {
        return status("already_attempted_or_capacity");
    }
    let cancel = ctx
        .cancellation_token
        .as_ref()
        .map(CancellationToken::child_token)
        .unwrap_or_default();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(DEADLINE_SECS);
    let work = async {
        if session.ensure_connected().await.is_err() {
            return status("browser_unavailable");
        }
        let Ok(adapter) = CdpBrowser::connect(&endpoint).await else {
            return status("browser_unavailable");
        };
        let _disconnect = ConnectionGuard(adapter.clone());
        let prompt = PromptContext {
            service,
            principal,
            workspace,
            execution_id: ctx.execution_id.clone(),
            task_id: ctx.task_id.clone(),
            owner_agent_id: ctx.agent_id.clone(),
            chat_session_id: ctx.chat_session_id.clone(),
            deadline,
        };
        let material = MaterialContext {
            store: ctx.secret_store.as_deref(),
            scope: ctx.ephemeral_secret_scope_id.as_deref(),
            delivered: ctx.delivered_secret_values.as_ref(),
            capture_withheld: ctx.browser_capture_withheld.as_ref(),
            pending_challenge: ctx.pending_challenge.as_ref(),
        };
        run(adapter.as_ref(), &args, &prompt, &material, cancel.clone()).await
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => status("cancelled_or_uncertain"),
        value = tokio::time::timeout(Duration::from_secs(DEADLINE_SECS), work) => value.unwrap_or_else(|_| status("expired_or_uncertain")),
    }
}
struct PromptContext<'a> {
    service: &'a Arc<UserRequestService>,
    principal: &'a str,
    workspace: &'a str,
    execution_id: Option<String>,
    task_id: Option<String>,
    owner_agent_id: Option<String>,
    chat_session_id: Option<String>,
    deadline: tokio::time::Instant,
}
impl PromptContext<'_> {
    fn request(
        &self,
        question: String,
        mut context: Value,
        options: Vec<RequestOption>,
    ) -> UserRequest {
        context["owner_agent_id"] = json!(self.owner_agent_id);
        context["chat_session_id"] = json!(self.chat_session_id);
        UserRequest {
            id: String::new(),
            request_type: "secure_browser_input".into(),
            question,
            options,
            principal: self.principal.into(),
            workspace: self.workspace.into(),
            context,
            source: "browser_secure_prompt_fill".into(),
            execution_id: self.execution_id.clone(),
            task_id: self.task_id.clone(),
            timeout_secs: self
                .deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .as_secs()
                .clamp(1, DEADLINE_SECS),
            default_on_timeout: "cancel".into(),
            created_at: 0,
            sensitive: None,
        }
    }
}
async fn run(
    adapter: &dyn BrowserAdapter,
    args: &Arguments,
    prompt: &PromptContext<'_>,
    material_ctx: &MaterialContext<'_>,
    cancel: CancellationToken,
) -> Value {
    let targets = match adapter
        .targets_filtered(&args.filter(), cancel.clone())
        .await
    {
        Ok(targets) => targets
            .into_iter()
            .filter(|t| {
                t.is_main_frame
                    && t.valid()
                    && magicvault_effect::matches_target_filter(t, &args.filter())
            })
            .collect::<Vec<_>>(),
        Err(_) => return status("target_unavailable"),
    };
    if targets.len() != 1 {
        return status(if targets.is_empty() {
            "target_unavailable"
        } else {
            "ambiguous_target"
        });
    }
    let target = &targets[0];
    // Referenced material first, each reference resolved once however many
    // fields name it, reserved against this exact origin, so a code that
    // expired or was spent refuses before the user is asked for anything
    // else.
    let mut referenced: Vec<Referenced<'_>> = Vec::new();
    for field in &args.fields {
        let Some(key) = field.reference() else {
            continue;
        };
        if referenced.iter().any(|item| item.key == key) {
            continue;
        }
        match material_ctx.resolve(&key, &target.top_origin) {
            Ok(item) => referenced.push(item),
            Err(reason) => {
                material_ctx.release(&mut referenced, "another field's material was unavailable");
                return status_with_reason("material_unavailable", &reason);
            },
        }
    }
    // Several fields naming one reference are a segmented input: one
    // character per field, in the order given, inside this one operation.
    let segments = |key: &str| {
        args.fields
            .iter()
            .filter(|field| field.reference().as_deref() == Some(key))
            .count()
    };
    let split = referenced.iter().any(|item| segments(&item.key) > 1);
    let mut material = Vec::with_capacity(args.fields.len());
    let mut prompted = 0usize;
    let mut positions: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for field in &args.fields {
        if let Some(key) = field.reference() {
            let Some(item) = referenced.iter().find(|item| item.key == key) else {
                material_ctx.release(&mut referenced, "field material missing");
                return status("invalid_request");
            };
            let position = positions.entry(key.clone()).or_default();
            let Some(value) = segment(&item.value, *position, segments(&key)) else {
                material_ctx.release(
                    &mut referenced,
                    "the material does not split across the fields",
                );
                return status_with_reason(
                    "invalid_request",
                    &format!(
                        "the material for `{key}` does not split across {} fields",
                        segments(&key)
                    ),
                );
            };
            *position += 1;
            material.push(MaterialField {
                css: field.css.clone(),
                value,
            });
            continue;
        }
        prompted += 1;
        let request = prompt.request(
            format!("Enter {} for {} (tab {}). Used once to fill {}. It will not be saved or sent to the agent.", field.field_name, target.top_origin, target.tab, field.css),
            json!({"top_origin":target.top_origin,"tab_id":target.tab,"field_name":field.field_name,"css":field.css}), vec![]);
        let Some(mut value) = prompt.service.ask_sensitive_once(request).await else {
            material_ctx.release(&mut referenced, "the user cancelled the prompt");
            return status("cancelled");
        };
        material.push(MaterialField {
            css: field.css.clone(),
            value: std::mem::take(&mut *value),
        });
    }
    // The user's "use once" is already given when every field is a code the
    // store bound to this exact origin (the challenge named the destination);
    // anything else — a prompted field, a password, an unbound code — asks.
    let confirmation_given = prompted == 0
        && !referenced.is_empty()
        && referenced.iter().all(|item| item.bound_to_origin);
    if !confirmation_given {
        let request = prompt.request(
            format!("Use these {} fields once on {} (tab {})? This fills the selected fields without saving credentials or submitting the form.", material.len(), target.top_origin, target.tab),
            json!({"top_origin":target.top_origin,"tab_id":target.tab,"credential_lifetime":"one_time"}),
            vec![RequestOption { id:"allow_once".into(),label:"Use once".into(),requires_input:false }, RequestOption { id:"cancel".into(),label:"Cancel".into(),requires_input:false }]);
        if !prompt.service.ask_secure_confirmation(request).await || cancel.is_cancelled() {
            material_ctx.release(&mut referenced, "the user did not confirm the fill");
            return status("cancelled");
        }
    }
    // Immediately before the codes are spent: the page must still be the one
    // the material was reserved for — same single target, same origin and
    // document — after however long the confirmation took. A changed page
    // releases; the adapter's own recheck inside `fill` guards the mutation
    // itself, but a miss there is after `consume` and spends the code.
    let still_there = adapter
        .targets_filtered(&args.filter(), cancel.clone())
        .await
        .map(|targets| {
            targets
                .into_iter()
                .filter(|t| {
                    t.is_main_frame
                        && t.valid()
                        && magicvault_effect::matches_target_filter(t, &args.filter())
                })
                .collect::<Vec<_>>()
        })
        .ok()
        .filter(|targets| {
            targets.len() == 1
                && targets[0].document == target.document
                && targets[0].top_origin == target.top_origin
        });
    if still_there.is_none() {
        material_ctx.release(&mut referenced, "the page changed before the fill");
        return status("target_unavailable");
    }
    // The fill starts: codes are spent now, never released after this point,
    // and the delivered values join the run's scrub set.
    if let Err(reason) = material_ctx.consume(&mut referenced, split) {
        return status_with_reason("material_unavailable", &reason);
    }
    // The shared adapter rechecks exact frame/document/origin and resolves all
    // selectors before mutation. No retry, target rediscovery, or auto-submit.
    let outcome = adapter.fill(target, material, cancel).await;
    let outcome = if outcome.valid(args.fields.len()) {
        outcome
    } else {
        magicvault_effect::Outcome::uncertain(args.fields.len())
    };
    json!({"status": if outcome.error.is_none() && outcome.fields.iter().all(|s| *s == FieldState::Filled) { "filled" } else { "not_completed" },
        "fields":outcome.fields,"error":outcome.error,"saved":false,"submitted":false})
}

#[cfg(test)]
#[path = "secure_prompt_fill_tests.rs"]
mod tests;
