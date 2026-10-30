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

Matching is `config::owner_identity_matches`: on the phone channels
(`kapso`, `whatsapp`, `sms`, `imessage`) a phone-like identity and sender
compare **digits only**, so `+91 98765 43210` in config matches the
`919876543210` Kapso sends (and a `…@s.whatsapp.net` JID); an email compares
case-insensitively; everything else — a Telegram chat id — is exact.

### Adding an owner identity

Two equal ways; both keep working side by side.

1. **Claim it from the chat (easiest).** Settings → Owner identities →
   *Claim* (or `POST /api/magician/v2/owner-identities/claim-code`, owner
   session or paired owner phone) mints a one-time code: 8 characters from an
   unambiguous alphabet, 10 minutes, single use. Send `/claim <code>` to the
   Magician bot from the WhatsApp number or Telegram chat to record
   (`/claim@Bot`, `/magic /claim` work too). The chat API answers it before
   any agent turn — from a guest session as well — and appends the sender
   address the transport verified to the channel's `owner_identity_envs`
   variable in both live env files and the process env, so the very next
   message routes as the owner with no restart
   (`magician/src/magician_v2/owner_claim.rs`). A wrong or expired code gets
   one neutral reply; five wrong codes per sender per 10 minutes, then every
   try is refused, and 50 failures across senders burn all live codes. The
   code is never echoed and the transcript stores `/claim ••••••••`. Mail
   channels are refused (a `From:` is not proof), and so is a channel with no
   `owner_identity_envs` variable (the routing config is a startup snapshot).
2. **Set it by hand.** Put the address in the channel's
   `MAGICIAN_OWNER_*_IDENTITIES` variable or in `envoy.owner_identities`
   (restart for the config-file form). A claim only appends: it never
   reorders, rewrites or removes hand-set entries or touches other env keys.

`GET /api/magician/v2/owner-identities` lists both sources per channel,
masked to the last two characters; `DELETE …/owner-identities/{channel}/{index}`
removes an env-held one (config-file entries are edited in the file).

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
`control_intent=false` for an ordinary channel message, which routes a
non-owner sender to the envoy guest route. A verified owner is the exception
(`envoy::effective_control_intent`): every channel bot forwards only direct
chats, so the owner's own number or chat id is in control with or without the
prefix, and the message lands on the owner's `ext:<channel>:<address>:magic`
thread rather than the envoy-bound `ext:<channel>:<address>` one. The bot SDK only sends `control_intent=true` when the message starts
with the configured control prefix (Kapso and Telegram bots: `MAGICIAN_CONTROL_PREFIX`,
default `/magic`, legacy alias `@magic`), and strips the prefix before dispatch. This bit never authenticates a
sender by itself. Shipped public chat bots also request a channel-scoped
owner-control thread (`ext:<channel>:<address>:magic`); a
`control_intent=true` request on the default `general` thread is mapped to the
same `:magic` thread before routing.

**Owner chat commands.** `/god` and `/godoff` sent by the verified owner turn
god mode on or off for that chat (`docs/components/magician/god-mode.md`).
The send endpoint re-checks `envoy::is_owner` against the stored session's
origin address; a guest's `/god` reaches the envoy as plain text and changes
nothing. `/claim <code>` is the one command a guest may send: see *Adding an
owner identity* above.

To set a Telegram chat id by hand instead of claiming it, send any message to
the bot and inspect `session.origin_channel.address` under
`MagicianNotes/scopes/<principal>/<workspace>/ui/chat_sessions/*/session.json`.

**Public ordinary chat policy.** Non-prefixed Kapso and Telegram messages
still create/use stable per-sender `ext:<channel>:<address>` sessions. Kapso
sends `source_surface=kapso-envoy-chat`; Telegram sends
`source_surface=telegram-envoy-chat`. The chat runtime treats those markers as
chat-only: no native tools, no chat-runtime tools, no delegation/handover, and
no adaptive thinking-mode tool. Model choice is backend-owned: `public_chat.kapso_envoy_chat` in
`magician-config.yaml` names the `kapso_envoy_chat` operation (paid) and
`kapso_envoy_chat_fallback` (taken when paid admission declines a turn), both
mapped in `llm-router.yaml`; with no `kapso_envoy_chat` profile the turn uses
the `chat_completion` operation default. Users must send
`/magic <request>` to enter the Magician control/work rail.

A session bound to envoy always runs on the `public_envoy` surface, whether or
not its bot sends a public-chat source surface (the AgentMail, Gmail and
WhatsApp bots send none). The server derives this from the binding, as it does
when the session is created; the source surface only adds the chat-only policy
and paid admission above. A shared meeting room bound to envoy keeps the
`meeting` surface.

A shared call binds this same agent. `delegate_to_chat` on that call runs as
that agent and does not become the personal assistant. Search and the browser
skill cannot use a CDP connection or the owner's signed-in Chrome. An
untrusted agent's empty browser-transport list is headless and headed, not
every transport.

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

`is_owner` matches through `owner_identity_matches` (digits-only phones on
phone channels, case-insensitive email, exact otherwise). An address that
still misses, or an unknown channel, fails **closed** — treated as a guest.

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

- **Non-control** (`control_intent=false`): guest thread + `envoy_agent_id`.
  The API seam promotes a verified owner's non-control message to
  `control_intent=true` first, so only non-owners reach this branch live.
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
| `tools` (allowlist) | owner relays, public lookup, source search, media, still images and GIFs, this agent's own memory | Relays: `notify_owner`, `ask_owner`, `propose_meeting`, `request_owner_action`. Lookup: `content_search`, `content_read`, `web_search`, `time_math`. Sources: `reddit-search`, `hackernews-search`, `github-search`, `producthunt-search`, `arxiv-search`, `polymarket-search`, `youtube-search`, `catchup_merge`. Long report: `deep-research-with-openai`. Media: `media-fetch`, `media_edit`, `media_edit_status`. Creative: `image-generation`, `image-generation-via-minimax`, `meme-generation-via-imgflip`, `gif-search-via-klipy`. Memory: `search_memory`, `save_preference`. `activate_skill` and `deactivate_skill` turn on only a procedure also named here; none are. None read the owner's shared memory or hand work to another agent. **No `harness:` enablement.** |
| `denied_tools` | `task_state`, `gmail`, `calendar`, `sheets`, `agentmail-send`, `kapso-whatsapp-send`, `forget_memory` | Defense in depth if a future substrate injection surfaces them. Deleting memory stays off. |
| `user_memory_isolation` | `fully_isolated` | Reads and writes `agents/{id}/memory/user_memory/`, not the owner's shared user root. |
| `readable_agents` | `[]` | No cross-agent memory reads. |
| `delegation_targets` | `[]` | No upward delegation to Presto or anyone else. |
| `trust_level` | `untrusted` | Treats inbound public chat as untrusted **and** narrows universal backend packs. |
| `constraints.coordination` | depth `0`, `allow_transitive_delegation: false` | Leaf agent; cannot sub-delegate. |

The guest-triage playbooks `kapso-processing` and `agentmail-processing` are
**not** on the grant. They are procedure skills (prose, not capability); their
rules are inlined in the persona (classify from the conversation, answer from
public lookup, otherwise reach the owner with the four relay tools). They
still exist under `skillshub/` but listing them is not what makes envoy
fail-closed.

Envoy is `surface_only`, so it does not receive `SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED`;
`time_math`, `activate_skill` and `deactivate_skill` are named on the
allowlist instead. A public call does not inject a playbook body.
`deep-research-with-openai` writes a long cited report on request using the
operator OpenAI key. `media-fetch` downloads one public media URL; `media_edit`
edits a file inside the caller workspace. Generated video, music, the browser
skill, owner accounts and `delegate_to_agent` stay off. `delegate_to_chat` on a
call is added by the voice catalog and keeps this grant. Guest text chat
(`kapso-envoy-chat`, `telegram-envoy-chat`) still gets no tools.

**Memory.** A finished turn still writes an episode. `save_preference` stores a
non-personal note in this agent's own user-memory file stamped `public`; an
untrusted agent can save only that kind of note, and naming an agent tier such
as insights is refused. `search_memory` and the prompt read public-labelled
notes and the meeting in progress, never unlabelled insights, archive, task
notes or the owner's shared user root. A shared call filters search even when
the seated agent is the trusted personal assistant; the owner Memory page stays
unfiltered. People, contacts, places, procedures and other meetings stay out.
Identifier-shaped secrets (email, phone number, token) are stripped on the way
out and refused on the way in; a regex does not catch a name beside a school, so
the persona tells the agent not to save that kind of fact.

Consolidation does not copy a fully isolated or untrusted agent's notes into the
owner's shared preferences. Teardown of a call seated as that agent does not
append the meeting summary to the owner's shared research findings or learning
funnel; an owner Listen, with no seated agent, still does. The live
`delegate_to_chat` path keeps a room on the meeting surface.

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
pins that allowlist, empty `readable_agents`/`delegation_targets`,
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
outward act with the exact canonical text and persisted session binding, then
queues one pending candidate in `TranscriptIngestion`. This is a controlled send,
not a retrospective meeting; it writes no assertion rows and assigns no
relationship from an untrusted message.

The channel bot claims dispatch through
`POST /chat/sessions/{id}/messages/{message_id}/envoy-delivery`, then reports
`provider_accepted` or `unknown`. Only a runtime-minted bot bearer for that
session's channel and scope is accepted. The bot sends a random attempt UUID,
channel type/address and the SHA-256 digest of the canonical text; these are
checked against the prepared act and session and cannot choose another scope,
sender or recipient. Dispatch also needs the matching claim candidate, so
partial preparation never grants a send. Prepared replies stay tracked after the
configured Envoy identity changes.

The attempt binding is durable before dispatch. A lost Begin response can resume
the same in-process attempt; a different attempt or a restart cannot acquire it.
The SDK consumes its resumable identity before invoking the adapter, so accepted
and uncertain attempts never send again, and receipt retries never resend.
Ordinary Envoy replies still send automatically: this is capture and delivery
accounting, not owner pre-approval.

Channel acceptance is recorded only when the adapter acknowledges every text
chunk; a Kapso suppression or template fallback, a partial send, or a swallowed
adapter failure cannot confirm the reply. Preparation and provider acceptance
are distinct from delivery. A named human separately confirms what was said in
[Claims Review](../unified-ui/claims-review.md), which neither makes it true nor
turns it into a confirmed commitment.

Backend and bots must be deployed together (the delivery claim is a shared
contract). Statements are not backfilled. A crash after preparation/dispatch
leaves a visible unacknowledged candidate; review cannot promote it without an
acceptance receipt.
