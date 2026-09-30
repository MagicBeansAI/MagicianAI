# Outward actions — capture by default

An **outward action** is a dispatch that sends something out of the owner's
control: an email, a chat message, a calendar invitation. Readiness review §9
steps 2 and 4 make these the gate before anything autonomous may run.

## Classification is argument-aware, not token-only

A capability is outward when it can transmit; an action token alone is not
enough. `gmail`/`presto-gmail` expose a `raw` action (argv passthrough), so

```text
gmail  action=raw  args=["+send", "--to", "someone@example.com", …]
```

sends mail under a non-sending token. `outward_dispatch_class(capability, action,
params)` treats any dispatch of a world-reaching capability that carries an
unbounded argument passthrough as outward, whatever the token. The passthrough
test is `effective_action::is_passthrough_parameter` — the same one restriction
and effective-action use, so there is one answer to "what is an escape hatch";
a test asserts every known spelling trips this gate.

This deliberately over-gates (a `triage` with `extra_args` is captured): a
capture notice on a read is cheaper than a send nobody recorded.

## Classified centrally, not per agent

`agents::outward_actions::outward_action_class(capability, action)` is a central
table rather than a `requires_approval` entry per definition, so "an autonomous
agent cannot send" holds for every agent, including future ones — not just the
tools someone remembered to gate.

- **Reading is not outward.** Inbox listing, `agentmail-read`,
  `kapso-whatsapp-read` pass untouched; a gate that blocked research would be
  switched off.
- **Send-only capabilities are outward whatever action they name**
  (`imessage_send`, `agentmail-send`, `kapso-whatsapp-send`), pinned by a test so
  a new action token cannot open an invisible path.
- Compiled pack tools arrive as `pack__action`; the capability is resolved
  through `approval::pack_tool_for_approval`, the same split the approval gate
  uses, so the two gates protect the same set.
- Name-only predicates, both asked through `outward_action_class`:
  `capability_can_reach_the_world(capability)` and
  `capability_only_sends(capability)` (tells `agentmail-send`, where the bare
  invocation is the send, from `gmail`, whose name also stands for reads).

## Chat channels: typed sends, narrow opaque-command denial

`whatsapp`, `telegram` and `telegram-self` each expose an explicit `send` action
(in `SENDING_ACTIONS`) with a typed recipient, which is what the suppression
screen, disclosure record and restricted form read:

| Capability | Send action | Recipient parameter | Body |
|---|---|---|---|
| `whatsapp` | `send` → `wu messages send <jid> <text> --json` | `jid` | `text` (+ optional `reply_to`) |
| `telegram` | `send` → the adapter's own route, method fixed to `sendMessage` | `chat_id` | `text` |
| `telegram-self` | `send` → `tgcli send text --to … --message … --json` | `to` | `message` (+ optional `reply_to`) |

**Classifying every `run` is the wrong direction.** A classified act meets §4A's
bindability refusal, and `run`'s single free-text parameter can never be bound,
so it would refuse `chats list`, `contacts search`, `channels`, `getMe`,
`getChat` along with the send. A gate that refuses every read gets routed around.

**Structural deny rule.** `outward_dispatch_class` inspects only the command head
each fixed adapter understands: WhatsApp `messages send` / `media send`, Telegram
Bot API methods beginning `send`, `forward` or `copy`, Telegram-self `send text` /
`send photo` / `send file`. Those are classified outward, enter the disclosure
path, and fail closed at §4A (an opaque string cannot bind its recipient). Reads
(`chats list`, `contacts search`, `channels`, `getMe`, `getUpdates`) stay
unclassified. The only live message path is typed `send`; unmodelled media sends
are unavailable until they gain typed actions.

`whatsapp react` is deliberately unclassified: no body and no recipient the
screen could act on.

Tests: `a_chat_channel_is_outward_through_its_explicit_send_action`,
`a_send_composed_inside_run_is_classified_without_classifying_reads`,
`reading_through_a_chat_channel_stays_ungated` (`agents::outward_actions`).

What the typed sends carry:

- **Consequence class** `bounded_communication`, asserted equal to
  `kapso-whatsapp-send` so reclassifying one without the other fails a test.
  Bounded communication is the one class a standing envelope may cover.
- **Restricted form** for all three — see `restricted-actions.md`.
- **Receipt**: `RECEIPT_SOURCES` keyed on capability **and `send`**, so a `run`
  result is never read as a message id (`outward-assertions.md`).
- **`jid` is a recipient parameter** in `effective_action`. Without it a WhatsApp
  send resolves to no recipients and `Addressing::for_class(Message)`
  (`MustNameRecipients`) refuses — which would push a model back onto `run`.

## Form submission: a second table

The sending table's fallbacks (`execute` for a dispatch with no action, and argv
passthrough) are right for a transmitter and catastrophic for a browser, where
`args` is how every subcommand is spelled. So `SUBMITTING_CAPABILITIES` is a
separate table with a **per-capability** action list.

**Classified: `submit` and `submit_form`, nothing else.** Every interaction verb
(`click`, `press`/`key`, `eval`, `find`/`batch`, …) can submit a form, but
classifying them would refuse every autonomous click — a classified act meets
§4A's bindability refusal and `browser` has no restricted form (its surface is
argv; no sender to bind, no recipient to canonicalise). Reading, navigating,
staging input (`fill`, `type`, `select`) and clicking stay unclassified.

- **There is no `submit` subcommand**, so this table sees no browser submission
  today. The tokens are listed so a vocabulary that grows one does not arrive
  ungated. `no_action_the_browser_skill_declares_is_seen_by_the_outward_gate`
  (`magician/tests/agent_definition_templates_test.rs`) fails the day one does.
- **The token cannot tell a link from a submit control** — classify the click and
  navigation is refused; leave it and an application can be submitted with no
  record.
- The class is `form_submission` → `submission_or_publication`, **not** bounded
  communication, so an engagement envelope can never authorise pressing Submit.

**Capture does not stand in front of an unclassified submit.** The whole outward
block in `execute_action_inner` (disclosure, work-context ceiling, §4A refusal,
suppression, envelope shadow, capture) sits behind
`if let Some((capability, action_token, class)) = classify_outward_dispatch(action)`,
which is `None` for `browser`/`click`. What limits the act is the **grant**: the
shipped `ambassador` (whose acts speak in the company's name) has `browser` in
`denied_tools`. Other holders (`web-researcher`, `cpo`, `cro`,
`personal-assistant`, `executive-assistant`) remain exposed. Closing it properly
needs an explicit browser submit action, or a restricted browser form that binds
the target. Same trade as `calendar`: autonomous browsing works; the cost is
coverage.

## Whose identity a browser visit carries

`ConnectionMode::from_runtime_selection` resolves transport as
`MAGICIAN_AGENT_BROWSER_MODE` → the call's `connection_mode` → `"cdp"`. `Cdp`
attaches to the owner's real Chrome through the magicutor proxy
(`DEFAULT_MAGICUTOR_PROXY_URL`) with his cookies and sessions.

§5A.2's guard in `primitive_dispatch::dispatch`
(`contained_retrieval_scope(...).map(|s| s.is_bound()).unwrap_or(true)`) refuses
CDP under an engagement or meeting and fails closed on an unreadable carrier. An
autonomous cycle (`agents::autonomous_goal`) carries `engagement_authority: None`
→ `RetrievalScope::Unbound`, so the refusal does not fire — correct for the
owner's own chat, but it skips exactly the shape an outward agent runs in. A
delegated child inherits the parent's carrier (`agents::runtime`), so research
delegated from an unbound cycle is unbound too. `skillshub/browser/SKILL.md`
declares `auth.profile_selection: mode: implicit` (one shared `browser_profile`).

Closing it needs:

1. **A company-owned profile the runtime can launch into** — open.
   `engagement_browser_session_id` partitions `agent-browser --session <id>` per
   engagement, but `run_command_internal` passes no `--user-data-dir`-shaped
   flag, and a session id does nothing for a browser the runtime attached to. The
   manifest can express a selector (`ProfileSelection::Fixed`/`Selectable`); the
   browser skill declares `Implicit`.
2. **Transport pinned to the agent** — built:
   `AgentDefinition::browser_transports`
   ([`agents/agent-definition-reference.md`](agents/agent-definition-reference.md#browser_transports)).

### Transport ceiling

`browser_transports` is a per-agent ceiling over `cdp` / `headed` / `headless`;
**empty means all three**. `dispatch_browser_primitive` applies it after
`ConnectionMode::from_call_arguments` folds call, operator override and default,
and it outranks all three. A transport outside the ceiling **refuses**
(`NOT BROWSED — this agent may only use the browser transports […]`) rather than
substituting — a `[cdp]`-only ceiling asked for `headless` could only substitute
by promoting to owner identity. An unrecognised name refuses too. The ceiling is
a fact about the agent, so a delegate carrying its parent's authority cannot
exceed its own declaration; it is replaced, never merged, at owner transitions.

Shipped: `web-researcher`, `cpo`, `cro` declare `[headless, headed]`
(`web-researcher` closes the ambassador's delegated-research path).
`personal-assistant` and `executive-assistant` declare nothing — acting as the
owner is their point. §5A.2 (keyed on the work carrier) and the ceiling (keyed on
the agent) are complementary. Test:
`an_execution_with_no_engagement_still_browses_as_the_owner`
(`magician/tests/engagement_scoped_retrieval.rs`).

A ceiling withholds the owner's identity; it does not grant a company one. An
agent under `[headless, headed]` browses as nobody. Where a company identity is
needed, **do not grant `browser` to an agent whose acts speak for somebody**.

## Capture is the posture unless sending was turned on

`outward_actions.capture_only` defaults to **true**. Under capture an outward
dispatch is logged and **not performed**, and the model gets a result beginning
`NOT SENT — captured` that tells it not to claim otherwise — a capture that
resembled success would lead the model to tell the owner it sent.

`outward_capture_only()` returns `true` when no posture was installed, so a test,
a partially initialised binary or an unwired entry point cannot send.

## Where it is enforced

At the top of `execute_action_inner`, the one function every action passes
through, before any side effect. The posture is published at boot in
`bin/magician.rs` before the CLI branch and any server start (the executor holds
no config snapshot). Boot logs `[OUTWARD-CAPTURE]` or `[OUTWARD-LIVE]` at warn;
each live dispatch logs `[OUTWARD-LIVE]` with capability, action, class, session
and agent — grep it after turning capture off.

## What this does NOT cover

- **Browser form submission** — see *Form submission* above.
- **Whose identity a browser act carries** — this module classifies *what* an act
  is; identity is constrained by §5A.2 and `browser_transports`.
- **Rate/volume caps** (§9 step 4) live in `resource_authority.budgets`:
  EMAIL_SENDS and WHATSAPP_SENDS are 50/day, matching the skills' `SKILL.md`,
  because the agent plans against its skill doc. Change `magician-config.yaml`
  (seed and live copy) and the `SKILL.md` together.
- **Calendar writes are seen only by argument shape.** `SENDING_ACTIONS` lists
  `create_event` and `invite`, but `calendar`/`presto-calendar` use
  `events_insert`/`events_patch`/`events_delete`. The static classifier fails
  these closed to `commitment_or_transaction`; the capture gate catches them
  because `events_insert` requires an `args` passthrough, so
  `outward_dispatch_class` returns `CalendarInvite` — only while the capability
  is in `OUTWARD_CAPABILITIES`. If a skill ever types `events_insert`'s
  parameters, `SENDING_ACTIONS` must gain the real tokens first.
- **`run` commands outside the recognised send heads** remain invisible to this
  gate; only the heads in the structural deny rule are classified.
- **Approval is independent.** Capture is an operator-wide posture;
  `requires_approval` is per act. Turning sending on must not remove the per-act
  check, which is why `company-assistant` carries both.

## Related

- Owner/company identity split: `docs/components/magician/company-identity-split.md`.
- Build order and gates: `docs/archive/plans/2026-08-07-opc-readiness-review.md` §9.
