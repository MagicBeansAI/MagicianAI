# Apps macOS host owner

The typed Apps macOS owner is the sealed pairing-backed desktop adapter for
reviewed interactive actions. Apps do not call `macos-ui-automation::call`,
`/host/ax/<action>`, AppleScript, JXA or a generic shell. There is no raw
action name, arguments JSON, AX selector, PID, window ID, element index,
executable, environment, profile path or filesystem path in app input.

## Actions and effect classes

The primitive catalog exposes eight Ready actions — `launch`, `focus`,
`snapshot`, `click`, `type`, `key`, `scroll`, `drag` — each
`AppPrimitiveDispatchStatus::Ready` as a sealed paired macOS action with exact
desktop-owner identity. Public names map from `AppMacosOperation`
(Launch/Focus/Observe/ClickElement/TypeText/PressKey/ScrollElement/DragElements).
`CapturePixels` has no public name and is not catalog-granted.

Desktop lowering targets CuaDriver 0.28: `launch_app`, `bring_to_front {pid}`
(focus), `get_window_state` (observe), `click` / `double_click`, `type_text`,
`press_key`, `scroll`, `drag`, each checked against the tool's
`additionalProperties:false` schema.

| Action | Effect class | Why |
| --- | --- | --- |
| `click`, `key`, `drag` | `OutwardCommit` | an AX label cannot prove a control will not send, submit, delete or move durable state; needs reviewed action + outward-commit permit/confirmation |
| `type`, `scroll` | `Interact` | |
| `launch`, `focus` | `NavigateOrLaunch` | |
| `snapshot` | `Observe` | |

The runtime never infers the effect class from a live UI label. `type` rejects
empty, >16 KiB and NUL input. Observation-bound actions (`click`, `type`, `key`,
`scroll`, `drag`) consume a fresh structured-tree observation from the same
owner session; state-changing actions invalidate earlier observation
generations and their element mappings.

**Pixel capture is fail-closed.** CuaDriver 0.28 has no standalone screenshot
tool (`get_window_state` returns window pixels inline, capture-only with
`include_accessibility_tree:false`; `get_desktop_state` captures whole
displays). Activation needs a reviewed bounded pixel-evidence result shape plus
a current Screen Recording fence, or removal of the internal operation.

## Wire

The private server-to-desktop wire is `magician.app-macos-host-wire.v2`
(`APP_MACOS_HOST_WIRE_V2`); v1 permits do not verify. Element-addressed actions
carry the observed CUA `element_token` (`s<8 hex>:<index>`), not a bare index.
`scroll` is `direction` (`up|down|left|right`) plus `amount` (1–50 notches).
`drag` is pixel-only in CuaDriver, so the wire carries both element tokens plus
the observation's screenshot scale and the desktop computes window-local pixel
centres. Key names follow CuaDriver: `backspace` → `delete`; `delete_forward` →
`delete` + `fn`.

## Authority path

- `apps/interactive.rs` owns the common reviewed grant, run-owned session,
  fresh observation, one-action permit, cancellation and settlement typestates.
- `apps/macos_host.rs` owns macOS target policy, protected-application refusal,
  exact application/physical identity, typed CUA lowering and host response
  materialization.
- The app effect kernel owns durable dispatch-start and canonical
  resource/result settlement.

Lowering is owner-only: it re-parses the canonical input, resolves opaque
element refs through a move-only owner-held observation map, and builds the
physical wire action internally. The prepared action retains the canonical input
digest/length and must match the common I/O permit before signing, so no caller
can pair a permit for one logical input with different PID/window/AX indices or
text/key parameters.

Immediately before provider I/O the adapter consumes the one-shot provider token
and signs one desktop request covering installation generation, run, grant
revision/digest, current policy, interactive grant descriptor,
target/profile/implementation digests, session, resource lease, effect binding,
action, input, observation, result/evidence ceilings, bundle/code identity,
TCC policy/epoch and expiry. The permit lasts at most 10 s; the desktop consumes
its nonce once.

Owner construction recomputes the grant's physical profile digest from the
paired host identity, typed endpoint digest, TCC policy digest and epoch, and
the implementation digest from the paired binary. A reviewed descriptor
therefore cannot be reused with a different gateway, host, permission epoch or
CUA build. The implementation digest frames every load-bearing runtime
contract, pairing/service/adapter source, desktop identity/pairing/gateway
source and trusted Settings pairing UI/client source, plus the CUA binary digest.

Pairing (three-stage handshake, rotation, revocation, crash recovery) is owned
by [Apps macOS pairing owner](app-macos-pairing.md); physical execution is
unavailable unless the exact owner-scoped pairing is `Active`.

## Desktop route and CUA binary

`/host/apps/macos/action` starts without a verifier and returns unavailable until
the pairing owner installs the key, exact CUA binary, physical
profile/implementation digests and current host/TCC/application snapshot. It
never resolves its binary from env, `PATH`, `HOME` or a fallback location, never
uses a generic launcher, and never retries an effectful call.

- **Binary staging.** The pairing UI asks for
  `/Applications/CuaDriver.app/Contents/MacOS/cua-driver` (the
  `~/.local/bin/cua-driver` symlink is rejected). Native approval copies the
  whole signed `.app` bundle into a private `0700` owner directory
  (`cua-owner-v3-<content digest>`) via create-new rename + fsync; symlinks in
  the bundle or non-exact path components are rejected. Why the whole bundle:
  the executable's restricted entitlements are honoured by AMFI only beside the
  embedded provisioning profile; a lone copy is killed at exec.
- **Identity.** V3 identity binds every bundle file's bytes plus the staged
  executable's canonical path and device/inode/mode/owner. Each typed call
  re-hashes it and execs the staged executable in place (macOS refuses exec via
  `/dev/fd`); the residual window is a same-uid writer between re-hash and exec.
  Verifier installation hashes on a bounded blocking pool before taking the host
  mutex, then publishes only if setup/generation/key/path/profile/implementation
  are still exact.
- **Upgrade.** Upgrading CuaDriver changes the digest and the pairing stops
  verifying: re-pair from desktop Settings after `make setup-cua-driver`.
- **Private daemon.** The typed owner never uses CuaDriver.app's shared daemon
  (the skill relay's). On first call the desktop spawns
  `cua-driver serve --embedded --no-overlay --socket <app-data>/cua-run/d.sock
  --pid-file <app-data>/cua-run/d.pid --host-bundle-id <desktop bundle id>
  --permission-mode standard` from the staged executable; every call passes
  `--socket`. `cua-run` is `0700`; a path over the 103-byte socket limit fails
  closed. `--embedded` puts the daemon in the desktop's responsibility chain, so
  it inherits the desktop's Accessibility/Screen Recording grants and never
  prompts; `check_permissions` (always `prompt:false`) reports the desktop's own
  TCC state. Env is cleared (`LANG`/`LC_ALL=C`, telemetry off). Stdin is a
  lifetime pipe (`CUA_DRIVER_PARENT_LIVENESS_STDIN`), so the daemon dies with
  the desktop; a daemon from another approved digest is replaced.
  `--permission-mode bounded` is not used because CuaDriver publishes no
  capability-manifest schema; the host's closed verb allowlist is the enforced
  ceiling (`host_gateway.rs`).

**Action-time fences.** The desktop re-resolves the application from Launch
Services or the exact running PID and recomputes a domain-separated identity
over the canonical bundle path, `Info.plist`, `CodeResources` and bounded
executable bytes; it must equal the reviewed pairing snapshot for launch and for
existing-process actions. After permit verification it rechecks the current
Accessibility/Screen Recording policy digest and epoch and the
bundle/PID/window before invoking one fixed CUA verb. For Observe it repeats the
TCC and PID-identity fences immediately before the final read and again before
releasing result bytes; mid-flight drift is an owner error settled
outcome-uncertain. All CUA preflight and action stdout/stderr share one byte
budget; expiry, cancellation, timeout, read failure or overflow kills the child.
Mutation responses are content-free; observations are parsed before
secure/password-field detection so JSON escaping cannot bypass policy.

**Transport.** Pairing-owned. V1 accepts only an exact HTTP URL on literal
`127.0.0.1` with explicit port and the typed path; hostnames (including
`localhost`, `host.docker.internal`) are rejected so DNS/hosts rebinding cannot
see the setup key. No ambient gateway env, system proxies or redirects. Endpoint
digest must equal the pairing capability; connect/body are async, cancellable
and streamed under the signed result-plus-evidence ceiling.

**Listener limits.** At most 32 loopback connections, 15 s whole-request read
deadline, ambiguous Content-Length/Transfer-Encoding rejected, route ceiling
chosen before body allocation. Typed Apps routes cap at 256 KiB (only reviewed
speech routes keep a larger ceiling). Response writes are deadline-bounded.

**Cancellation** posts the same signed one-shot request to the stop-only
`/host/apps/macos/cancel`, matched only against the currently active action; it
does not reuse the global owner-stop route or cancel other sessions. Lost or
late cancellation stays outcome-uncertain.

The legacy `/host/ax/<action>` route remains only for existing non-App skill
callers; the jailed App path cannot reach it.

## Workflow vertical

The catalog projects the legacy macOS skill to eight Interactive Ready
descriptors with closed input schemas: `snapshot` is `{ "operation": "observe" }`;
`launch`/`focus` take only their operation const; observation-bound leaves
require an opaque `observation_ref` plus element refs (and bounded
`click_count`, `text`, key/modifiers, or scroll `direction` + `amount` 1–50).
Results use `magician.app-macos-result.v1`. Each dependency selects exactly one
Ready leaf.

The workflow accepts only the locked singleton selector, opens the durable
pairing store for the authenticated scope, and consumes only `active_snapshot`.
Launch requires the pairing to contain exactly one reviewed target, so
package/model input cannot choose another bundle. It recomputes target policy,
owner profile and implementation digests, acquires a one-step MacosHost
session, and resolves PID/window only inside the desktop physical owner.

The effect kernel durably records dispatch-start before the provider is polled.
Completed results are projected to one canonical `ActionResult`, preflighted
against the interactive receipt, and the completion intent and receipt are
stored before commit. Recovery reopens a fresh Active pairing and re-attests the
locked primitive/action/physical target before historical result bytes can be
labeled, so a changed or revoked pairing cannot reattribute them.

## Failure and cancellation

Only the outer common-effect abort path can prove provider I/O was never polled.
Once the owner holds an I/O permit, cancellation, timeout, disconnect, stale host
identity, replay rejection, malformed response, bound violation, correlation
mismatch, result persistence failure or post-start identity drift is
`outcome_uncertain`. The owner emits a payload-minimal interactive receipt bound
to the same effect identity; the caller compares result/evidence digests before
committing.

### Observation projection

Raw AX/pixel evidence stays with the physical owner. The owner parses
CuaDriver's structured `elements[]` (never `tree_markdown`, whose `[N]` markers
are presentation only), requires every row's `element_token` to be minted by the
reply's own `snapshot_id` for that row's `element_index`, derives random opaque
element refs, and keeps tokens and screenshot scale in a move-only private
projection. The result holds at most 8,192 sanitized nodes (opaque ref, bounded
AX role, safe label/title/value subset, enabled/selected, bounded depth). All
free text is omitted for editable roles (some AX renderers alias the value into
label/title); secure/password markers reject the whole observation. The
semantic-labels digest covers sanitized nodes; the result/receipt digest also
binds opaque refs and the TCC snapshot. PID, window ID, element index, selectors
and raw evidence never enter the public result. Projection, a final cancellation
fence and receipt completion run synchronously inside the owner, so a caller
cannot delay materialization and then settle Completed after cancellation.

### Element fences

Element addresses are scoped to the snapshot that minted them; every
`get_window_state` supersedes older tokens and a bare `element_index` is refused
(`snapshot_id_required`). Before an observed action the desktop re-snapshots
with `include_screenshot:false` (a fresh screenshot could exceed the ceiling
derived from the original evidence) and checks the permit's
`observation_content_digest`, recomputed from the fresh `tree_markdown` by shared
contract functions:

- **Element action** (`app_macos_host_element_fence`): the target's own line
  exactly (index, role, label, attributes) plus each ancestor's index and role,
  not ancestor labels; drag fences both endpoints.
- **Key press** (`app_macos_host_window_fence`): the `[0] AXWindow` row and the
  roles of its direct children, catching a sheet or popover taking the key;
  process and window-id fences already prove the same window.
- The fence is deliberately not the whole window: macOS retitles windows on its
  own after edits, and a whole-content fence would refuse unrelated actions.
- The menu bar CuaDriver appends is never fenced (menus self-update), so menu
  chrome and anything under it are not element-action targets (use a menu path);
  runtime and desktop both refuse absent, duplicated or menu targets.

The desktop then rebinds the token's index to the fresh `snapshot_id`, refusing
an index the fresh `elements[]` lacks. Drag takes both centres from fresh frames
relative to the fresh `AXWindow` frame times the screenshot scale, refusing
virtualized `h:1` rows or points outside the window. The legacy `/host/ax`
relay never retries a stale address; it appends a re-snapshot hint.

Live owner round-trip test: `live_textedit_owner_round_trip` (`host_gateway.rs`,
`--ignored`).
