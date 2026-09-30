# One-time credential prompts

The built-in browser credential flow uses the existing hidden password input
for every requested field, including username. Its prompt identifies the
website, tab and field and states that material is used once without saving.
After entry, a separate **Use once / Cancel** choice controls delivery.

The canonical HITL schema retains `request_type`. Dismissing a
`secure_browser_input` or `secure_browser_confirm` dialog posts `aborted` to
the scoped canonical HITL endpoint, releasing pending material promptly.
Generic dialog dismissal keeps its existing behavior. Secure values have no
prefilled/default value and are not added to the chat transcript.

See [one-time browser credentials](../magician/jit-browser-credentials.md) for
server-side custody, limitations, tests and the coordinated runtime/UI/skill
rollout. This does not change generic `need_user_input` into a secure credential
transport.

## Every secret ask, by the published spec (P3)

Since P3 the backend publishes its value-free classification of an ask on
`hitl.requested` as `input_schema.sensitive` and on the pending listings
(`HitlSensitiveSpec` in `src/lib/hitl/types.ts`, read by
`readSensitiveSpec` in `adapters.ts`). **The web client masks by that spec,
never by the request-type name or the wording**; the two built-in secure
browser types above are one producer of the same spec.

- `respondToHitl.ts`: `promptFor` renders a `text`/`guidance` ask the backend
  classified as a code as the `otp` prompt kind, and one classified as a
  password or another secret as `password`; the typed `otp` input type has its
  own kind. The value posted keeps the ask's wire type (a masked `text` ask
  posts `text`; an `otp` ask posts the `password` value shape), which is what
  the pause expects — the backend routes it to custody by the spec. A secret
  is never pre-filled from a re-ask's previous answer. Dismissing any ask with
  a spec posts `aborted`, like the built-in secure types.
- `HitlPromptFields.svelte`: `otp` renders a masked field with
  `autocomplete="one-time-code"` (and no `inputmode`: a code is an exact
  string, and a numeric keypad would lock out an alphanumeric one); `password` sets
  `autocomplete="off"`; a form masks exactly the fields the spec flags (or a
  question typed `password`/`otp`) and labels an identifier as kept private.
  A banner says what happens to the value; a `collection_deadline_ms` counts
  down, and past it the fields refuse the value and offer **Request a fresh
  code**, which dismisses the prompt (an explicit cancel). Typed values leave
  with the component. The task panel hands every secret ask (by spec or by
  type) to the modal — a drawer that stays up is not where a secret is typed —
  and its inline "fresh code" gesture cancels the ask rather than opening it.
- Form fields listen for the component's `change` event (it dispatches no DOM
  `input`), so Submit enables on entry.
- A credential prompt is never DISPLACED. `requestAttentionInput` resolves the
  previous prompt with `null` when another one opens, and a `null` on a
  sensitive ask posts `aborted` — so any other caller of the singleton prompt
  (a second attention item, editing a task description) would cancel the
  credential ask and retire its custody mid-typing. Prompts that arrive while a sensitive one is open
  are QUEUED (bounded) and open in turn as it is answered or dismissed; past the
  bound the NEWEST is refused rather than the oldest evicted, because evicting a
  queued sensitive prompt would resolve it `null` and post the very `aborted`
  this rule exists to prevent.

Tests: `respondToHitl.test.ts` ("the sensitive spec published by the
backend"), `promptFor.test.ts` (`readSensitiveSpec`, the envelope),
`HitlPromptFields.component.test.ts` ("one-time code and secret fields").

