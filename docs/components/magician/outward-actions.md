# Outward actions — capture by default

An **outward action** is a dispatch that sends something out of the owner's
control: an email, a chat message, a calendar invitation. These are the gate
before anything autonomous may run.

## Classification is argument-aware, not token-only

A capability is outward when it can transmit; the action token alone is not
enough. `gmail`/`presto-gmail` expose a `raw` action (argv passthrough), so

```text
gmail  action=raw  args=["+send", "--to", "someone@example.com", …]
```

sends mail under a non-sending token. `outward_dispatch_class(capability, action,
params)` treats any dispatch of a world-reaching capability with an unbounded
argument passthrough as outward. The passthrough test is
`effective_action::is_passthrough_parameter`, shared with restriction and
effective-action, and a test asserts every known spelling trips it. This
over-gates on purpose (a `triage` with `extra_args` is captured): a capture
notice on a read is cheaper than an unrecorded send.

## Classified centrally, not per agent

`agents::outward_actions::outward_action_class(capability, action)` is one
central table, not a `requires_approval` entry per definition, so "an
autonomous agent cannot send" holds for every agent, including future ones.

- **Reading is not outward.** Inbox listing, `agentmail-read` and
  `kapso-whatsapp-read` pass untouched.
- **Send-only capabilities are outward whatever action they name**
  (`imessage_send`, `agentmail-send`, `kapso-whatsapp-send`), pinned by a test.
- Compiled pack tools arrive as `pack__action`; the capability is resolved
  through `approval::pack_tool_for_approval`, as the approval gate does, so both
  gates protect the same set.
- Name-only predicates, both via `outward_action_class`:
  `capability_can_reach_the_world(capability)` and
  `capability_only_sends(capability)` (separates `agentmail-send`, where the bare
  call is the send, from `gmail`, which also reads).

## Optional context controls

The legacy `capture_only` baseline is unchanged. Each context may explicitly
choose `inherit` (the baseline), `capture`, or `live`; omitted contexts inherit.
`force_capture_only: true` captures every context, including live overrides.
Edit these process-wide controls in **Settings → Agents & safety → Outward
actions** (`/settings#outward-actions`). Save and apply persists only this
section and updates the running policy immediately; accepted config reloads
also apply it. Defaults remain unchanged in seed and live config.

```yaml
outward_actions:
  capture_only: true
  force_capture_only: false
  contexts:
    owner_chat: inherit
    owner_tasks: inherit
    opc: inherit
    public_agents: inherit
```

- `owner_chat`: authenticated private chat/voice tool calls and the work they
  launch, including delegated specialists, handovers and further task creation.
- `owner_tasks`: tasks created through Work or VibeDev by an authenticated owner
  session, owner API token or paired owner phone, including descendants and scheduled runs.
- `opc`: autonomous goal cycles and programme work. Starting a programme cycle
  manually does not convert that programme into personal work.
- `public_agents`: public-facing agents — work that began from a public
  invocation (Envoy guest relays, meeting rooms, untrusted voice surfaces) and
  the cycles of agents whose definition declares
  `autonomous_config.outward_context: public_agent` (the ambassador). Split
  from `opc` so autonomous work can send while public agents stay captured.
  An agent may only declare itself public, never owner work; an owner chat
  delegating to such an agent stays `owner_chat`. Origins recorded before the
  split stay `opc` (origins are never re-derived). Owner saves must name it
  like the other contexts; the web panel shows it only when the server does.

The origin is server-issued, immutable task metadata in
`scopes/<p>/<w>/restricted/outward_origins/<task-hash>.json`, authenticated with
an HMAC bound to principal, workspace and task id. It survives execution resume
and restart. Child executions on the same task reuse it; newly created tasks
inherit it. Goals, tags, `created_by`, chat links and tool arguments cannot
promote an origin. Task creation with bot/terminal credentials alone does not
establish an owner-task origin.
Legacy/unclassified tasks capture when any context override is in use; with
all contexts inheriting they retain the legacy baseline. Corrupt or transplanted
metadata refuses dispatch. No automatic origin inference from old task notes.

For a classified origin, with force-capture off:

| Context choice | Baseline capture | Baseline live |
|---|---|---|
| `inherit` | Capture | Live |
| `capture` | Capture | Capture |
| `live` | Live | Live |

With `capture_only: true` and `force_capture_only: false`, all combinations of
explicit context choices reduce to this table (`inherit` also means Capture):

| Owner chat | Owner tasks | OPC | What may dispatch live |
|---|---|---|---|
| Capture | Capture | Capture | Nothing |
| Live | Capture | Capture | Chat-origin work |
| Capture | Live | Capture | Work-created owner tasks |
| Capture | Capture | Live | OPC work |
| Live | Live | Capture | Chat-origin work and owner tasks |
| Live | Capture | Live | Chat-origin work and OPC work |
| Capture | Live | Live | Owner tasks and OPC work |
| Live | Live | Live | All three classified contexts |

This table controls posture, not permission. Apply the following independent
conditions as well; a Live result means only that the capture gate permits
provider dispatch.

| Condition | Result |
|---|---|
| Force capture enabled | Capture, regardless of context or God mode |
| Context is `capture` | Capture, even if the baseline is live |
| Classified context is `inherit` | Use `capture_only` |
| Origin is unclassified/legacy and any context override is used | Capture |
| Origin is unclassified/legacy and every context inherits | Use `capture_only` |
| Config/posture was never installed | Capture |
| Invalid origin proof or provenance store unreadable | Refuse; no live fallback |
| Tool/delegation denied, work ceiling invalid, unbindable send or suppressed recipient | Existing refusal remains |
| Approval required and unanswered | Wait for approval |
| Approval denied or expired | Do not send |
| Preference check conflicts or is unavailable | Existing owner review/recovery rules remain |
| God/Super God mode | Existing approval rules apply; capture policy still applies |
| Browser form clicks | Separate submission-review switch and site approvals; capture does not block arbitrary clicks |
| Task paused, cancelled or stopped | Lifecycle rules still prevent execution |
| Live posture and all other gates satisfied | Dispatch and record the result |
| Transport fails after dispatch may have occurred | Unknown outcome; reconcile before retrying |
| Provider acknowledges the send | Report accepted; delivery requires its own receipt |
| Settings save is stale or cannot be persisted | Reject; keep the running policy unchanged |
| An older config reload finishes after a Settings save | Keep the newer Settings policy |
| Captured and review before sending is on (default) | Pause the run and ask the owner: Send, Discard, or always allow this agent → recipient |
| Owner answers Send / Always allow | Replay exactly the held call once; only capture is lifted, every other gate runs again |
| Owner answers Discard, or 7 days pass unanswered | Nothing is sent; the run continues told so |
| An always-allow grant covers the agent and every recipient | Lift capture only; suppression, compliance and preference checks remain |
| Capture changes to Live | Permit future decisions; never replay captured calls |
| Policy changes after a send was admitted | Already admitted sends may finish |

Native tools and governed stdio MCP use the same posture resolver. Overrides
also cover other classified outward acts (calendar writes and form submissions),
not only messaging. They grant no tools or access to additional private data,
waive no preference check, and do not change which payload an approval binds.

## Review before sending

`outward_actions.review_captured_sends` (default `true`; Settings → Outward
actions, "When a send is captured") turns a capture into a question. The run
loop's apply step (`agents::outward_review`, `outward_review_for`) pauses a
classified send its context would capture as a confirmation whose HITL
request carries `input_schema.escalation_trigger: "outward_review"`, the
options `send` / `discard` / `always_allow_recipient`, an `approval_preview`
naming agent, recipients, subject and excerpt, and an `outward_review` record
(capability, action, class, recipients, agent, `deadline_at_ms`) bound into
the pause's authorization hash. Answers are `{type:"choice", selected_id}`;
`{type:"confirmation", confirmed}` maps true to send and false to discard.

- **Send** registers a one-time pass for this execution and the call's
  canonical JSON. When the replay passes its gate, the direct path binds the
  pass to that dispatch attempt (`effect_id`) before secrets and credentials
  are filled in; the capture check takes it and dispatches. Every other gate
  runs again, including the preference check (after the review, not folded
  into it). Resolved `$file` / `$ref` values are bound by a scoped fingerprint
  rechecked at dispatch, so changed values cannot reuse Send. The pass lasts
  as long as the review.
- **Always allow** also records a grant (agent, capability, recipient) at
  `<runtime root>/outward/recipient_grants.json`, listed by
  `GET /api/magician/v2/settings/outward-actions/grants` and revoked by
  `DELETE …/grants/{id}`. A grant lifts capture only while review is on,
  when the act is bindable and every recipient is granted; force capture is
  never lifted. Grants are per agent, not per context.
- **Discard**, an unknown choice, or an answer after the deadline continues
  the run with a "NOT SENT" note. A sweep answers reviews still open after
  7 days as discarded.

The card fills in data passed by reference, or says when it cannot. A send
batched behind another act waits for its own turn so it is reviewed.
Governed stdio MCP sends cannot pause and keep being captured. With the
setting off, every capture is recorded without asking.

## Chat channels: typed sends and native CLI access

`whatsapp`, `telegram` and `telegram-self` each expose an explicit `send` action
(in `SENDING_ACTIONS`) with a typed recipient, which the suppression screen,
disclosure record and restricted form read:

| Capability | Send action | Recipient parameter | Body |
|---|---|---|---|
| `whatsapp` | native `send` / `poll` | `jid` | text/reply, file/caption, or poll question/options |
| `telegram` | `send` → the adapter's own route, method fixed to `sendMessage` | `chat_id` | `text` |
| `telegram-self` | `send` → `tgcli send text --to … --message … --json` | `to` | `message` (+ optional `reply_to`) |

Classifying every `run` would be wrong: a classified act meets the bindability
refusal, and `run`'s single free-text parameter can never bind, so `chats list`,
`contacts search`, `channels`, `getMe` and `getChat` would be refused too.

**Structural deny rule.** `outward_dispatch_class` inspects only the command
head each adapter understands: WhatsApp `messages send` / `media send`,
Telegram Bot API methods starting `send`, `forward` or `copy`, Telegram-self
`send text` / `send photo` / `send file`. These are outward, enter the
disclosure path and fail closed at the bindability refusal. Reads (`chats list`,
`contacts search`, `channels`, `getMe`, `getUpdates`) stay unclassified. The
send paths are typed `send`, including WhatsApp attachments. Attachment paths use
`workspace_path: read_file`: stage authorised sources in `workdirs`; absolute
paths, traversal and symlinks are refused. `whatsapp react` is unclassified:
it uses its existing reaction-key adapter and wu send constraints.

Tests: `a_chat_channel_is_outward_through_its_explicit_send_action`,
`a_send_composed_inside_run_is_classified_without_classifying_reads`,
`reading_through_a_chat_channel_stays_ungated` (`agents::outward_actions`).

WhatsApp `run` retains the native command surface, including writes and
administration. Known reads remain ordinary; write/admin/unknown commands use
the shared owner-approval gate. Existing raw-send bindability rules still apply.
`whatsapp-mcp` discovers upstream wu schemas directly; reviewed JSON-pointer
bindings govern file paths, recipients, capture and durable disclosure/receipts.
Group mutations record operations without inventing message-delivery receipts.

Typed sends carry:

- **Consequence class** `bounded_communication`, asserted equal to
  `kapso-whatsapp-send`. It is the one class a standing envelope may cover.
- **Restricted form** for all three; see `restricted-actions.md`.
- **Receipt**: `RECEIPT_SOURCES` keyed on capability **and send action**, so a `run`
  result is never read as a message id (`outward-assertions.md`).
- **`jid` as a recipient parameter** in `effective_action`. Without it a
  WhatsApp send has no recipients and `Addressing::for_class(Message)`
  (`MustNameRecipients`) refuses.

## Form submission: a second table

The sending table's fallbacks (`execute` for a dispatch with no action, argv
passthrough) suit a transmitter but not a browser, where `args` spells every
subcommand. `SUBMITTING_CAPABILITIES` is a separate table with a
**per-capability** action list.

**Classified: `submit` and `submit_form`, nothing else.** Any interaction verb
(`click`, `press`/`key`, `eval`, `find`/`batch`, …) can submit a form, but
classifying them would refuse every autonomous click, and `browser` has no
restricted form to bind. Reading, navigating, staging input (`fill`, `type`,
`select`) and clicking stay unclassified.

- **There is no `submit` subcommand**, so this table sees no browser submission
  today; the tokens are listed so one cannot arrive ungated.
  `no_action_the_browser_skill_declares_is_seen_by_the_outward_gate`
  (`magician/tests/agent_definition_templates_test.rs`) fails the day one does.
- **The token cannot tell a link from a submit control.** Classifying the
  click refuses navigation; leaving it lets an application be submitted with
  no record.
- The class is `form_submission` → `submission_or_publication`, **not** bounded
  communication, so an engagement envelope can never authorize pressing Submit.

**Capture does not stand in front of an unclassified submit.** The outward block
in `execute_action_inner` (disclosure, work-context ceiling, bindability
refusal, suppression, envelope shadow, capture) sits behind
`if let Some((capability, action_token, class)) = classify_outward_dispatch(action)`,
which is `None` for `browser`/`click`. The **grant** limits the act: the shipped
`ambassador` (which speaks in the company's name) has `browser` in
`denied_tools`. Other shipped holders (`web-researcher`,
`web-researcher-opc`, `cpo`, `cro`, `presentation-maker`, `personal-assistant`,
`executive-assistant`) remain exposed. A full fix needs an
explicit browser submit action or a restricted browser form that binds the
target.

**The opt-in submit guard stands in front of detected submissions.**
`outward_actions.browser_submission_guard` defaults to `false`. When enabled,
before any browser interaction,
`primitive_dispatch::browser::submit_guard` inspects the element it would hit
(`get box` → `document.elementFromPoint`) and holds the act when it would submit: a
submit control of a non-GET form, a final-action label ("Submit", "Send
application", "Place order", "Pay", "Publish", "Sign up", …), Enter in a text
field of a non-GET form, or a script calling `form.submit()` /
`requestSubmit()`. GET forms (search, filters) and wizard words ("Next",
"Continue", "Apply") stay free. A held submission asks the owner through a
`browser_submission_approval` decision card (the page, the control, and the
draft in the focused or first editable field) and waits, up to 30 minutes.
**Approve Once** binds the observed URL, draft and available form-state digest;
changes while approval is pending require review again. **Approve Always**
grants the current site until revoked; navigation/switching batches offer only
once. **Deny**, or no answer, stops the act
(`[OUTWARD-DECLINED]`) and tells the agent not to retry or find another route.
One-time approval never carries over to a later submission. With no approval channel in
the run the guard still refuses outright (`[OUTWARD-REFUSED]`). Limits: an element the probe
cannot read (iframe, DOM-only engine, probe error) is allowed and logged, and a
script-driven form whose final control uses an unknown word is not caught.

## Whose identity a browser visit carries

`ConnectionMode::from_runtime_selection` resolves transport as
`MAGICIAN_AGENT_BROWSER_MODE` → the call's `connection_mode` → `"cdp"`. `Cdp`
attaches to the owner's real Chrome through the magicutor proxy
(`DEFAULT_MAGICUTOR_PROXY_URL`), with the owner's cookies and sessions.

Trusted agents may use owner CDP without God mode. Program/engagement-confined
work needs an explicit `cdp` entry in the agent's `browser_transports`; an
unreadable engagement carrier fails closed. Delegation retains the work carrier,
and cached sessions are checked against the acting agent's current ceiling.
Session IDs partition automation tabs, not the owner's underlying identity.

### Transport ceiling

`browser_transports` is a per-agent ceiling over `cdp` / `headed` / `headless`;
**empty means all three for ordinary trusted runs**, with the explicit-CDP
requirement above for confined work. `dispatch_browser_primitive` applies it after
`ConnectionMode::from_call_arguments` folds call, operator override and
default, and it outranks all three. A transport outside the ceiling, or an
unknown name, **refuses**
(`NOT BROWSED — this agent may only use the browser transports […]`) rather
than substituting: a `[cdp]`-only ceiling asked for `headless` could only
substitute by promoting to owner identity. The ceiling is a fact about the
agent, so a delegate carrying its parent's authority cannot exceed its own
declaration; it is replaced, never merged, at owner transitions.

Shipped: `web-researcher`, `web-researcher-opc`, `cpo` and `envoy` declare `[headless, headed]`
(`web-researcher` closes the ambassador's delegated-research path).
`cro` and `presentation-maker` explicitly include CDP; `personal-assistant` and
`executive-assistant` use the ordinary trusted-run default. The work carrier and
agent ceiling both apply. Test:
`an_execution_with_no_engagement_still_browses_as_the_owner`
(`magician/tests/engagement_scoped_retrieval.rs`).

A ceiling withholds the owner's identity; it does not grant a company one. An
agent under `[headless, headed]` browses as nobody. Where a company identity is
needed, **do not grant `browser` to an agent whose acts speak for somebody**.

## Capture is the posture unless sending was turned on

`outward_actions.capture_only` defaults to **true**. Under capture an outward
dispatch is logged and **not performed**, and the model gets a result starting
`NOT SENT — captured` that tells it not to claim a send.

`outward_capture_only()` returns `true` when no posture was installed, so a
test, a partly initialized binary or an unwired entry point cannot send.

## Where it is enforced

At the top of `execute_action_inner`, which every action passes through, before
any side effect. `agents::outward_policy` publishes accepted config and owner
Settings updates; native and MCP dispatch read it after provenance lookup.
Boot logs the baseline and overrides; each live dispatch logs `[OUTWARD-LIVE]` with
capability, action, class, session and agent.

## What this does NOT cover

- **Browser form submission**; see *Form submission* above.
- **Whose identity a browser act carries.** This module classifies *what* an
  act is; identity is limited by the engagement guard and `browser_transports`.
- **Rate and volume caps** live in `resource_authority.budgets`: EMAIL_SENDS and
  WHATSAPP_SENDS are 50/day, matching the skills' `SKILL.md`, which the agent
  plans against. Change `magician-config.yaml` (seed and live) and `SKILL.md`
  together.
- **Calendar writes are seen only by argument shape.** `SENDING_ACTIONS` lists
  `create_event` and `invite`, but `calendar`/`presto-calendar` use
  `events_insert`/`events_patch`/`events_delete`. The static classifier fails
  these closed to `commitment_or_transaction`; capture catches them because
  `events_insert` needs an `args` passthrough, so `outward_dispatch_class`
  returns `CalendarInvite`, but only while the capability is in
  `OUTWARD_CAPABILITIES`. If a skill types `events_insert`'s parameters,
  `SENDING_ACTIONS` must gain the real tokens first.
- **`run` commands outside the recognized send heads** are invisible to this
  gate.
- **Approval is independent.** Capture is an operator-wide posture;
  `requires_approval` is per act. Turning sending on must not remove the per-act
  check, which is why `company-assistant` carries both.

## Related

- Owner/company identity split: `docs/components/magician/company-identity-split.md`.
- Build order and gates: `docs/archive/plans/2026-08-07-opc-readiness-review.md` §9.
