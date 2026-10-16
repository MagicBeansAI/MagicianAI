# Effective action — what a dispatch will actually do

A primitive, not a feature. **Any** gate that decides from a dispatch's
arguments — approval, spend ceilings, capture, audit, authority envelopes — is
unsound if the arguments it inspected can be widened by arguments it did not.

`magician/src/magician_v2/execution/effective_action.rs`.

## The concrete hole

Skill command templates support a `passthrough` mapping: a parameter whose string
array is appended to `argv` verbatim. The shipped `agentmail-send` skill
documents the consequence outright:

> `to` — "Primary recipient. **For multiple recipients pass extra `--to` pairs
> via `extra_args`.**"

So a gate reading the typed `to` field sees one recipient while five receive the
mail. `kapso-whatsapp-send` exposes the same hatch.

Nothing about this is specific to email, or to outward work, or to any one plan.
It is a general property of any template that forwards raw tokens, and it defeats
any decision made on typed arguments.

## Two questions, deliberately separate

**What does this act actually touch?** `EffectiveAction::recipients` merges typed
fields with values expanded out of passthrough tokens — both `--to value` and
`--to=value` spellings — canonicalised and deduplicated. A record or a log built
from this describes the real act.

**May a decision be BOUND to this?** `is_bindable()` is false whenever an escape
hatch is present **at all**, even when every token in it was recognised, and even
when it is empty.

That second point is the one worth being clear about. Expanding what we recognise
makes a *description* truthful; it cannot make an open-ended token list safe to
authorise against, because the property that matters is that nothing outside what
was inspected can still change the act. An empty hatch still blocks binding:
bindability is a property of the action's **shape** — the parameter is accepted,
so the next call can fill it — not of one call's arguments happening to be tame.

**The fix for a false result is not better parsing.** It is a restricted form of
the action with no escape hatch, at which point `is_bindable()` is true by
construction.

## How to use it

- A gate that **describes** — audit, capture telemetry, a disclosure record —
  uses `recipients`, so what it writes down is what happened.
- A gate that **grants authority** uses `is_bindable()` and refuses when false.
  Authorising an unbindable act means authorising a description of it.

`unmodelled_tokens` names the tokens that were not recognised. It exists so a
refusal can be explained: a gate that fails closed without being able to say why
gets switched off.

## Where it is used today

The outward disclosure write point (`docs/components/magician/outward-assertions.md`)
resolves the effective action and records **everyone reached**, rather than the
typed `to`, so correction propagation aims at every real recipient.

The same write point logs `[OUTWARD-UNBINDABLE]` when a dispatch accepts a
passthrough. It **records rather than refuses**, deliberately: that gate
describes acts, and refusing there would block every send through a skill that
merely accepts a passthrough, which is most of them. Failing closed on an
unbindable act is an authority decision, and belongs to the envelope resolver.

## Why this matters beyond one plan

Approval Envelopes §4A states that until decisions bind the canonical effective
action, envelopes *"authorise a description of an act rather than the act"*. That
is true of every gate, not only envelopes — which is why this is a primitive in
`execution/` rather than part of any plan's module.
