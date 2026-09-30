# One-time browser credentials

Magician's built-in `browser__secure_prompt_fill` asks for credentials only when
an automation needs them. It uses Magician HITL and the in-process MagicVault
browser adapter. No standalone MagicVault MCP connection, daemon, vault unlock,
credential enrollment, or MagicRun source change is required.

The agent navigates to the login page and supplies field **metadata**:

```json
{
  "top_origin": "https://example.com",
  "fields": [
    { "field_name": "username", "css": "#username" },
    { "field_name": "password", "css": "#password" }
  ]
}
```

Magician resolves one main-frame target before prompting. An optional backend
`tab_id` disambiguates multiple tabs on the same origin. All values, including
username, use hidden HITL fields in the authenticated UI; prompts show origin,
tab and selector. Final **Use once** approval authorizes filling; **Cancel**,
dismissal, execution cancellation and the 180-second overall deadline discard
the pending material. Filling never submits the form.

## Custody and delivery

- `UserRequestService::ask_sensitive_once` installs a private oneshot receiver
  before publishing the request. Under the scoped first-response lock, submitted
  material moves into a zeroizing buffer; the ordinary response is reduced to a
  fixed status before history persistence, event publication or agent delivery.
- Only the trusted in-process operation owns the receiver. Cancellation closes
  it and retires the prompt; restored requests have no receiver and cannot
  accept credentials; orphan resolution discards secure input.
- Passwords are never enrolled or written to the vault. Request metadata and
  content-free decisions remain in history.
- The HTTP handler keeps no copy for generic response relays; confirmation input
  is discarded. Values move into MagicVault `MaterialField`, which zeroizes on
  drop. These are lifetime reductions, not a claim that browsers, HTTP parsing,
  OS buffers or all allocator copies are erased.

The `magicvault-effect` adapter performs a bounded isolated-world CDP fill,
validates every selector, and rechecks frame, document and exact origin before
mutation. Magician bypasses CLI argument construction, progress output,
workflow/API replay, and generic browser result/trace processing for this
operation. It returns only field states, fixed errors, and `saved:false`,
`submitted:false`. Partial/uncertain outcomes are never retried automatically.

Runtime-owned effect identities are sealed in a bounded process-local set
(4,096 attempts). A repeated decision or exhausted capacity refuses without
prompting or filling. Restart destroys all material and the set; another attempt
needs fresh UI input and consent. No persistent replay credential or reusable
plaintext reference is created.

## Supported boundary

- Operator-configured local CDP endpoint and main-frame HTML input elements
  only. Rejected: model-selected CDP endpoints, retrieval-handoff overrides,
  headed/headless sessions, iframes, unsupported input types, ambiguous targets.
- HTTPS required except explicit loopback HTTP development sites. Browser
  transport ceilings and work confinement checks still run before dispatch.
- The website receives the values; this does not stop site scripts reading them
  or a later, independent browser inspection tool. Generic `need_user_input`,
  saved-secret substitution, network capture outside this operation, OTP
  retrieval and all-channel secure input are separate surfaces, not certified by
  this flow. Browser credential requests should use this dedicated action.
- The adapter requires websocket payload logging disabled: Magician enables
  `log`'s `max_level_off` and `release_max_level_off` features (`tracing`
  instrumentation is unaffected; `log`-facade-only events are disabled).
- The local Magicutor proxy supports a reversible scoped endpoint alias so IDs
  with underscores and transport-ceiling suffixes reach the same owned tabs.

## MagicVault dependency mapping

Cargo resolves these libraries from the public
`https://github.com/MagicBeansAI/MagicVault.git` repository, with exact revisions
recorded in the workspace manifest and lockfile:

| Library | Version | Revision |
| --- | --- | --- |
| `magicvault-effect` | 0.7.1 | `5849709138e1d0ee9f2b7da4ca71b896eedbf3b1` |
| `magicvault-protocol` | 0.7.0 | `5849709138e1d0ee9f2b7da4ca71b896eedbf3b1` |
| `magicvault-core` | 0.1.6 | `5849709138e1d0ee9f2b7da4ca71b896eedbf3b1` |
| `magicvault-primitives` | 0.1.2 | `5849709138e1d0ee9f2b7da4ca71b896eedbf3b1` |

All four pin one MagicVault revision in the root `Cargo.toml`; the earlier
`[patch]` over `../MagicVault-secure-hitl` is gone (the secure-HITL work landed on
MagicVault `main`). Magician owns its HITL/material channel and calls the adapter
directly, so it needs the published adapter contract, not the standalone
MagicVault broker/MCP wire protocol. There are no machine-specific path
dependencies or runtime connections to another MagicVault checkout.

MagicRun `tool-runtime-core` is at 0.1.81, commit
`c65fbbaac46a70f8a1247307fc7daa5ded018275`; the lockfile's two source spellings
resolve to it. Use locked Cargo resolution when verifying this mapping.

Tests: `cargo test -p magician --lib jit_`; UI
`src/lib/hitl/respondToHitl.test.ts`, `src/lib/hitl/promptFor.test.ts`.

## Relation to the shared sensitivity contract (P1)

The one-time browser prompt is one producer of the shared contract in
[HITL / Attention](hitl-attention.md#sensitivity-contract-secure-hitl-credentials-p1):
`ask_sensitive_once` requests are classified `password`, one-time, `producer`
at acceptance, and keep their private oneshot receiver; every other sensitive
answer goes to the service's custody and is taken by reference.

## One-time custody in the shared core (P2)

`magicvault-core` (0.1.4 onward; 0.1.6 pinned) adds the custody the OTP path
needs; re-exports live in `magician_v2::secrets`:

- an injectable store clock (`CustodyClock`; `SecretStoreResolver::with_clock`,
  `ManualClock` under `test-fixtures`) — every deadline is an absolute timestamp
  checked at the moment of the operation;
- bounded ephemeral entries (`register_ephemeral_bounded`) that no reader or
  placeholder resolves at or past their deadline;
- one-time material: `register_one_time` (scope, challenge id, expected
  destination, deadline clamped to `ONE_TIME_MAX_RETENTION_MS` = 10 min) →
  `reserve_one_time` for exactly one bound claim → `consume_one_time` when
  submission starts; `release_one_time(PreDispatchFailure)` is the only way
  back; plus `cancel_one_time`, lazy and swept expiry, value-free
  `OneTimeReceipt`s and `one_time_*` audit lines. Nothing persists: a restart
  loses the code and the run fails closed to a fresh ask. One-time material is
  never a placeholder read and retires with `clear_ephemeral`.

`otp` inputs register through `register_one_time`; passwords through the
bounded variant at the resume site. Core `0.1.5` lets a one-time receipt name
its bound destination. Contract test:
`magician/tests/magicvault_facade.rs::p0_a_one_time_code_cannot_be_used_twice_from_ephemeral_custody`.

## Referenced material through the typed fill (P4)

`browser__secure_prompt_fill` is the browser lane's only sink for material the
run already holds. A field may carry `value: "[REF:<key>]"` — exactly one
reference, never a typed value (`invalid_request`) — naming a password or
one-time code the user gave through `need_user_input`.

1. Each reference resolves from the run's ephemeral scope before any prompt.
   Destination binding is checked first: material bound to another origin
   refuses (`material_unavailable`, `bound to <origin>`) before anything is
   asked or filled.
2. A password lowers by a plain scoped read. A code lowers by
   `reserve_one_time` with a claim naming this exact origin
   (`scheme://host[:port]`, same vocabulary as the HTTP lane and challenges)
   AND the challenge still standing at that origin; a code bound elsewhere,
   expired, consumed, or collected for an earlier challenge refuses with the
   store's reason. Fields without a value are prompted.
3. **Use once** is asked unless every field is a code bound to exactly this
   origin — the challenge that named the destination already carried consent.
4. Immediately before consuming, the fill rechecks the page (same single target,
   origin, document). A changed page, declined confirmation, ambiguous target,
   missing sibling material, cancel or deadline releases every reservation
   (`Referenced` releases what it holds on drop).
5. Codes are consumed as the fill starts; a selector the adapter cannot resolve
   after that spends the code — the accepted residual, narrowed by the recheck.

Several fields naming the same reference are a segmented input (one box per
character): resolved, reserved, split in field order and spent once; the model
never holds a per-digit reference. A field count the material does not split
into is `invalid_request` (naming the count, never the length) before anything
is asked or spent.

Leak controls after delivery:

- Delivered values join the run's scrub set (keyed per delivery) and are
  scrubbed from traces by value.
- Image captures through the browser dispatcher are withheld until a navigation
  command, or a text observation without a delivered value, shows the page moved
  on; a click proves nothing.
- After a *split* delivery no text can be recognised, so every content-returning
  command (snapshots, `get`, `eval`, `find`, captures) is withheld until a
  navigation command; act-only commands (click, press, type, scroll, wait,
  navigation) still run.
- CLI `fill`/`type`, `batch` page commands, `eval` scripts and arguments refuse
  a reference outright. The only CLI stdin that lowers one is
  `auth … --password-stdin`, and only for an unbound password; codes and bound
  passwords never ride the CLI.

Tests: `secure_prompt_fill_tests`, `browser::dispatch::tests`,
`trace_drain::tests::delivered_values_are_scrubbed_from_traces_by_value`.
