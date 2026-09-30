# Restricted actions — the bindable form of an outward act

Plan §4A of `docs/archive/plans/2026-08-07-opc-approval-envelopes.md`.
Module: `magician/src/magician_v2/execution/restricted_action.rs`.

A primitive, like its neighbour `effective-action.md`. Any gate that grants
authority needs an act whose argument surface is **closed**; this builds one.

## Why not just inspect harder

`resolve_effective_action` recovers recipients hidden in a passthrough, which
makes a *record* truthful. It cannot make the act safe to authorise, because the
next token nobody modelled changes the act again.

The only fix is an action with no escape hatch at all — then `is_bindable()` is
true **by construction** rather than reported as false. Holding a `BoundDispatch`
is the proof: the sender is bound, the recipients are canonical, and an authority
decision taken on it is a decision on the act rather than on a description of it.

## Refuse, never strip

An escape hatch or an unknown parameter is **refused**, not silently dropped.
Dropping would dispatch a send that is not the one the agent composed, and
nobody would find out until a person read the mail. A refusal carries
`guidance()` naming the closed parameter set, so the next attempt can succeed.

A parameter supplied twice under two spellings (`to` and `TO`) is refused
rather than resolved: which value won would otherwise depend on hash order.

## The sender is bound, never caller-chosen

§4A.3. Whatever the caller supplied in a sender parameter is discarded and
rebound from the runtime's view of who is acting, so a compromised agent cannot
send as somebody else.

- **`agentmail-send`** binds `inbox_id` (the inbox to send from).
- **`gmail`** binds `account`, the skill's `profile_parameter` (business /
  personal / work).
- **`kapso-whatsapp-send`**, **`whatsapp`**, **`telegram`** and
  **`telegram-self`** bind nothing: each transport has one configured identity
  and no caller-settable field (Presto's WhatsApp number, one WhatsApp Web
  session, one bot token from the scoped vault, one MTProto session).

A spec that forgot its binding looks exactly like one with nothing to bind, so
the exempt set is a named list, `SENDER_IS_THE_TRANSPORT`, asserted by
`every_restricted_form_binds_a_sender_or_has_none_to_bind`; joining it must mean
reading the skill and finding no sender parameter. A second test asserts none of
the four allows `from`, `account`, `inbox_id` or `sender`.

Sender derivation follows the work the execution carries: an `Engagement`
carrier gives `SenderIdentity::Engagement`; a `Program` carrier gives
`FirstContact` with the programme id; no work gives `FirstContact` with the
sentinel `"unattributed"` (never a workspace name).

## The shipped skill is the source of truth

Forms composed from what an API plausibly looks like are wrong in both
directions — an invented parameter refuses a real send, a missing one refuses it
too, and each looks like the gate doing its job (an invented `from` binding also
leaves the real sender field unbound). So
`every_allowed_parameter_exists_in_the_shipped_skill` reads
`skillshub/<capability>/SKILL.md` and asserts every allowed and sender parameter
is declared there and none is a passthrough. A form is added only after its
skill declares the parameters.

Current chat-channel forms:

| spec | allowed | recipient | sender |
|---|---|---|---|
| `whatsapp` | `jid`, `text`, `reply_to` | `jid` | none — the session is the sender |
| `telegram` | `chat_id`, `text` | `chat_id` | none — the bot token is the sender |
| `telegram-self` | `to`, `message`, `reply_to` | `to` | none — the operator's own session |

`telegram`'s allowlist agrees with its adapter, which refuses unmodelled keys
with `unexpected_send_parameter` (so `parse_mode` is rejected in both places).

## Absent means refused

`restricted_form_for` returning `None` is a refusal, not a fallback. The table is
deliberately **shorter** than the outward capability table in
`agents::outward_actions` — `presto-gmail`, `calendar`, `presto-calendar` and
`imessage_send` are outward and have no restricted form, so an autonomous agent
cannot send through them. Adding a way to reach the world requires deciding its
bindable form, not inheriting authority by joining a list.

`calendar` and `presto-calendar` compose invitations entirely from raw argv
(`events_insert` takes one `args` parameter): no field to allow, no attendee to
canonicalise, no organiser to bind. `outward_dispatch_class` still classifies
and captures those invitations so they are recorded.

### What "absent" actually costs, so an owner reads it here

For a capability the outward table classifies:

- `restrict` answers `NoRestrictedForm`, so **every** classified send through it
  is refused at gate 1 of the outward gate — under capture as well as live;
- `restricted_toolset` **withholds** the sending leaf from the catalog
  altogether, callable tier and deferred names both.

That cost lands on the gated path only. An ungated sibling action
(`whatsapp__run`, `telegram__run`, `telegram-self__run`) keeps working, so an
absent form does not stop the send; it stops the send that could have been
recorded.

## Routing keys are not payload

`agents::approval::pack_action_for_approval` reads `action`, `action_type`,
`operation` and `tool_name` from the parameter map to decide which action a pack
is invoking, so they are on essentially every dispatch. They are exempt from the
closed-set check and preserved verbatim; without that, every live outward act is
refused with `UnknownParameter { "action" }` in a way that looks like the gate
working. A test pins the exempt list against the four the runtime reads.

## Recipients: one table, not two

`is_passthrough_parameter` is exported from `effective_action` and used here, so
both modules agree on what an escape hatch is. A test resolves every
`recipient_parameters` entry through `resolve_effective_action` and asserts it
is a recipient there too (e.g. `attendees`, `jid`). A recipient the resolver
ignores is worse than a mismatch on a `Message` act: an empty list makes
`Addressing::for_class(Message)` refuse with *"No recipient could be
extracted"*.

## Idempotency

The key is derived from what the act **does** — capability, action, bound
sender, canonical recipients, ordered parameters — never from when it was
attempted (a clock would make every retry a new send). Canonicalisation happens
first, so two spellings of one send produce one key; parameters go through a
`BTreeMap` so the key does not depend on hash order. A different sender is a
different act.

## Where it is enforced

`execute_action_inner`, as **gate 1 of the outward gate** — after the
work-context ceiling, before the suppression screen, and **ahead of the
capture/live branch**. Under capture (the default posture) an act that cannot be
reduced to a bindable form is refused with `guidance()` and its disclosure moves
to `failed`, rather than being rehearsed: rehearsing an act that can never be
authorised teaches the model that it is composable.

`outward_restriction_refusal` refuses on:

- `restricted_form_for` returning `None` (`NoRestrictedForm`);
- a passthrough, unknown, or twice-spelled parameter;
- a bound act whose `EffectiveAction::is_bindable()` is still false.

A non-`Pack` action yields `None`, which is not a hole:
`classify_outward_dispatch` only names a capability for a `Pack`.

**The `BoundDispatch` is consumed.** The act that dispatches is the bound one
(runtime sender, canonical recipients). The substitution happens *after* the
gates, so every gate judges the act as the agent composed it (what the
disclosure records) while what leaves is what §4A bound. Only the sender is
rewritten; anything else unknown was already refused.

`restricted_form_for` resolves the capability through
`agents::approval::pack_tool_for_approval` (the same delegation as
`outward_actions::capability_key`), so a flat leaf like `gmail__send` or
`telegram-self__send` maps to the same capability in the catalog and the gate.
Pinned by `a_compiled_leaf_resolves_to_the_capability_it_names`.

## The toolset says it first, the gate still decides

`execution::restricted_toolset` projects each capability before the model sees
it, in `build_pack_capability_tool` (which returns an `Option` so the withheld
case cannot be forgotten):

- **as declared** — the capability reaches nobody;
- **restricted** — it reaches people and has a bindable form: the closed
  argument surface replaces the declared one, and the description says so;
- **withheld** — it reaches people with no bindable form: offered nowhere, not
  even in deferred names, because a name `tool_search` can load is one the model
  will reach for.

The split is per action. The send allowlist applies only to leaves that send
(`agentmail-send`, `gmail__send`). A leaf that merely accepts a passthrough
loses the passthrough and keeps everything else (e.g. `gmail__triage` keeps its
search arguments). A leaf whose passthrough is **required** (argv-only skills,
`gmail__raw`, all of `calendar`) is withheld. The passthrough is judged from the
tool's **shape**, not one call's arguments — the same rule as
`EffectiveAction::is_bindable`.

This is a narrowing, never a widening: every withheld tool and removed parameter
is one the gate already refuses, so the projection never becomes a second
authority. It only saves the model an iteration spent composing an act it could
not send.

## Not built here

- **Attachment revision binding** (§4A.3's batch clause).
- **One idempotency key, not two.** `OutwardAssertionStore::prepare_dispatch`
  derives its key from the raw payload hash, so two spellings of one send read
  as two acts there while `BoundDispatch` collapses them. Closing it means the
  store taking the bound key, which needs the gate to restrict before it
  records — and it deliberately records before anything can refuse.
