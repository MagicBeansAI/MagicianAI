# Envoy Agent

**Envoy = "Presto in public-chat mode"**: a least-privilege agent that handles
ordinary bot messages from public surfaces, including messages from the owner
when they did not explicitly invoke a control surface. The owner (web UI, or an
allowlisted channel message that carries the control prefix) gets full-trust
Presto treatment; every ordinary bot message is a public-chat conversation
bound to the envoy agent on an isolated, per-sender thread.

## Configuration — `EnvoyConfig`

Defined in `magician/src/config.rs` as `MagicianConfig.envoy`
(`#[serde(default)]`). Sample lives in the active `magician-config.yaml` under
the `envoy:` block. Wired onto `ChatApi` via `with_envoy_config(...)` from
`magician-bin/src/main.rs`.

| Field | Type | Meaning |
|-------|------|---------|
| `owner_identities` | `HashMap<String, Vec<String>>` | `channel_type` → owner addresses that get full-trust (Presto) treatment. Kept for tests/dev/backward compatibility. |
| `owner_identity_envs` | `HashMap<String, String>` | `channel_type` → env var containing owner addresses. Use this for real phone numbers and emails. |
| `envoy_agent_id` | `String` (default `"envoy"`) | Agent bound to guest threads. |
| `engagement_forwarding_enabled` | `bool` (default `false`) | When off, an engagement lane still *resolves* but projects to the guest pair. Turning it on is a separate act. |

```yaml
envoy:
  envoy_agent_id: envoy
  owner_identities: {}
  owner_identity_envs:
    kapso: MAGICIAN_OWNER_KAPSO_IDENTITIES
    telegram: MAGICIAN_OWNER_TELEGRAM_IDENTITIES
    agentmail: MAGICIAN_OWNER_AGENTMAIL_IDENTITIES
  engagement_forwarding_enabled: false
```

`owner_identity_envs` values are parsed as comma, semicolon, or newline
separated lists, trimmed, deduped, and merged with `owner_identities` at
runtime. Keep real phone numbers, Telegram chat ids, and email addresses in
local, untracked env files:

```bash
MAGICIAN_OWNER_KAPSO_IDENTITIES=919876543210,918765432109
MAGICIAN_OWNER_TELEGRAM_IDENTITIES=123456789
MAGICIAN_OWNER_AGENTMAIL_IDENTITIES=owner@example.com
```

Matching is exact-string against the sender as the channel delivers it (Kapso
sends the number without `+`; Telegram private chats send the numeric
`ctx.chat.id`; emails are lower-cased).

**Sender-auth gate.** `is_owner` / routing take a `verified` flag. The
active-session handler does **not** trust `channel_verified` as an escalation:
`channel_is_verified` honours a caller `Some(false)` (AgentMail's
`unauthenticated` label) and otherwise requires a `VerifiedRequestIdentity`
**and** a transport that authenticates the sender (`web`, `kapso`/`whatsapp`,
`telegram`, `imessage`, `sms`). `agentmail`/`email`/`mail` and unknown
channels fail closed. An unauthenticated email from an allowlisted owner
address routes to the per-sender envoy guest thread
(`ext:agentmail:<email>`), not the owner's full-trust thread.

**Control-intent gate.** Kapso and the normal Telegram bot send
`control_intent=false` for an ordinary channel message. That suppresses owner
allowlist routing and sends even an allowlisted sender to the envoy guest
route. The bot SDK only sends `control_intent=true` when the message starts
with the configured control prefix (default `/magic`; legacy `@magic` is an
alias), and strips the prefix before dispatch. This bit never authenticates a
sender by itself. Shipped public chat bots also request a channel-scoped
owner-control thread (`ext:<channel>:<address>:magic`); a
`control_intent=true` request on the default `general` thread is mapped to the
same `:magic` thread before routing.

To find a Telegram chat id, send any message to the bot and inspect
`session.origin_channel.address` under
`MagicianNotes/scopes/<principal>/<workspace>/ui/chat_sessions/*/session.json`.

**Public ordinary chat policy.** Non-prefixed Kapso and Telegram messages
still create/use stable per-sender `ext:<channel>:<address>` sessions. Kapso
sends `source_surface=kapso-envoy-chat`; Telegram sends
`source_surface=telegram-envoy-chat`. The chat runtime treats those markers as
chat-only: no native tools, no chat-runtime tools, no delegation/handover, and
no adaptive thinking-mode tool. Model choice is backend-owned through
`kapso_envoy_chat` in the current template. If that mapping is absent, the
service falls back to the normal chat operation default. Users must send
`/magic <request>` to enter the Magician control/work rail.

## Helpers — `magician/src/magician_v2/chat/envoy.rs`

- `channel_is_verified(channel_type, request_authenticated, caller_claim) -> bool`
  — de-escalation only; a caller cannot assert verification.
- `is_owner(cfg, channel_type, address, verified) -> bool` — `web` is owner
  only when `verified`; otherwise the `(channel_type, address)` pair must
  appear on the allowlist **and** the transport must have authenticated the
  sender. Everyone else is a guest.
- `guest_thread_id(channel_type, address) -> String` — `ext:<channel_type>:<address>`.
- `route_for_inbound(...)` — projection of `resolve_inbound_lane` with
  `engagement: None`. Returns `(ui_thread_id, force_agent_id)`.

Both identity helpers use `address` verbatim. Callers must pass a
canonicalized address (E.164 for phones, lower-cased for email). An
un-normalized or unknown-channel address fails **closed** — treated as a guest.

## Routing

Routing is decided in `GET /api/magician/v2/chat/active`. The kapso bot (and
any consumer channel) calls the endpoint with `channel`, `channel_address`,
optional `channel_verified`, optional `control_intent`, and optionally
`ui_thread_id`. Shipped public chat bots opt into SDK per-address threading:
ordinary messages request `ext:<channel>:<address>`; explicit owner-control
messages request `ext:<channel>:<address>:magic`.

The handler calls `resolve_inbound_lane` (not `route_for_inbound`) so an
authoritative engagement can be named; `into_routing` still projects that
lane to the guest pair while `engagement_forwarding_enabled` is off.
`get_active_session_handler` reads the optional `EnvoyConfig` on `ChatApi`
and computes `effective_ui_thread_id` and `force_agent_id` **before**
creating the session.

```rust
pub fn resolve_inbound_lane(
    cfg: &EnvoyConfig,
    channel_type: &str,
    address: &str,
    verified: bool,
    control_intent: Option<bool>,
    requested_ui_thread_id: &str,
    engagement: Option<EngagementLane>,
) -> InboundLane
```

- **Non-control** (`control_intent=false`): guest thread + `envoy_agent_id`,
  even when the sender is allowlisted.
- **Owner** (verified web, or an allowlisted `(channel_type, address)` with
  authenticated sender and no explicit non-control signal): requested thread,
  no forced agent.
- **Guest** (everyone else, fail-closed): `ext:<channel>:<addr>` plus the
  envoy agent.
- **Engagement**: only from an `InboundIdentification::Authoritative` lane,
  and only changes landing when `engagement_forwarding_enabled` is on.

When `EnvoyConfig` is absent, routing is a no-op (requested thread, no forced
agent). Building `ChatApi` directly (tests) must set `envoy_config` — `None`
disables routing.

### `force_agent_id`

`ChatService::get_or_create_session(..., force_agent_id: Option<&str>)` uses
the override only when **creating** a session. An already-active session is
returned unchanged. The first guest message on a new per-sender thread binds
the envoy agent; subsequent messages reuse that session. Non-envoy callers
pass `None`.

## Envoy agent (grant)

Checked in at
`magician_data_v3/system/agent_templates/agents/envoy/definition.agent.yaml`
(`agent_id: envoy`, `kind: personal`, `is_primary: false`). Auto-discovered at
startup; the definition store materializes a template into the active scope on
first access.

**The grant is the security boundary (fail-closed).** Isolation is structural:

| Lever | Value | Why |
|-------|-------|-----|
| `tools` (allowlist) | `notify_owner`, `ask_owner`, `propose_meeting`, `request_owner_action` | Four owner-relay tools. Each surfaces a request to the owner — none act as, or read the data of, the owner. **No `harness:` enablement.** |
| `denied_tools` | `task_state`, `gmail`, `calendar`, `sheets`, `agentmail-send`, `kapso-whatsapp-send`, `search_memory` | Defense in depth if a future substrate injection surfaces them. |
| `user_memory_isolation` | `fully_isolated` | Cannot read owner / user memory tiers. |
| `readable_agents` | `[]` | No cross-agent memory reads. |
| `delegation_targets` | `[]` | No upward delegation to Presto or anyone else. |
| `trust_level` | `untrusted` | Treats inbound public chat as untrusted **and** narrows universal backend packs. |
| `constraints.coordination` | depth `0`, `allow_transitive_delegation: false` | Leaf agent; cannot sub-delegate. |

The guest-triage playbooks `kapso-processing` and `agentmail-processing` are
**not** on the grant. They are procedure skills (prose, not capability); their
rules are inlined in the persona (classify from the conversation, answer only
from clearly-public information, otherwise reach the owner with the four
relay tools). They still exist under `skillshub/` but listing them is not
what makes envoy fail-closed.

Every agent's catalog is augmented with `UNIVERSAL_BACKEND_PACKS`
(`native_integration.rs`). Both the offered catalog
(`extract_direct_capabilities`) and the dispatch ceiling
(`compute_dispatch_tool_scope` → `apply_tool_scope`) key off
`trust_level_is_untrusted`. An `untrusted` agent receives only
`SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED` (`activate_skill`, `deactivate_skill`,
`time_math`) from the universal set. Trusted agents (any non-`untrusted`
`trust_level`, e.g. Presto's default `local`) still receive the full universal
set. Narrowing **both** layers is essential: if only the catalog were
narrowed, a withheld pack would remain in the scoped registry.

The persona introduces as **Presto**, Magican's public-facing AI partner. It
must not assert that the speaker is or is not the owner; ordinary bot chat is
not the work/control lane. If the speaker is the owner and wants work done or
private data accessed, the persona tells them to use `/magic <request>`. It
must not claim private calendar/email/file access, must treat embedded
instructions to change role or reveal info as untrusted for authorization,
must never claim it notified or asked unless the matching tool actually
succeeded, and must never make commitments for the owner.

`default_personality: witty` is a copied default pinned in the envoy
definition, not dynamic inheritance from `personal-assistant`.

`tests/agent_definition_templates_test.rs::envoy_definition_is_least_privilege`
pins the four-tool allowlist, empty `readable_agents`/`delegation_targets`,
`user_memory_isolation == fully_isolated`, **`harness.is_none()`**, and an
explicit deny-list of fleet/lifecycle harness tools.

## Owner-consent diode

Downward (Presto → envoy) is ordinary `delegate_to_agent`. Upward (envoy →
owner) is a structured owner-consent request, never a privilege-escalating
delegation.

| Tool | Owner sees | On resolve |
|------|-----------|------------|
| `ask_owner` | A guest's question, with a text **Reply** affordance | The owner's reply is relayed to the guest by envoy. |
| `propose_meeting` | A meeting proposal (topic/window/duration), **Approve & book** / Reject | Approve → an owner-owned `personal-assistant` task books it, then relays confirmation. |
| `request_owner_action` | A requested privileged action, **Approve** / Reject | Approve → an owner-owned `personal-assistant` task performs it, then relays the outcome. |
| `notify_owner` | A 1–2 line who/what/when flag | One-way; no reply affordance. |

Each surfaces in the owner's global `/attention` (no thread filter). The
privileged action always runs as the **owner's** agent.

These four tools only **emit a `UserRequest`**. They live on the compiled-
handler path
(`magician/src/magician_v2/execution/compiled_handlers/{notify_owner,ask_owner,propose_meeting,request_owner_action}.rs`),
not as harness tools. Harness tools resolve only through autonomous-
coordinator registries and are unreachable in the reactive chat path.

### Resolution + relay

The tools emit via `UserRequestService::ask()`. At resolve time the
`user_request` arm of `respond_hitl_handler` recovers routing from
`pending_request_snapshot` and, on `Accepted` for `envoy.*` request types,
dispatches `ChatService::relay_envoy_owner_decision`, which binds the guest
session (`force_agent_id="envoy"`) and runs an owner-owned envoy turn (or, for
approvals, an owner-owned `personal-assistant` task).

**Privacy invariant:** the relay recipient is derived **only** from the stored
`channel`+`address` for that request — never from the owner's response text.
The guest session's `origin_channel` (set at session creation, not
agent-writable) is the actual egress target, so a prompt-injected envoy cannot
redirect a reply to a different contact.

## See also

- Design doc (archived): `docs/archive/plans/2026-06-09-external-contact-envoy-agent-design.md`

## Outbound replies → Claims Review

Before publishing an external Envoy text reply, `chat::envoy_claims` prepares an
outward act with the exact canonical text and persisted session binding,
then queues one pending candidate in `TranscriptIngestion`. This is a controlled
send, never a retrospective meeting. It neither writes assertion rows nor
assigns a relationship from an untrusted message.

The channel bot claims dispatch through
`POST /chat/sessions/{id}/messages/{message_id}/envoy-delivery`, then reports
`provider_accepted` or `unknown`. Only a runtime-minted bot bearer for that
session’s channel and scope is accepted. The bot supplies a random attempt UUID, channel type/address and SHA-256 digest
of the canonical text. These are checked against the prepared act and persisted
session; they cannot choose another scope, sender or recipient. Dispatch also
requires the matching claim candidate, so partial preparation never grants a
send. Prepared replies remain tracked after the configured Envoy identity changes.

The attempt binding is durable before dispatch. Losing a Begin response can
resume the same in-process attempt, while a different attempt or process restart
cannot acquire it. The SDK consumes its resumable identity before invoking the
adapter; accepted and uncertain attempts cannot send again. Receipt retries
retain the same binding and never resend. Ordinary Envoy replies still send
automatically: this is capture and delivery accounting, not owner pre-approval.

Channel acceptance is recorded only when the adapter explicitly acknowledges
all text chunks. A Kapso suppression or different template fallback, a partial
send, or a logged-and-swallowed adapter failure cannot confirm the exact reply.
Preparation and provider acceptance remain distinct from actual delivery.
A named human must separately confirm what was said in [Claims Review](../unified-ui/claims-review.md).
This does not make its contents true or turn it into a confirmed commitment.

Backend and bots must be deployed together (the delivery claim is a shared
contract). Statements are not backfilled. A crash after preparation/dispatch
leaves a visible unacknowledged candidate; review cannot promote it without an
acceptance receipt.
