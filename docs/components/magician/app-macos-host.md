# Apps macOS host owner

The typed Apps macOS owner is the sealed, pairing-backed desktop adapter for
reviewed interactive actions. Apps do not call `macos-ui-automation::call`,
`/host/ax/<action>`, AppleScript, JXA or a shell. App input carries no raw
action name, arguments JSON, AX selector, PID, window ID, element index,
executable, environment, profile path or filesystem path.

## Actions and effect classes

The primitive catalog exposes eight Ready actions: `launch`, `focus`,
`snapshot`, `click`, `type`, `key`, `scroll`, `drag`, each
`AppPrimitiveDispatchStatus::Ready` with exact desktop-owner identity. Public
names map from `AppMacosOperation`
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

The runtime never infers the effect class from a live label. `type` rejects
empty, >16 KiB and NUL input. `click`, `type`, `key`, `scroll` and `drag` need
a fresh structured-tree observation from the same owner session; a
state-changing action invalidates earlier observations and their element
mappings.

**Pixel capture is fail-closed.** CuaDriver 0.28 has no standalone screenshot
tool (`get_window_state` returns window pixels inline, capture-only with
`include_accessibility_tree:false`; `get_desktop_state` captures whole
displays). Enabling it needs a reviewed bounded pixel-evidence result shape and
a current Screen Recording fence, or the internal operation is removed.

## Wire

The private server-to-desktop wire is `magician.app-macos-host-wire.v2`
(`APP_MACOS_HOST_WIRE_V2`); v1 permits do not verify. Element actions carry the
observed CUA `element_token` (`s<8 hex>:<index>`), not a bare index. `scroll`
is `direction` (`up|down|left|right`) plus `amount` (1–50 notches). `drag` is
pixel-only in CuaDriver, so the wire carries both element tokens plus the
screenshot scale, and the desktop computes window-local centres. Key names
follow CuaDriver: `backspace` → `delete`; `delete_forward` → `delete` + `fn`.

## Authority path

- `apps/interactive.rs` owns the reviewed grant, run-owned session, fresh
  observation, one-action permit, cancellation and settlement typestates.
- `apps/macos_host.rs` owns macOS target policy, protected-application refusal,
  application identity, typed CUA lowering and response materialization.
- The app effect kernel owns durable dispatch-start and result settlement.

Lowering happens only in the owner: it re-parses the canonical input, resolves
opaque element refs through a move-only observation map and builds the wire
action itself. The prepared action keeps the input digest and must match the
I/O permit before signing, so no caller can pair a permit for one input with
other PIDs, AX indices or text.

Just before provider I/O the adapter consumes the one-shot provider token and
signs one desktop request covering installation generation, run, grant,
policy, interactive descriptor, target/profile/implementation digests,
session, lease, effect binding, action, input, observation, ceilings,
bundle/code identity, TCC policy and epoch, and expiry. The permit lasts at
most 10 s; the desktop consumes its nonce once.

The owner recomputes the physical profile digest from the paired host
identity, endpoint digest, TCC policy digest and epoch, and the implementation
digest from the paired binary, so a reviewed descriptor cannot be reused with
another gateway, host, permission epoch or CUA build. The implementation digest
covers the runtime contracts, pairing, service and adapter source, desktop
gateway source, the Settings pairing UI and the CUA binary digest.

Pairing (three-stage handshake, rotation, revocation, crash recovery) is owned
by [Apps macOS pairing owner](app-macos-pairing.md); physical execution is
unavailable unless the owner-scoped pairing is `Active`.

## Desktop route and CUA binary

`/host/apps/macos/action` starts without a verifier and returns unavailable
until the pairing owner installs the key, exact CUA binary, profile and
implementation digests and the current host/TCC/application snapshot. It never
resolves its binary from env, `PATH`, `HOME` or a fallback, never uses a
generic launcher and never retries an effectful call.

- **Binary staging.** The pairing UI asks for
  `/Applications/CuaDriver.app/Contents/MacOS/cua-driver` (the
  `~/.local/bin/cua-driver` symlink is rejected). Native approval copies the
  whole signed `.app` bundle into a private `0700` directory
  (`cua-owner-v3-<content digest>`); symlinks or non-exact path components are
  rejected. The whole bundle is needed because AMFI honors the executable's
  restricted entitlements only beside the embedded provisioning profile.
- **Identity.** V3 identity binds every bundle file's bytes plus the staged
  executable's path and device/inode/mode/owner. Each call re-hashes it and
  execs the staged executable in place (macOS refuses exec via `/dev/fd`); the
  remaining window is a same-uid writer between re-hash and exec.
- **Upgrade.** Upgrading CuaDriver changes the digest and the pairing stops
  verifying: re-pair from desktop Settings after `make setup-cua-driver`.
- **Private daemon.** The typed owner never uses CuaDriver.app's shared daemon
  (the skill relay's). On first call the desktop spawns
  `cua-driver serve --embedded --no-overlay --socket <app-data>/cua-run/d.sock
  --pid-file <app-data>/cua-run/d.pid --host-bundle-id <desktop bundle id>
  --permission-mode standard` from the staged executable; every call passes
  `--socket`. `cua-run` is `0700`; a path over the 103-byte socket limit fails
  closed. `--embedded` puts the daemon in the desktop's responsibility chain,
  so it inherits the desktop's Accessibility and Screen Recording grants and
  never prompts; `check_permissions` (always `prompt:false`) reports the
  desktop's TCC state. Env is cleared (`LANG`/`LC_ALL=C`, telemetry off). Stdin
  is a lifetime pipe (`CUA_DRIVER_PARENT_LIVENESS_STDIN`), so the daemon dies
  with the desktop. `--permission-mode bounded` is not used because CuaDriver
  publishes no capability-manifest schema; the host's closed verb allowlist is
  the ceiling (`host_gateway.rs`).

**Action-time fences.** The desktop re-resolves the application from Launch
Services or the running PID and recomputes an identity over the bundle path,
`Info.plist`, `CodeResources` and executable bytes; it must equal the reviewed
pairing snapshot. After permit verification it rechecks the Accessibility and
Screen Recording policy digest and epoch and the bundle/PID/window before one
fixed CUA verb. Observe repeats the TCC and PID fences before the final read and
before releasing bytes; drift settles outcome-uncertain. All CUA output shares
one byte budget; expiry, cancellation, timeout, read failure or overflow kills
the child. Mutation responses are content-free; observations are parsed before
secure-field detection so JSON escaping cannot bypass policy.

**Transport.** Pairing-owned. V1 accepts only an exact HTTP URL on literal
`127.0.0.1` with an explicit port and the typed path; hostnames (including
`localhost`, `host.docker.internal`) are rejected so DNS or hosts rebinding
cannot see the setup key. No ambient gateway env, system proxies or redirects.
The endpoint digest must equal the pairing capability; the body is streamed
under the signed result ceiling.

**Listener limits.** At most 32 loopback connections, a 15 s whole-request read
deadline, ambiguous Content-Length/Transfer-Encoding rejected, route ceiling
chosen before body allocation (`host_gateway_route_body_ceiling`): 256 KiB for
`/host/apps/macos/*` and `/host/apps/android/*`, 40 MiB for the two speech
routes, 512 KiB for every other route. Response writes have a 15 s deadline.

**Cancellation** posts the same signed one-shot request to the stop-only
`/host/apps/macos/cancel`, matched only against the active action; it does not
use the global owner-stop route or cancel other sessions. Lost or late
cancellation stays outcome-uncertain.

The legacy `/host/ax/<action>` route remains only for non-App skill callers;
the jailed App path cannot reach it.

## Workflow vertical

The catalog projects the legacy macOS skill to eight Interactive Ready
descriptors with closed input schemas: `snapshot` is
`{ "operation": "observe" }`; `launch`/`focus` take only their operation const;
observation-bound leaves need an opaque `observation_ref` plus element refs
(and bounded `click_count`, `text`, key/modifiers, or scroll `direction` +
`amount` 1–50). Results use `magician.app-macos-result.v1`. Each dependency
selects exactly one Ready leaf.

The workflow accepts only the locked singleton selector, opens the pairing
store for the authenticated scope and uses only `active_snapshot`. Launch
requires exactly one reviewed target in the pairing, so package or model input
cannot pick another bundle. It recomputes the digests, acquires a one-step
MacosHost session and resolves PID/window only inside the desktop owner.

The effect kernel records dispatch-start before the provider is polled.
Completed results become one canonical `ActionResult`, checked against the
interactive receipt, and the completion intent and receipt are stored before
commit. Recovery reopens a fresh Active pairing and re-attests the locked
action and target before historical result bytes are labeled, so a changed or
revoked pairing cannot reattribute them.

## Failure and cancellation

Only the outer common-effect abort path can prove provider I/O never ran. Once
the owner holds an I/O permit, cancellation, timeout, disconnect, stale
identity, replay rejection, malformed response, bound violation, correlation
mismatch, persistence failure or identity drift is `outcome_uncertain`. The
owner emits a payload-minimal receipt bound to the same effect identity, and
the caller compares digests before committing.

### Observation projection

Raw AX and pixel evidence stays with the owner. It parses CuaDriver's
structured `elements[]` (never `tree_markdown`, whose `[N]` markers are
presentation only), requires each row's `element_token` to be minted by the
reply's own `snapshot_id` for that `element_index`, and keeps tokens and
screenshot scale in a private move-only projection. The result holds at most
8,192 sanitized nodes (opaque ref, AX role, safe label/title/value subset,
enabled/selected, depth). Editable roles lose all free text (some AX renderers
alias the value into label/title); a secure/password marker rejects the whole
observation. The result digest binds opaque refs and the TCC snapshot. PID,
window ID, element index, selectors and raw evidence never reach the public
result. Projection, a final cancellation fence and receipt completion run
synchronously inside the owner, so a caller cannot settle Completed after
cancellation.

### Element fences

Element addresses belong to the snapshot that minted them; every
`get_window_state` supersedes older tokens and a bare `element_index` is
refused (`snapshot_id_required`). Before an observed action the desktop
re-snapshots with `include_screenshot:false` (a fresh screenshot could exceed
the original ceiling) and checks the permit's `observation_content_digest`,
recomputed from the fresh `tree_markdown`:

- **Element action** (`app_macos_host_element_fence`): the target's own line
  (index, role, label, attributes) plus each ancestor's index and role, not
  ancestor labels; drag fences both endpoints.
- **Key press** (`app_macos_host_window_fence`): the `[0] AXWindow` row and the
  roles of its direct children, which catches a sheet or popover taking the
  key.
- The fence is deliberately not the whole window: macOS retitles windows after
  edits, and a whole-content fence would refuse unrelated actions.
- The menu bar CuaDriver appends is never fenced, so menu chrome is not an
  element-action target (use a menu path); absent, duplicated or menu targets
  are refused.

The desktop then rebinds the token's index to the fresh `snapshot_id`,
refusing an index the fresh `elements[]` lacks. Drag takes both centres from
fresh frames relative to the fresh `AXWindow` frame times the screenshot scale,
refusing virtualized `h:1` rows or points outside the window. The legacy
`/host/ax` relay never retries a stale address; it adds a re-snapshot hint.

Live owner round-trip test: `live_textedit_owner_round_trip`
(`host_gateway.rs`, `--ignored`).
