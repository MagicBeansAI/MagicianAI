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

## Chat channels: typed sends, narrow opaque-command denial

`whatsapp`, `telegram` and `telegram-self` each expose an explicit `send` action
(in `SENDING_ACTIONS`) with a typed recipient, which the suppression screen,
disclosure record and restricted form read:

| Capability | Send action | Recipient parameter | Body |
|---|---|---|---|
| `whatsapp` | `send` → `wu messages send <jid> <text> --json` | `jid` | `text` (+ optional `reply_to`) |
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
only live message path is typed `send`; media sends are unavailable until they
get typed actions. `whatsapp react` is unclassified: no body and no recipient.

Tests: `a_chat_channel_is_outward_through_its_explicit_send_action`,
`a_send_composed_inside_run_is_classified_without_classifying_reads`,
`reading_through_a_chat_channel_stays_ungated` (`agents::outward_actions`).

Typed sends carry:

- **Consequence class** `bounded_communication`, asserted equal to
  `kapso-whatsapp-send`. It is the one class a standing envelope may cover.
- **Restricted form** for all three; see `restricted-actions.md`.
- **Receipt**: `RECEIPT_SOURCES` keyed on capability **and `send`**, so a `run`
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

**The submit guard does stand in front of it.** Before any browser interaction,
`primitive_dispatch::browser::submit_guard` inspects the element it would hit
(`get box` → `document.elementFromPoint`) and refuses when it would submit: a
submit control of a non-GET form, a final-action label ("Submit", "Send
application", "Place order", "Pay", "Publish", "Sign up", …), Enter in a text
field of a non-GET form, or a script calling `form.submit()` /
`requestSubmit()`. GET forms (search, filters) and wizard words ("Next",
"Continue", "Apply") stay free. A refusal logs `[OUTWARD-REFUSED]` and tells
the agent to leave the form filled for the owner. Limits: an element the probe
cannot read (iframe, DOM-only engine, probe error) is allowed and logged, and a
script-driven form whose final control uses an unknown word is not caught.

## Whose identity a browser visit carries

`ConnectionMode::from_runtime_selection` resolves transport as
`MAGICIAN_AGENT_BROWSER_MODE` → the call's `connection_mode` → `"cdp"`. `Cdp`
attaches to the owner's real Chrome through the magicutor proxy
(`DEFAULT_MAGICUTOR_PROXY_URL`), with the owner's cookies and sessions.

The engagement guard in `primitive_dispatch::dispatch`
(`contained_retrieval_scope(...).map(|s| s.is_bound()).unwrap_or(true)`) refuses
CDP under an engagement or meeting and fails closed on an unreadable carrier.
An autonomous cycle (`agents::autonomous_goal`) carries
`engagement_authority: None` → `RetrievalScope::Unbound`, so the refusal does
not fire, which is right for the owner's own chat but skips the shape an
outward agent runs in. A delegated child inherits the parent's carrier
(`agents::runtime`), so research delegated from an unbound cycle is unbound
too. `skillshub/browser/SKILL.md` declares
`auth.profile_selection: mode: implicit` (one shared `browser_profile`).

Closing it needs:

1. **A company-owned profile the runtime can launch into**, still open.
   `engagement_browser_session_id` partitions `agent-browser --session <id>` per
   engagement, but `run_command_internal` passes no `--user-data-dir`-style
   flag, and a session id does nothing for an attached browser. The manifest can
   express a selector (`ProfileSelection::Fixed`/`Selectable`); the browser
   skill declares `Implicit`.
2. **Transport pinned to the agent**, built:
   `AgentDefinition::browser_transports`
   ([`agents/agent-definition-reference.md`](agents/agent-definition-reference.md#browser_transports)).

### Transport ceiling

`browser_transports` is a per-agent ceiling over `cdp` / `headed` / `headless`;
**empty means all three**. `dispatch_browser_primitive` applies it after
`ConnectionMode::from_call_arguments` folds call, operator override and
default, and it outranks all three. A transport outside the ceiling, or an
unknown name, **refuses**
(`NOT BROWSED — this agent may only use the browser transports […]`) rather
than substituting: a `[cdp]`-only ceiling asked for `headless` could only
substitute by promoting to owner identity. The ceiling is a fact about the
agent, so a delegate carrying its parent's authority cannot exceed its own
declaration; it is replaced, never merged, at owner transitions.

Shipped: `web-researcher`, `web-researcher-opc`, `cpo`, `cro`,
`presentation-maker` and `envoy` declare `[headless, headed]`
(`web-researcher` closes the ambassador's delegated-research path).
`personal-assistant` and `executive-assistant` declare nothing; acting as the
owner is their point. The engagement guard (keyed on the work carrier) and the
ceiling (keyed on the agent) complement each other. Test:
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
any side effect. The posture is published at boot in `magician-bin/src/main.rs`
(`install_outward_capture_posture`) before
the CLI branch and any server start. Boot logs `[OUTWARD-CAPTURE]` or
`[OUTWARD-LIVE]` at warn; each live dispatch logs `[OUTWARD-LIVE]` with
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
