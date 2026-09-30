---
name: "work-modules"
version: 0.1.0
description: "Run the composable work modules: durable multi-session runs with grounded answers and owner-facing gaps (Module C), scheduling negotiations with somebody outside (Module A), the obligation register — what is owed, by whom, by when (Module D), and claim manifests binding which claims an artifact revision carries (Module B substrate). Pull this whenever the work spans sessions, waits on somebody else, promises something with a deadline, or produces an artifact that makes claims. It RECORDS and READS; it never sends, books, or reads an inbox — the acting is another capability's, and submission is always a person's."
metadata:
  magician:
    skill_type: tool
    user_invocable: false
    requires:
      bins: ["work-modules"]
    install_hint:
      docs: "No credentials. Talks plain HTTP to the local Magician runtime (`MAGICIAN_API_BASE`, default http://127.0.0.1:3002), so it needs no CA bundle — which matters, because the governed runtime env_clears SSL_CERT_FILE."
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          every action reads or writes an owner's own work register in a live
          runtime. A canary would either need a running server or would write a
          fabricated run into somebody's scope.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: ["work-modules"]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin:
          mode: required
          sensitivity: private
        working_directory:
          mode: workspace
        limits:
          timeout_secs: 60
          stdin_bytes: 1048576
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: none
        requirement: none
      policy_floor:
        # `ordinary`, deliberately. Nothing here reaches a person: it records
        # what an agent already decided or read, into the owner's own stores.
        # The two actions that WOULD reach the world are not in this
        # capability at all — see the note below.
        approval: ordinary
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        open_run:
          description: >-
            Open a durable run for a long multi-session piece of work — an application, a CFP, an onboarding flow.
            Returns the run id every later action needs.
            A run survives restarts; a half-filled form that nothing remembers is worse than one never started.
          fixed_args: [open_run]
          timeout_secs: 60
          parameters:
            purpose:
              type: string
              description: "What this run is for, in the owner's words."
              required: true
              max_length: 8192
            resource_ref:
              type: string
              description: "The thing being worked on \u2014 a URL, a portal id, a document ref."
              required: true
              max_length: 8192
            opened_by:
              type: string
              description: "Who opened it. Recorded, never inferred."
              required: true
              max_length: 8192
            audience_kind:
              type: string
              description: "`engagement` or `program`, when the run belongs to a relationship."
              max_length: 8192
            audience_id:
              type: string
              description: "The engagement or program id."
              max_length: 8192
            fields:
              type: json_array
              description: "Fields to declare up front: `[{\"name\":..., \"required\":true}]`. Survey the whole form before filling any of it."
        list_runs:
          description: >-
            Every run in this scope.
          fixed_args: [list_runs]
          timeout_secs: 60
        read_run:
          description: >-
            One run: its fields, answers, open gaps and outstanding expectations.
          fixed_args: [read_run]
          timeout_secs: 60
          parameters:
            run_id:
              type: string
              description: "The run."
              required: true
              max_length: 8192
        declare_field:
          description: >-
            Declare a field discovered after the run opened — a conditional section that only appeared once an earlier answer was given.
          fixed_args: [declare_field]
          timeout_secs: 60
          parameters:
            run_id:
              type: string
              description: "The run."
              required: true
              max_length: 8192
            name:
              type: string
              description: "The field's label as written, not a paraphrase."
              required: true
              max_length: 8192
            required:
              type: boolean
              description: "Whether the form requires it."
              required: true
        answer:
          description: >-
            Record a grounded answer.
            `evidence_refs` is not optional decoration: an answer with no evidence is unrepresentable here, because an invented figure on a real application is unrecoverable in a way a missed deadline is not.
          fixed_args: [answer]
          timeout_secs: 60
          parameters:
            run_id:
              type: string
              description: "The run."
              required: true
              max_length: 8192
            field:
              type: string
              description: "Which declared field."
              required: true
              max_length: 8192
            text:
              type: string
              description: "The answer as it will be submitted."
              required: true
              max_length: 8192
            evidence_refs:
              type: string_array
              description: "What the answer rests on. Empty is refused."
              required: true
              max_items: 16
              max_item_bytes: 4096
        raise_gap:
          description: >-
            Raise a question the evidence store cannot ground, for the owner.
            A gap is paid for once: the owner's reply becomes new evidence.
          fixed_args: [raise_gap]
          timeout_secs: 60
          parameters:
            run_id:
              type: string
              description: "The run."
              required: true
              max_length: 8192
            field:
              type: string
              description: "Which field is unanswerable."
              required: true
              max_length: 8192
            question:
              type: string
              description: "What you need from the owner."
              required: true
              max_length: 8192
        resolve_gap:
          description: >-
            Record the owner's reply to a gap, which answers the field and files the reply as evidence.
          fixed_args: [resolve_gap]
          timeout_secs: 60
          parameters:
            run_id:
              type: string
              description: "The run."
              required: true
              max_length: 8192
            field:
              type: string
              description: "Which field."
              required: true
              max_length: 8192
            question:
              type: string
              description: "The question that was asked, verbatim."
              required: true
              max_length: 8192
            text:
              type: string
              description: "The answer."
              required: true
              max_length: 8192
            evidence_refs:
              type: string_array
              description: "What it rests on."
              required: true
              max_items: 16
              max_item_bytes: 4096
            resolved_by:
              type: string
              description: "Who answered."
              required: true
              max_length: 8192
        expect:
          description: >-
            Declare that this run is waiting on something out-of-band — a verification code, a countersignature, a reply.
            The run parks rather than spinning.
          fixed_args: [expect]
          timeout_secs: 60
          parameters:
            run_id:
              type: string
              description: "The run."
              required: true
              max_length: 8192
            description:
              type: string
              description: "What is being waited for."
              required: true
              max_length: 8192
            source_hint:
              type: string
              description: "Where it will arrive \u2014 an inbox, a channel, a person."
              required: true
              max_length: 8192
        fulfil_expectation:
          description: >-
            Record that the awaited thing arrived.
            Takes what you already extracted; this capability reads no inbox.
          fixed_args: [fulfil_expectation]
          timeout_secs: 60
          parameters:
            run_id:
              type: string
              description: "The run."
              required: true
              max_length: 8192
            event_ref:
              type: string
              description: "The message or event it arrived in."
              required: true
              max_length: 8192
            source_hint:
              type: string
              description: "The hint the expectation was raised under."
              required: true
              max_length: 8192
            at:
              type: string
              description: "RFC 3339 instant it arrived. Defaults to now."
              max_length: 8192
        offer_times:
          description: >-
            Open a scheduling negotiation by offering slots to somebody outside.
          fixed_args: [offer_times]
          timeout_secs: 60
          parameters:
            audience_kind:
              type: string
              description: "`engagement` or `program`."
              required: true
              max_length: 8192
            audience_id:
              type: string
              description: "The relationship this happens in."
              required: true
              max_length: 8192
            counterparty:
              type: string
              description: "Who is being offered the time."
              required: true
              max_length: 8192
            purpose:
              type: string
              description: "What the meeting is for."
              required: true
              max_length: 8192
            slots:
              type: json_array
              description: "`[{\"start\":\"...\",\"end\":\"...\"}]`, RFC 3339."
              required: true
            offer_act_ref:
              type: string
              description: "The outward act that carried the offer, when one did."
              max_length: 8192
        list_negotiations:
          description: >-
            Every negotiation in a relationship.
          fixed_args: [list_negotiations]
          timeout_secs: 60
          parameters:
            audience_kind:
              type: string
              description: "`engagement` or `program`."
              max_length: 8192
            audience_id:
              type: string
              description: "The relationship."
              max_length: 8192
        read_negotiation:
          description: >-
            One negotiation: what was offered, what came back, and what is still open.
          fixed_args: [read_negotiation]
          timeout_secs: 60
          parameters:
            negotiation_id:
              type: string
              description: "The negotiation."
              required: true
              max_length: 8192
            audience_kind:
              type: string
              description: "`engagement` or `program`."
              required: true
              max_length: 8192
            audience_id:
              type: string
              description: "The relationship \u2014 the store keeps one log per relationship, so an id alone cannot say which log to open."
              required: true
              max_length: 8192
        record_reply:
          description: >-
            Record what a counterparty said.
            The READING is supplied, never inferred: turning prose into a verdict is language work, and a parser guessing `accepted` from `no problem` puts words in their mouth.
          fixed_args: [record_reply]
          timeout_secs: 60
          parameters:
            negotiation_id:
              type: string
              description: "The negotiation."
              required: true
              max_length: 8192
            audience_kind:
              type: string
              description: "`engagement` or `program`."
              required: true
              max_length: 8192
            audience_id:
              type: string
              description: "The relationship."
              required: true
              max_length: 8192
            source_ref:
              type: string
              description: "The message this was read from. The store's idempotency key \u2014 a re-read inbox re-delivers the same source."
              required: true
              max_length: 8192
            at:
              type: string
              description: "When THEY replied, RFC 3339. The message's own time, never your clock."
              required: true
              max_length: 8192
            reading:
              type: string
              description: "`accepted`, `declined` or `countered`."
              required: true
              max_length: 8192
            starting_at:
              type: string
              description: "For `accepted`: the instant they took, RFC 3339."
              max_length: 8192
            reason:
              type: string
              description: "For `declined`: what they said, when they said why."
              max_length: 8192
            slots:
              type: json_array
              description: "For `countered`: what they proposed instead."
        re_offer:
          description: >-
            Offer fresh slots after a decline or a counter.
          fixed_args: [re_offer]
          timeout_secs: 60
          parameters:
            negotiation_id:
              type: string
              description: "The negotiation."
              required: true
              max_length: 8192
            audience_kind:
              type: string
              description: "`engagement` or `program`."
              required: true
              max_length: 8192
            audience_id:
              type: string
              description: "The relationship."
              required: true
              max_length: 8192
            slots:
              type: json_array
              description: "The new slots."
              required: true
            offer_act_ref:
              type: string
              description: "The outward act that carried them."
              max_length: 8192
        hold:
          description: >-
            Record that a calendar event now holds the agreed time.
            The booking happens elsewhere; this records that it did.
          fixed_args: [hold]
          timeout_secs: 60
          parameters:
            negotiation_id:
              type: string
              description: "The negotiation."
              required: true
              max_length: 8192
            audience_kind:
              type: string
              description: "`engagement` or `program`."
              required: true
              max_length: 8192
            audience_id:
              type: string
              description: "The relationship."
              required: true
              max_length: 8192
            calendar_event_ref:
              type: string
              description: "The event that holds it."
              required: true
              max_length: 8192
        reschedule:
          description: >-
            Reopen an agreed time.
          fixed_args: [reschedule]
          timeout_secs: 60
          parameters:
            negotiation_id:
              type: string
              description: "The negotiation."
              required: true
              max_length: 8192
            audience_kind:
              type: string
              description: "`engagement` or `program`."
              required: true
              max_length: 8192
            audience_id:
              type: string
              description: "The relationship."
              required: true
              max_length: 8192
            reason:
              type: string
              description: "Why."
              required: true
              max_length: 8192
        close:
          description: >-
            Close a negotiation that will not produce a meeting.
          fixed_args: [close]
          timeout_secs: 60
          parameters:
            negotiation_id:
              type: string
              description: "The negotiation."
              required: true
              max_length: 8192
            audience_kind:
              type: string
              description: "`engagement` or `program`."
              required: true
              max_length: 8192
            audience_id:
              type: string
              description: "The relationship."
              required: true
              max_length: 8192
            reason:
              type: string
              description: "Why."
              required: true
              max_length: 8192
        record_obligation:
          description: >-
            Record a promise with a deadline and a direction.
            `owed_by_us` and `owed_to_us` need different words and different urgency, so they are kept apart rather than counted together.
          fixed_args: [record_obligation]
          timeout_secs: 60
          parameters:
            audience_kind:
              type: string
              description: "`engagement` or `program`."
              required: true
              max_length: 8192
            audience_id:
              type: string
              description: "The relationship."
              required: true
              max_length: 8192
            what:
              type: string
              description: "What was promised, in the words it was promised in."
              required: true
              max_length: 8192
            due_at:
              type: string
              description: "The deadline, RFC 3339."
              required: true
              max_length: 8192
            direction:
              type: string
              description: "`owed_by_us` or `owed_to_us`."
              required: true
              max_length: 8192
            created_by:
              type: string
              description: "Who recorded it."
              required: true
              max_length: 8192
            program_id:
              type: string
              description: "The programme, when it is not the audience."
              max_length: 8192
            source_act_ref:
              type: string
              description: "The act the promise was made in."
              max_length: 8192
        list_obligations:
          description: >-
            What is owed.
            Defaults to unsettled only, because the question a cycle asks is what is still outstanding.
          fixed_args: [list_obligations]
          timeout_secs: 60
          parameters:
            audience_kind:
              type: string
              description: "`engagement` or `program`."
              max_length: 8192
            audience_id:
              type: string
              description: "The relationship."
              max_length: 8192
            direction:
              type: string
              description: "`owed_by_us` or `owed_to_us`. Absent means both, still kept apart in the answer."
              max_length: 8192
            lapsed_only:
              type: boolean
              description: "Narrow to what is past its deadline and unsettled."
            include_settled:
              type: boolean
              description: "Include settled rows."
        settle_obligation:
          description: >-
            Settle an obligation.
            Settling is terminal and never a deletion.
          fixed_args: [settle_obligation]
          timeout_secs: 60
          parameters:
            obligation_id:
              type: string
              description: "The obligation."
              required: true
              max_length: 8192
            audience_kind:
              type: string
              description: "`engagement` or `program`."
              required: true
              max_length: 8192
            audience_id:
              type: string
              description: "The relationship."
              required: true
              max_length: 8192
            settlement:
              type: string
              description: "How it was settled."
              required: true
              max_length: 8192
            note:
              type: string
              description: "Free text."
              max_length: 8192
            reason:
              type: string
              description: "Why, when the settlement needs one."
              max_length: 8192
        bind_claims:
          description: >-
            Bind the claims an artifact revision carries.
            The built-side twin of the outward assertions store: this is what makes `which decks said this` answerable when a claim turns out to be wrong.
          fixed_args: [bind_claims]
          timeout_secs: 60
          parameters:
            artifact_ref:
              type: string
              description: "The artifact."
              required: true
              max_length: 8192
            revision_ref:
              type: string
              description: "The exact revision. A revision, not the artifact \u2014 claims change between revisions."
              required: true
              max_length: 8192
            claims:
              type: json_array
              description: "`[{\"claim_ref\":..., \"evidence_refs\":[...]}]`."
              required: true
            bound_by:
              type: string
              description: "Who bound them."
              required: true
              max_length: 8192
        read_manifest:
          description: >-
            Which claims a revision carries.
          fixed_args: [read_manifest]
          timeout_secs: 60
          parameters:
            artifact_ref:
              type: string
              description: "The artifact."
              max_length: 8192
            revision_ref:
              type: string
              description: "The revision."
              max_length: 8192
        carrying_claim:
          description: >-
            Which artifacts carry a claim.
            `latest_only` asks `still carrying it now`, which is what a correction chases; the default is the full history — `carried it once`.
          fixed_args: [carrying_claim]
          timeout_secs: 60
          parameters:
            claim_ref:
              type: string
              description: "The claim."
              required: true
              max_length: 8192
            latest_only:
              type: boolean
              description: "Narrow to revisions that still carry it."
    runtime_catalog:
      categories:
      - work
      - coordination
      composition_category: execution
      expose_timeout_control: false
      timeout_default_secs: 60
---

# work-modules — the durable half of long work

Doc: `docs/plans/2026-08-07-opc-composable-work-modules.md`.

Four registers, one adapter. Each answers a question that a chat transcript
cannot, because a transcript is not a record you can query later:

| module | the question |
|---|---|
| **C** — runs | *what is filled, what is pending, what changed since* |
| **A** — negotiations | *what did we offer, what came back, what is agreed* |
| **D** — obligations | *what is owed, by whom, by when* |
| **B** — claim manifests | *which claims does this revision carry* |

## The two things this capability deliberately cannot do

**It cannot submit.** A submission is public and irreversible, and no standing
permission covers one. You draft the whole thing and hand it over; a person
presses send. There is no action here that does it, and that absence *is* the
enforcement — not a rule you are being asked to remember.

**It cannot sweep the obligation register.** A sweep is operator maintenance
over a whole scope, not a step in anybody's work.

Both remain on the owner routes, which is where they belong.

## And three it does not pretend to do

- **It reads no inbox.** `fulfil_expectation` takes what you already extracted.
- **It books no calendar.** `hold` records that an event holds the time; making
  the event is another capability's job.
- **It sends nothing.** `offer_times` records that an offer was made. The
  message goes out through a channel capability, and its act ref comes back
  here as `offer_act_ref` so the two are joined.

That separation is the whole reason these registers are reusable: a supplier
order, a job application, a grant and a fundraise are the same four questions
with different nouns, and the moment one of them learned about email it would
stop being any of the others.

## Grounding is enforced, not encouraged

`answer` requires `evidence_refs`. An answer with no evidence is not
representable, because an invented metric on a real application is unrecoverable
in a way a missed deadline is not. When the evidence store cannot ground a
field, that is a `raise_gap` — an owner-facing question — never synthesised
text. The owner's reply becomes evidence, so each gap is paid for once.

## Readings are supplied, never inferred

`record_reply` takes `reading` as `accepted` / `declined` / `countered`. Turning
prose into a verdict is language work that belongs to whoever holds the
conversation. A parser reading "no problem, but let me check with my co-founder"
as an acceptance puts words in a counterparty's mouth and then books a room.

## Refusals are information

A `4xx` comes back as `status: "refused"` with the store's own sentence in
`response`. Every refusal here is a rule about what may be recorded — an
acceptance of a slot nobody offered, a rebind that would change a revision's
claims, a settlement of an obligation that is already settled. Read the
sentence; retrying the same call harder will not change it.

## Scope

`principal` / `workspace` may be passed per call, or come from
`MAGICIAN_PRINCIPAL` / `MAGICIAN_WORKSPACE`. The runtime resolves the scope from
its own boundary regardless; passing a different one does not widen anything.

## The cycle these are for

The program document says what good looks like; you sequence it.

```
1. Open a run for the piece of work.               open_run
2. Survey it. Declare what it needs.               declare_field
3. Answer what you can ground.                     answer
4. Raise what you cannot, once.                    raise_gap
5. Park on anything out-of-band.                   expect
6. When it arrives, record that.                   fulfil_expectation
7. Hand the draft to a person.                     (not this capability)

On a reply that wants a meeting:                   offer_times → record_reply
                                                   → re_offer / hold
Anything promised with a date:                     record_obligation
Anything produced that makes claims:               bind_claims
```
