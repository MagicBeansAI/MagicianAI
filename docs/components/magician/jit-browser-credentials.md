# One-time browser credentials

Magician's built-in `browser__secure_prompt_fill` asks for credentials only when
an automation needs them. It uses Magician HITL and the in-process MagicVault
browser adapter. No standalone MagicVault MCP connection, daemon, vault unlock,
credential enrollment or MagicRun source change is required.

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

Magician resolves one main-frame target before prompting; an optional backend
`tab_id` picks between tabs on the same origin. Every value, username included,
uses hidden HITL fields in the authenticated UI; prompts show origin, tab and
selector. A final **Use once** approval authorizes the fill. **Cancel**,
dismissal, execution cancellation and the 180-second overall deadline discard
the pending material (`DEADLINE_SECS = 180`, `secure_prompt_fill.rs`). Filling
never submits the form.

## Custody and delivery

- `UserRequestService::ask_sensitive_once` installs a private oneshot receiver
  before publishing the request. Under the scoped first-response lock, submitted
  material moves into a zeroizing buffer, and the ordinary response is reduced to
  a fixed status before history, event publication or agent delivery.
- Only the trusted in-process operation owns the receiver. Cancellation closes it
  and retires the prompt; restored requests have no receiver and cannot accept
  credentials; orphan resolution discards secure input.
- Passwords are never enrolled or written to the vault. Request metadata and
  content-free decisions stay in history.
- The HTTP handler keeps no copy for response relays; confirmation input is
  discarded. Values move into MagicVault `MaterialField`, which zeroizes on drop.
  This shortens lifetimes; it does not claim that browsers, HTTP parsing, OS
  buffers or every allocator copy are erased.

The `magicvault-effect` adapter does a bounded isolated-world CDP fill, validates
every selector, and rechecks frame, document and exact origin before changing
the page. This operation skips CLI argument construction, progress output,
workflow/API replay and generic browser result/trace processing. It returns only
field states, fixed errors, `saved:false` and `submitted:false`. Partial or
uncertain outcomes are never retried automatically.

Runtime-owned effect identities are sealed in a bounded process-local set (4,096
attempts). A repeated decision or a full set refuses without prompting or
filling. A restart destroys all material and the set; another attempt needs
fresh UI input and consent. No persistent replay credential or reusable
plaintext reference is created.

## Supported boundary

- Operator-configured local CDP endpoint and main-frame HTML input elements
  only. Rejected: model-selected CDP endpoints, retrieval-handoff overrides,
  headed/headless sessions, iframes, unsupported input types, ambiguous targets.
- HTTPS required, except explicit loopback HTTP development sites. Browser
  transport ceilings and work confinement checks still run before dispatch.
- The website receives the values; this does not stop site scripts or a later,
  independent browser inspection tool from reading them. Generic
  `need_user_input`, saved-secret substitution, network capture outside this
  operation, OTP retrieval and all-channel secure input are separate surfaces,
  not certified by this flow. Browser credential requests should use this
  action.
- The adapter requires websocket payload logging off: Magician enables `log`'s
  `max_level_off` and `release_max_level_off` features (`tracing`
  instrumentation is unaffected; `log`-facade-only events are disabled).
- The local Magicutor proxy supports a reversible scoped endpoint alias, so IDs
  with underscores and transport-ceiling suffixes reach the same owned tabs.

## MagicVault dependency mapping

Cargo resolves these libraries from the public
`https://github.com/MagicBeansAI/MagicVault.git` repository, at exact revisions
recorded in the workspace manifest and lockfile:

| Library | Version | Revision |
| --- | --- | --- |
| `magicvault-effect` | 0.7.1 | `4902803b35211ea2a1dc7e6bdb112cb9da03000f` |
| `magicvault-protocol` | 0.7.0 | `4902803b35211ea2a1dc7e6bdb112cb9da03000f` |
| `magicvault-core` | 0.1.6 | `4902803b35211ea2a1dc7e6bdb112cb9da03000f` |
| `magicvault-primitives` | 0.1.2 | `4902803b35211ea2a1dc7e6bdb112cb9da03000f` |

All four pin one MagicVault revision in the root `Cargo.toml`, with no `[patch]`
and no machine-specific path dependencies or runtime connections to another
MagicVault checkout. Magician owns its HITL/material channel and calls the
adapter directly, so it needs the published adapter contract, not the standalone
MagicVault broker/MCP wire protocol.

MagicRun `tool-runtime-core` is at 0.1.82, commit
`e9a4fdc4d028c5d3e0d2041e4a55bff26edc1bd4`; the lockfile's two source spellings
resolve to it. Use locked Cargo resolution when verifying this mapping.

Tests: `make cargo ARGS="test -p magician --lib jit_"` (in
`secure_prompt_fill_tests.rs`); UI `ui/unified-ui/src/lib/hitl/respondToHitl.test.ts`,
`ui/unified-ui/src/lib/hitl/promptFor.test.ts`.

## Relation to the shared sensitivity contract

The one-time browser prompt is one producer of the shared contract in
[HITL / Attention](hitl-attention.md#sensitivity-contract-secure-hitl-credentials-p1):
`ask_sensitive_once` requests are classified `password`, one-time, `producer` at
acceptance and keep their private oneshot receiver. Every other sensitive answer
goes to the service's custody and is taken by reference.

## One-time custody in the shared core

`magicvault-core` (0.1.4 onward; 0.1.6 pinned) provides the custody the OTP path
needs; re-exports live in `magician_v2::secrets`:

- an injectable store clock (`CustodyClock`; `SecretStoreResolver::with_clock`,
  `ManualClock` under `test-fixtures`). Every deadline is an absolute timestamp
  checked at the moment of the operation;
- bounded ephemeral entries (`register_ephemeral_bounded`) that no reader or
  placeholder resolves at or past their deadline;
- one-time material: `register_one_time` (scope, challenge id, expected
  destination, deadline clamped to `ONE_TIME_MAX_RETENTION_MS` = 10 min) →
  `reserve_one_time` for exactly one bound claim → `consume_one_time` when
  submission starts. `release_one_time(PreDispatchFailure)` is the only way
  back. Also `cancel_one_time`, lazy and swept expiry, value-free
  `OneTimeReceipt`s (core `0.1.5`+ can name the bound destination) and
  `one_time_*` audit lines.

Nothing persists: a restart loses the code and the run fails closed to a fresh
ask. One-time material is never a placeholder read and retires with
`clear_ephemeral`. `otp` inputs register through `register_one_time`, passwords
through the bounded variant at the resume site. Contract test:
`magician/tests/magicvault_facade.rs::p0_a_one_time_code_cannot_be_used_twice_from_ephemeral_custody`.

## Referenced material through the typed fill

`browser__secure_prompt_fill` is the browser lane's only sink for material the
run already holds. A field may carry `value: "[REF:<key>]"`: exactly one
reference, never a typed value (`invalid_request`), naming a password or
one-time code the user gave through `need_user_input`.

1. Each reference resolves from the run's ephemeral scope before any prompt.
   Destination binding is checked first: material bound to another origin
   refuses (`material_unavailable`, `bound to <origin>`) before anything is
   asked or filled.
2. A password lowers by a plain scoped read. A code lowers by `reserve_one_time`
   with a claim naming this exact origin (`scheme://host[:port]`, as in the HTTP
   lane and challenges) and the challenge still standing there; a code bound
   elsewhere, expired, consumed or collected for an earlier challenge refuses
   with the store's reason. Fields without a value are prompted.
3. **Use once** is asked unless every field is a code bound to exactly this
   origin; the challenge that named the destination already carried consent.
4. Right before consuming, the fill rechecks the page (same single target,
   origin, document). A changed page, declined confirmation, ambiguous target,
   missing sibling material, cancel or deadline releases every reservation
   (`Referenced` releases what it holds on drop).
5. Codes are consumed as the fill starts. A selector the adapter cannot resolve
   after that spends the code: the accepted residual, narrowed by the recheck.

Several fields naming one reference are a segmented input (one box per
character): resolved, reserved, split in field order and spent once; the model
never holds a per-digit reference. A field count the material does not split
into is `invalid_request` (naming the count, never the length) before anything
is asked or spent.

Leak controls after delivery:

- Delivered values join the run's scrub set (keyed per delivery) and are
  scrubbed from traces by value.
- Image captures through the browser dispatcher are withheld until a navigation
  command, or a text observation without a delivered value, shows the page moved
  on. A click proves nothing.
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
