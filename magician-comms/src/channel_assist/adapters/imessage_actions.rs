//! iMessage action adapter (Channel Assist Phase 2, iMessage Assist Task 4).
//!
//! Implements [`ChannelActionAdapter`] for iMessage. The only shipped action is
//! `reply`, which:
//! - `compose` — renders the managed `channel_reply_draft` prompts from the
//!   request draft-inputs and dispatches them through the LOCAL (ollama) op via
//!   [`RouterReplyDraftLlm`] (same fail-closed locality guard the drafter uses;
//!   unbound ⇒ the reply draft is unavailable and nothing is sent remotely), and
//! - `commit` — builds the FIXED, escaped AppleScript template (reusing the
//!   [`imessage_send`] escaping) and relays it through the Tauri host gateway
//!   ([`HostAutomationProvider`]) so a reply is sent from the owner's own
//!   Messages account, natively or from a container via one code path.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;

use magician::magician_v2::execution::compiled_handlers::imessage_send::applescript_escape;
use magician::magician_v2::media_seam::{
    host_gateway_url_from_env, AppleScriptRequest, HostAutomationProvider,
};
use magician::magician_v2::prompts::{
    names as prompt_names, rendered_prompt, versions as prompt_versions,
};

use super::super::adapter_registry::{
    ChannelActionAdapter, ChannelActionContext, ChannelActionDescriptor, ChannelActionDraft,
    ChannelActionRequest, ChannelActionResult,
};
use super::super::assist::reply_draft::{ReplyDraftLlm, RouterReplyDraftLlm};

/// The single shipped iMessage action id.
const REPLY_ACTION_ID: &str = "reply";

/// iMessage action adapter. Stateless — the scope + router come in via
/// [`ChannelActionContext`], so a single unit value is reused for every call.
pub struct ImessageActionAdapter;

#[async_trait]
impl ChannelActionAdapter for ImessageActionAdapter {
    fn available_actions(&self) -> Vec<ChannelActionDescriptor> {
        vec![ChannelActionDescriptor {
            id: REPLY_ACTION_ID.to_string(),
            label: "Reply".to_string(),
            needs_compose: true,
            confirm: true,
            icon: Some("reply".to_string()),
        }]
    }

    async fn compose(
        &self,
        action_id: &str,
        ctx: &ChannelActionContext,
        req: &ChannelActionRequest,
    ) -> Result<ChannelActionDraft> {
        if action_id != REPLY_ACTION_ID {
            bail!("iMessage action adapter: unknown action id '{action_id}' for compose");
        }

        // Local-pinned drafter, constructed exactly like the classify/distill
        // workers construct their router LLMs (router + broadcaster + scope).
        let llm = RouterReplyDraftLlm::new(
            ctx.router.clone(),
            ctx.broadcaster.clone(),
            ctx.principal.clone(),
            ctx.workspace.clone(),
        );
        if !llm.bound() {
            bail!("reply draft unavailable — no local model bound");
        }

        let (system, user) = render_reply_draft_prompts(req).await?;
        let text = llm.draft(&system, &user).await?;
        Ok(ChannelActionDraft { text })
    }

    async fn commit(
        &self,
        action_id: &str,
        _ctx: &ChannelActionContext,
        req: &ChannelActionRequest,
    ) -> Result<ChannelActionResult> {
        if action_id != REPLY_ACTION_ID {
            bail!("iMessage action adapter: unknown action id '{action_id}' for commit");
        }

        let recipient = reply_recipient_handle(req)?;
        let body = req.body.trim();
        if body.is_empty() {
            bail!("iMessage reply commit requires a non-empty body");
        }

        let script = build_imessage_send_script(&recipient, body);
        let provider = HostAutomationProvider::new(host_gateway_url_from_env());
        let result = provider
            .run_applescript(AppleScriptRequest {
                source: script,
                language: None,
                timeout_secs: Some(30),
            })
            .await
            .map_err(|error| anyhow!("iMessage send failed: host automation error: {error}"))?;

        if result.exit_code != 0 {
            bail!(
                "iMessage send failed (exit {}): {}",
                result.exit_code,
                result.stderr.trim()
            );
        }

        Ok(ChannelActionResult {
            provider: req.provider.clone(),
            account_alias: req.account_alias.clone(),
            action_id: REPLY_ACTION_ID.to_string(),
            provider_thread_id: Some(req.identity.external_conversation_id.clone()),
            provider_message_id: None,
            external_url: None,
            completed_at_ms: Some(now_millis()),
            provider_metadata: None,
        })
    }
}

/// Resolve the Messages recipient handle for a reply: the actor id when present
/// (a specific participant), else the conversation id.
fn reply_recipient_handle(req: &ChannelActionRequest) -> Result<String> {
    let handle = req
        .identity
        .actor_external_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| req.identity.external_conversation_id.trim());
    if handle.is_empty() {
        bail!("iMessage reply commit requires a recipient handle (actor or conversation id)");
    }
    Ok(handle.to_string())
}

/// Build the FIXED, escaped AppleScript send template (mirrors
/// `imessage_send::handle` for the iMessage service). `to` and `body` are
/// escaped so they cannot break out of the double-quoted literals.
fn build_imessage_send_script(to: &str, body: &str) -> String {
    format!(
        "tell application \"Messages\"\n\
         \tset targetService to 1st account whose service type = iMessage\n\
         \tset targetParticipant to participant \"{to}\" of targetService\n\
         \tsend \"{body}\" to targetParticipant\n\
         end tell",
        to = applescript_escape(to),
        body = applescript_escape(body),
    )
}

/// Render the managed `channel_reply_draft` system + user prompts from the
/// request draft-inputs, via the same managed-prompt path the classify/distill
/// workers use. Missing optional inputs fall back to the prompt template's own
/// declared defaults.
async fn render_reply_draft_prompts(req: &ChannelActionRequest) -> Result<(String, String)> {
    let system = rendered_prompt(
        prompt_names::CHANNEL_REPLY_DRAFT_SYSTEM,
        prompt_versions::CHANNEL_REPLY_DRAFT,
        HashMap::new(),
    )
    .await
    .map_err(|error| anyhow!("rendering channel reply-draft system prompt: {error}"))?;

    let latest_message = req
        .latest_message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("reply draft compose requires the latest_message to reply to"))?;

    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("latest_message".to_string(), latest_message.to_string());
    if let Some(subject) = non_empty(req.subject.as_deref()) {
        vars.insert("subject".to_string(), subject);
    }
    if let Some(sender) = non_empty(req.sender.as_deref()) {
        vars.insert("sender".to_string(), sender);
    }
    if let Some(thread_summary) = non_empty(req.thread_summary.as_deref()) {
        vars.insert("thread_summary".to_string(), thread_summary);
    }
    if let Some(hint) = non_empty(req.hint.as_deref()) {
        vars.insert("hint".to_string(), hint);
    }

    let user = rendered_prompt(
        prompt_names::CHANNEL_REPLY_DRAFT_USER,
        prompt_versions::CHANNEL_REPLY_DRAFT,
        vars,
    )
    .await
    .map_err(|error| anyhow!("rendering channel reply-draft user prompt: {error}"))?;

    Ok((system, user))
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A single reusable action-adapter value (stateless).
pub static IMESSAGE_ACTION_ADAPTER: ImessageActionAdapter = ImessageActionAdapter;

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::super::adapter_registry::ChannelIdentity;
    use super::super::super::ingest_imessage::IMESSAGE_PROVIDER;
    use super::super::super::types::ChannelLane;
    use super::*;

    fn request(body: &str, actor: Option<&str>, conversation: &str) -> ChannelActionRequest {
        ChannelActionRequest {
            provider: IMESSAGE_PROVIDER.to_string(),
            account_alias: "default".to_string(),
            lane: ChannelLane::default(),
            identity: ChannelIdentity {
                external_conversation_id: conversation.to_string(),
                external_message_id: None,
                actor_external_id: actor.map(str::to_string),
                actor_display_name: None,
                direction: None,
            },
            reply_to_message_id: None,
            body: body.to_string(),
            subject: None,
            sender: None,
            latest_message: None,
            thread_summary: None,
            hint: None,
            provider_metadata: None,
        }
    }

    #[test]
    fn available_actions_returns_the_reply_descriptor() {
        let actions = ImessageActionAdapter.available_actions();
        assert_eq!(actions.len(), 1);
        let reply = &actions[0];
        assert_eq!(reply.id, "reply");
        assert_eq!(reply.label, "Reply");
        assert!(reply.needs_compose);
        assert!(reply.confirm);
        assert_eq!(reply.icon.as_deref(), Some("reply"));
    }

    #[test]
    fn commit_escapes_quotes_and_backslashes_in_the_body() {
        let script = build_imessage_send_script("+14155551234", r#"say "hi" \ bye"#);
        // The recipient sits unescaped-safe inside the quoted literal.
        assert!(script.contains(r#"participant "+14155551234""#));
        // The body's " and \ are both escaped so they cannot break out.
        assert!(script.contains(r#"send "say \"hi\" \\ bye""#));
        // Fixed template invariants.
        assert!(script.contains("service type = iMessage"));
        assert!(script.contains("tell application \"Messages\""));
    }

    #[test]
    fn commit_prefers_actor_handle_then_conversation() {
        let with_actor = request("hi", Some("+14155551234"), "chat-1");
        assert_eq!(reply_recipient_handle(&with_actor).unwrap(), "+14155551234");
        let no_actor = request("hi", None, "buddy@example.com");
        assert_eq!(
            reply_recipient_handle(&no_actor).unwrap(),
            "buddy@example.com"
        );
    }

    #[tokio::test]
    async fn compose_rejects_unknown_action_id() {
        let ctx = ChannelActionContext {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            router: std::sync::Arc::new(
                magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter::new(
                    None,
                ),
            ),
            broadcaster: None,
        };
        let req = request("hi", Some("+1"), "chat-1");
        let err = ImessageActionAdapter
            .compose("react", &ctx, &req)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown action id"),
            "unexpected: {err}"
        );
    }

    #[tokio::test]
    async fn commit_rejects_unknown_action_id() {
        let ctx = ChannelActionContext {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            router: std::sync::Arc::new(
                magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter::new(
                    None,
                ),
            ),
            broadcaster: None,
        };
        let req = request("hi", Some("+1"), "chat-1");
        let err = ImessageActionAdapter
            .commit("react", &ctx, &req)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown action id"),
            "unexpected: {err}"
        );
    }
}
