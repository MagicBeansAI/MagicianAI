# Desktop App (Magician Desktop)

Version 0.3.23 connects source-checkout installation, bounded build progress and data-root-aware native readiness checks.

**Current development version:** `0.3.23`. The tray app presents as
**Magican** / **Magican Desktop** (`ai.magicbeans.magican.desktop`).
The docked Orb is a side notch on the right screen edge: Ask-user questions and the voice transcript open as drawers there.
Application Support stays `dev.magician.desktop`. Magican and legacy Magdroid
packages cannot be observation targets.

Tauri v2 native menu-bar/tray application. macOS places the control menu in the
top menu bar; Windows in the notification area (left click opens Magican, right
click the menu); Linux in the desktop environment's status-notifier area.
Periodic window/monitor work uses an Objective-C autorelease pool only on
macOS; Linux and Windows call the same helper without AppKit dependencies.
Windows and Linux use the full-colour app icon rather than the monochrome macOS
template mask so it stays visible on dark panels. Ships `.dmg` (macOS),
`.AppImage` (Linux) and NSIS `.exe` (Windows), and registers and routes only the
`magican://` custom scheme.

macOS source packages use the same debug-signing helper as Desktop, retaining
the runtime code identifier and matching Apple Team ID when a certificate is
available. An ad-hoc fallback runs the core backend; protected native owner
pairing remains unavailable without an Apple signing identity.

The standard non-macOS deployment runs Magician, Magicutor and the supervisor
from the Linux OCI image, locally or remotely; the native Linux/Windows Desktop
stays on the user's computer as its CUA, browser and platform-tool edge. Linux
can manage a local Docker backend or select a remote engine; Windows can manage
the image through Docker Desktop, connect to an existing local container, or
select a remote engine. Native Windows backend executables and Task Scheduler
support are a future/development path; the standard NSIS installer does not
bundle them.

Sibling pages: [Mac Notch Orb](mac-notch-orb.md) ·
[HUD overlay window](hud-overlay-window.md) ·
[Contextual assist](contextual-assist.md) ·
[Notification overlay](notification-overlay.md) ·
[Custom-surface origin policy](custom-surfaces-v1-origin-policy.md) ·
[Install manifest and cleanup](install-manifest-cleanup.md) ·
[Runtime config seeding](runtime-config-seeding.md).

## Guided first-run setup

Optional Cloudflare remote access and Microsoft Clarity session recording use
the shared capability catalog. Identifiers use plain fields, credentials remain
masked, and saves update both private runtime env files. Clarity recording is
off without explicit opt-in and applies on page reload without rebuilding.
When the tunnel step is already installed, Desktop shows the catalog's domain
switch. Save the new zone on the Cloudflare account step first, then run the
switch. The shared installer clears hostnames from the previous zone and
rebuilds this computer's tunnel and the original shared tunnel. Old DNS records
and Access apps stay until each hostname is parked.

A fresh install opens a placement chooser before it starts or downloads any
backend; it is reachable later as **Change setup**. The same four explicit
placements appear on every OS (the app never infers intent from a port or URL):

- **Install native services here** — enabled on macOS when a checksummed
  backend package or a source checkout is available (disabled as a future option on Linux/Windows).
  It seeds the chosen data root without replacing files and registers the
  supervisor as a per-user LaunchAgent.
  Source setup uses the same `install-source-checkout.sh` preparation as the
  terminal wizard: required software, local memory embedder, debug build and
  checked native package. Set `MAGICIAN_SOURCE_DIR` for a checkout different
  from the one used to build Desktop. The reviewed plan names the source build;
  progress points to `<data root>/logs/source-install.log`. It waits for
  Magician, Magicutor, the memory model and this app's host gateway before
  opening capability onboarding. Build or readiness failures leave setup
  incomplete and preserve the chosen data folder. Re-running reconciles the
  LaunchAgent with the reviewed root and updated binaries/configuration.
  The LaunchAgent passes the supervisor `--magician-config` and, from a stable
  bundle, `--desktop-app`/`--desktop-log`, so it relaunches the app after a
  crash, never after Quit (`supervisor.manage_desktop`). At login it starts
  the app only if "launch at login" is on and the app has not appeared within
  30 s. Each app launch updates those plist arguments without restarting the
  backend; see the
  [supervisor guide](../magic-supervisor/supervisor.md#desktop-app-macos).
  The read-only, loopback-only `/host/permissions/status` route reports key/state
  pairs from the signed app's permission registry for terminal setup. It does
  not request grants, return account details or replace native permission checks.
- **Install and manage a local container** — Apple Container on Apple Silicon
  with macOS 26+, Docker via Colima on other Macs (Magician needs Apple Silicon
  and macOS 14+); the choice comes from the real OS version and CPU. Desktop
  installs/starts the macOS runtime; a missing Homebrew installs as the
  signed-in user with a native password dialog only for its privileged
  operations. Colima starts with 2 CPUs / 4 GiB and its freshly installed binaries
  are resolved outside the interactive shell `PATH`. Linux and Windows require a
  usable Docker first; Linux also needs host `python3` for the credential-custody
  preflight (distro-neutral guidance, no silent host changes). The reviewed plan
  pulls the image, creates private persistent credential custody outside the data
  folder, seeds missing runtime files, starts the container and verifies health.
  The data folder becomes `/data`; configuration changes name any required
  container replacement before approval.
  Magicutor (3003) is published on host loopback only, with
  `MAGICUTOR_TRUST_GATEWAY=1` so it trusts the bridge gateway; see
  [Magicutor trust boundary](../magicutor/README.md).
- **Connect to a local container** — loopback HTTP only, verifies `/health`, and
  takes no lifecycle ownership.
- **Connect to a remote container** — HTTPS only, verified before the saved origin
  changes. When Cloudflare Access is the outer gate, setup opens the server's
  browser Settings, where the authenticated owner creates a one-time,
  origin-bound **Desktop Edge** link that the `magican://connect` handler
  exchanges before sign-in. Backend data stays remote; CUA, the browser extension
  and platform capabilities stay on this computer.

Setup persists the same `MagicianDesktopConfig` fields used by startup and
Settings (no parallel connection file). Provider secrets stay in the runtime
env/Settings flow; user sessions and Desktop Edge credentials stay in the OS
credential store; remote engines keep rejecting engine-owned local file access.
For an Access-protected origin, the exchange also returns a narrowly scoped outer
service credential that native code attaches to exact-origin API and WebSocket
requests; WebView JavaScript never receives it. Native API responses are streamed
under a fixed size limit; sign-in verifies the issued bearer before saving it,
and sign-out clears local authority before its best-effort server request.

### Sign-in and capability onboarding

Unified-UI native windows (including the docked Attention drawer) sign in through
the existing native credential owner, which binds the request to the selected
server and verifies the new session before saving it. Sign-in does not require
reading the previous bearer first, so a damaged prior session cannot prevent
recovery. The requested workspace is preserved. Ordinary browser sign-in keeps
its scoped web transport; no native credentials cross into an iframe.
The shared UI invokes the `magician-session` bridge, whose three explicit
permissions cover session hydration, revision-fenced credential rotation and
sign-in. Native Attention windows receive these permissions only for their
reviewed UI origin and window label; each command still verifies the selected
server binding. This avoids granting remote content the desktop's other local
commands. Native command errors retain their message on the sign-in form.
The separate `magician-attention` bridge admits same-application review paths
through the existing native application router, so a full Work or transcript
review opens in the browser rather than replacing the docked inbox.

After backend health, Desktop verifies its origin-bound session (a valid stored
session continues; otherwise the Settings password door appears). On an empty
server with `auth.allow_signup: true`, the first login creates the owner and
adopts the existing `anonymous/default` data. Only then does capability
onboarding open, reading the component graph and Skillshub catalog **from the
selected engine** — a desktop connected to a remote server sees the server's
packages, probes and login requirements.

- Order: capabilities → requirements → installed skills and credentials →
  external-harness detection and Chat/run selection. **Finish setup** stays
  disabled while any selected component or required skill is unready or a
  selected harness is unavailable. Requirements use the same planner as
  `magician-setup`.
- Capability onboarding fits the window: navigation stays in a footer, with the
  primary action at the bottom right, while the capability, requirement, skill
  and harness lists scroll inside the panel. The footer has a compact bottom
  inset. The selected step uses the theme's main text color with an accent border
  and tinted background; the section label uses muted text, keeping both
  readable in dark and light themes. Headings and guidance use the generated
  Magican product identity, while service and API identifiers remain stable.
- Privacy-sensitive processing needs an explicit local-vs-remote choice. Local
  mode offers the engine's three local-generation catalog models and adds the
  chosen model's install step; remote mode omits it. Both require the local PPLX
  memory embedder. Requirements show an already-running embedder as available
  and distinguish the processing-choice confirmation from installation. A
  verified installed model offers rechecking rather than another download.
- Browser extension: Desktop copies its bundled unpacked extension to a versioned
  `Magican Browser Extension` folder in Downloads, opens `chrome://extensions`
  and explains **Load unpacked**; the step unblocks only when the desktop-local
  Magicutor reports a connected extension.
- Machine-verifiable probes and account pairing cannot be manually bypassed. The
  component graph and Skillshub manifests select a typed setup driver from the
  shared YAML catalog; Desktop renders its fields, executes declared bot start
  actions, and opens only catalog-allowed HTTPS login hosts. File prerequisites
  (e.g. a Google OAuth Desktop-client JSON) upload to the engine through a
  setup-token-gated endpoint; secrets are never returned.
- Once a managed Google account connects, the pairing panel checks its Cloud
  project roles (`onboarding_google_project_access` →
  `auth/browser-signin/bot/project-access`, session bearer) and offers
  **Grant access** or copyable commands; a 400 `bot_method_unsupported`
  shows nothing.
- **Working now** combines the engine's software probes with fresh desktop-owned
  permissions, CuaDriver readiness, extension connection and managed account
  pairing. The same observations feed the selection plan before dependency
  resolution. Native grants are checked by the signed app that uses them;
  refreshing the list never requests permission or captures the screen.
  It starts an already-installed CuaDriver service on demand through the same
  bounded, serialized lifecycle as computer-use calls, then reads its grants
  with `prompt: false`. A stopped daemon does not force repeat installation;
  startup/probe failures remain unverified.
- Governed remote-MCP skills: Desktop asks the engine to begin the scoped OAuth
  binding and opens the authorization page locally; callback state and tokens stay
  in the engine vault (remote engines use their public origin for the callback).
- The Skillshub inventory shows version, required programs, secret names,
  OAuth/CLI-profile/native-permission setup and installed scopes; install/remove
  act on the request's exact workspace scope. Component installers run only the
  graph-named script, one at a time, with bounded live output and a re-probe — a
  zero exit code alone never marks a requirement ready.
- `conditional`/`optional` skill auth stays visible but does not block Finish
  until selected; `required`/`at_least_one` does.
- **Control desktop apps** is selected by default: a bounded desktop-local
  installer installs exactly the catalog-pinned CuaDriver (0.28.2) from
  SHA-256-checked, tag-pinned scripts, replaces other versions, starts it in the
  graphical session, and verifies native access (macOS: `cua-driver permissions
  grant`, then Privacy panes). It never installs the GUI driver in a backend
  container. See [CUA setup](../scripts/cua-setup.md).
- **Harnesses** reads the server's `/coding/models` engine readiness list and
  `/plane/engines` roster (Codex, Claude Code, Grok Build, Antigravity) in catalog
  order. Desktop never installs or signs into them; only server-reported Ready
  harnesses can be selected for Chat or runs. Scheduled work follows the run
  engine; one-shot background model work stays under Settings → Model routing.
  Readiness is separate from model descriptors and contains no saved profile
  aliases. The managed-container acceptance fixture derives its local-processing
  assignment from the seed and pins only the query-analysis selector.
- Credential writes use the engine's atomic write-only APIs with its setup token,
  stored per exact origin in the OS credential store (captured automatically over
  loopback; copied manually for remote administration). Secret-backed skills are
  rechecked by key name only; managed CLI profiles by the supervisor's live auth
  status; providers with no verifiable login stay hard-blocked until configured
  or removed.

Completion is recorded against the exact engine origin and workspace only after a
fresh component plan and installed-skill check pass; closing early, changing
origin or restarting setup leaves onboarding pending. **Manage capabilities**
reopens the flow behind the same session gate. The Orb is created hidden and stays
gated through placement, installation, sign-in and required onboarding; finishing
reapplies the persisted Orb and wake settings.
While onboarding is pending, holding the overlay modifier or choosing an
explicit Orb summon reopens capability setup instead of silently ignoring the
gesture. It does not open the microphone or bypass readiness checks.
Catalog refreshes preserve completion in the same engine and workspace and do
not create a pending record merely by reading a legacy installation. Closing
capability management restores the persisted Orb settings when setup remains
complete; changing the engine or workspace still invalidates completion.
Pending setup now requires a persisted explicit-start marker. Older incomplete
records lack that provenance because catalog reads could create them, so they
retain legacy startup behavior on an already configured installation. Choosing
a placement, restarting onboarding, changing capabilities or authority scope
records an explicit attempt and keeps the Orb gated until validation succeeds.
This migration preserves the incomplete record; it does not mark requirements
as verified. Authentication and native permission checks remain in effect.

Host prerequisites: the Linux OCI image carries pinned Skillshub Node and Python,
so managed containers need no host Node. macOS installs Python 3 when absent
(credential custody runs on the host); a native macOS backend also bootstraps
Homebrew, Node.js/npm and `uv`. Ordered profiles and formulae live in
`magician-components/src/setup_catalog.yaml`; Desktop implements only bounded
Homebrew installer primitives. A missing required program is a hard stop.

### Packaging notes

- Native-service packages are staged with `make stage-desktop-native-backend` and
  covered provider-free by `make test-native-package`. They carry the pinned
  ripgrep the `grep` and `glob` tools run (`rg.bin`, copied by the installer with
  the other `*.bin`; `rg.exe` on Windows, copied by `install_windows_package` and the package's own `install.ps1`). Standard macOS release
  builds embed that package, and every backend executable and the app must pass
  Gatekeeper. `make release-desktop-native` builds, stages, requires and
  re-verifies it inside the finished bundle. Linux AppImage and Windows NSIS are
  Desktop Edge apps connecting to the Linux OCI runtime.
- Windows NSIS artifacts are unsigned (publisher/reputation warnings until an
  Authenticode certificate exists); Tauri updater signatures protect update
  integrity but do not replace Authenticode.
- Linux window input transparency applies after GTK realizes a window: hidden orb
  and draw-overlay windows set focusability at construction and click-through after
  first show. Package acceptance runs the `.deb` payload and AppImage in a GTK
  session (Xvfb is fine) with `MAGICIAN_DESKTOP_MANAGE_RUNTIME=0`.
- The gateway resolves Windows `.exe` install paths and reports `cua_available`
  separately from native Mac automation; iMessage and typed macOS app observation
  are absent from Linux/Windows builds.
- Startup/status text and Settings use generated product names from
  `data/presentation_identity.json` (compiled fallback before the backend starts);
  static OS metadata and compatibility paths never derive from a mutable name.
  `make generate-magican-app-icons` renders Tauri app icons. The checked-in
  menu-bar star mask is 36 pixels for an 18-point HiDPI item; its regression
  checks black template channels, opaque mark pixels and transparent corners.
  Raw `magician-desktop.bin` assigns its PNG to `NSApplication`;
  packaged macOS builds use `icon.icns`.

## Engine placement and signed-in scope

Remote setup requires the operator’s HTTPS origin; it never preselects an
operator deployment hostname. Connection examples use `example.com`.

Engine HTTP/WebSocket traffic uses `engine_base_url` (`None` derives
`http://127.0.0.1:{magician_port}`), shown as **Network → Backend URL**. It accepts
an origin only; remote origins require HTTPS, HTTP is loopback-only.
`is_remote_engine()` skips local container supervision and refuses engine-owned
`MAGICIAN_ROOT_DIR` reads even if a stale preference asks to manage a container.
Wake-word models and Application Support stay device-local. Copy says "backend"
or "server" for placement and keeps "Magician" for explicit service naming.

**Local container:** set `general.container_name`/`container_image`, published
`network.magician_port`/`magicutor_port`, the HTTP origin in
`network.engine_base_url`/`host_gateway.ui_url`, and
`general.manage_runtime_stack` (the debug launcher also needs
`DESKTOP_MANAGE_RUNTIME=1`; default 0 for native `run-all`). Magician/Magicutor
stop/restart run `/app/magic-supervisor client` in the container; for older
images whose client times out early, Desktop polls supervisor status up to 30 s
for a stopped service or changed PID and never repeats the mutation. Supervisor
controls preserve the container's writable layer, mounts, image and resources;
only explicit Settings apply/recreate and image-update flows replace it.
Managed-container acceptance uses the shipped GPT-6 Luna/Sol profile names; the
GPT-5.6 profiles are rollback choices.

**Custody:** managed creation embeds the shared Python 3 keyring provisioner on
macOS and Linux; Windows provisions equivalent per-user custody under Local
AppData restricted to the user and SYSTEM (the setup lock inherits the directory
ACL; state and credential files get explicit ACLs). Update/recreation validates
custody and actual mounts before stopping a service; first use of an existing
macOS runtime migrates and verifies Keychain entries into the Linux keyring.
Contract: [container credential provisioning](../scripts/container-runtime.md#persistent-device-pairing-on-headless-linux)
(`make test-desktop-managed-container`).

**External engine:** an unmanaged non-default origin is **External engine (URL)**
with native supervisor actions disabled; default 3002/3003 native mode is **Local
supervisor**. For a remote origin, host automation flows over an outbound
**Magician Edge** WebSocket: Settings enrolls/revokes this desktop, the token
lives in the OS credential store, and the lease reconnects with bounded backoff.
The connector advertises installed CUA tools, bounded Magicutor CDP operations
while the extension is connected, and (macOS only) read-only Messages queries —
or an empty manifest when no local provider is healthy. It refreshes every 15 s;
a change rotates the capability generation and aborts work holding the old one.
The browser extension always discovers the local gateway (`127.0.0.1:3017`) and
local Magicutor, never the remote service. Linux and Windows must never advertise
iMessage.

Apple Container 1.0 inspection reads `status.state` and
`configuration.image.reference`; an unrecognized state stops orchestration rather
than being treated as stopped or missing. `make test-desktop-container-routing`
covers inspection and label/control selection; its opt-in live mode restarts only
an explicitly named disposable container:

```sh
MAGICIAN_DESKTOP_TEST_CONTAINER=magician-integration-test make test-desktop-container-routing DESKTOP_CONTAINER_TEST_ARGS=--ignored
```

### Session and credentials

The embedded UI and native helpers share one workspace-bound bearer. Native
Settings signs in/out against the selected server; the bearer is stored in the OS
credential store under the exact origin (scheme, host, port) and passwords are
never retained. Cold launches restore it; a legacy launch bearer bootstraps only
the first origin with no saved decision, and logout persists an empty decision so
the env token cannot return. Credential-store failures are reported, never
replaced by a disk file. Session writes carry a revision so a stale window cannot
clear a newer login. Debug webviews route API and voice/event sockets to the
selected engine, not Vite's proxy. `make test-desktop-auth` uses private test
stores.

- Credentials attach only to credential-free HTTP/WebSocket destinations under
  `/api/magician/` at the exact `engine_base_url` origin (`ws`/`wss` mapped to
  `http`/`https` first, so plaintext `ws://` against an `https://` base is
  untrusted). Redirects are refused; each request reads an atomic
  origin/credential snapshot.
- `magicianWebSocketProtocols(url, protocols)` appends the bearer to the caller's
  subprotocols (e.g. `magician-events-v2`, exported as
  `MAGICIAN_REALTIME_WEBSOCKET_PROTOCOL`), first filtering any caller-supplied
  `magician-bearer.` entry.
- Tray Settings labels the bearer card **Signed-in scope** (server and
  principal·workspace). Notes `space_path` is the per-workspace notes folder.

## Android owner authority

The Android owner Settings surface signs and displays the full canonical
eight-action roster (`snapshot`, `screenshot`, `launch`, `close`, `tap`, `type`,
`key`, `scroll`) with the exact package set; it cannot approve a subset or generic
Android/MCP authority.

- Web Settings is the canonical management surface and owns the trust-method
  choice; Desktop receives only a validated `play_integrity` or
  `owner_pinned_private_build`, displays it and binds it into the signed
  begin-enrollment request. A local page opens the compact approval via an
  origin-fenced loopback POST (fallback: `magican://android-observation`); a remote
  server uses the bounded `host.android-observation/open-settings` Edge capability.
  Browser JavaScript never receives signing authority.
- Expired unconfirmed bootstrap codes are cleared on status refresh. Native owner
  calls carry both a short-lived pinned desktop signature and the session bearer.
  Owner generation `0` is valid between identity bootstrap and the first device
  receipt.
- The reciprocal owner Unix socket lives under
  `~/Library/Application Support/Magican/app-android-owner-v1/` (independent of
  the bundle id and within Unix-socket path limits) and is exposed only with a
  production-valid Apple Team ID. An ad-hoc debug binary keeps the owner typed
  unavailable with one boot diagnostic. An admitted runtime needs the exact
  Magician identifier and the desktop's Team ID.
- Signing is a final-path staging invariant: replacement copies and atomically
  installs, then signs the destination and verifies it strictly; the debug app
  materializer signs nested code before the bundle, and app and nested runtime must
  share a non-empty Team ID when a real identity exists. Nothing is mutated after
  verification. See [scripts: desktop debug app](../scripts/README.md#desktop-debug-app).
- Cold live/static code verification has a 25 s deadline. Magician waits for a
  connection-local desktop readiness barrier (emitted after audit-token/code
  verification with the native-control admission slot held) before recovering or
  minting the 30 s bootstrap nonce. Expired unspent nonces are dropped at point of
  use; hashing and admission waits are not charged against a nonce.

## Local debug tray

- `make build-desktop-tray-debug` stages `./magician-desktop.bin` and a generated
  `.local/Magican-Debug.app`; `make build-macos-speech-helper` stages
  `magician-macos-speech-helper.bin` and `magician-macos-meet-audio.bin` (both in
  `make build-all-debug`). Desktop targets first run `make setup-desktop-pnpm`.
  Build or staging failure aborts the target (no stale-app success).
- On Linux/Windows the same target builds without the macOS wake feature;
  `make run-desktop-tray-debug` runs `pnpm tauri dev`. Without GNU Make on Windows,
  install the pinned pnpm and run `pnpm install --frozen-lockfile` then
  `pnpm tauri dev` in `desktop/`. `make release-desktop[-target TARGET=…]` builds
  packages; `make check-desktop-target TARGET=…` is compile-only.
- `make run-desktop-tray-debug` (started by `make run-all` once the API is healthy)
  and `make restart-desktop-tray` open the **bundle** through LaunchServices,
  verify the gateway on 3017, and return. Never run bare `./magician-desktop.bin`:
  it is real-signed with hardened runtime, and outside a bundle its
  `@executable_path/../Frameworks` rpath resolves to nothing, so dyld refuses the
  vendor `libvosk.dylib` for a Team ID mismatch. `verify-desktop-debug-app.sh`
  runs before launch. LaunchServices owns the process (closing the shell cannot
  reap it; raw `launchctl` can starve Tauri main-thread dispatch). Output goes to
  `magician-host-tray.log`. `magic-supervisor` owns Magician and Magicutor; the
  tray is a peer, not a child.
- With an isolated container `RUNTIME_ROOT_DIR`, pass `DESKTOP_VOSK_RUNTIME_DIR`
  (sets `MAGICIAN_VOSK_MODEL_DIR` when it contains `am/final.mdl`).
- Cold startup binds the host gateway before overlay, Accessibility and shortcut
  work; HUD, Contextual Assist chip and notification overlay are created lazily.

## Draw overlay control regions

The draw overlay is click-through, so its buttons are painted in the webview and
hit-tested by the host from rects reported via `set_draw_overlay_control_regions`
(`dismiss_rect`, `keep_showing_rect`, `replay_rect`, `deeper_rect`), falling back
to mirrored constants where one exists. The page stacks Explain deeper, Keep
showing, Dismiss, and Replay down the top-right, clear of the menu bar and the
Dock. Dismiss and Replay fallbacks use that same corner (Dismiss alone at the
top until shapes exist, then slots under the two controls that have no
fallback). **`deeper_rect` has no fallback** — its visibility depends on Tutor
session state the host cannot reconstruct. Half-open ownership is resolved
before padded targets so Explain Deeper cannot be stolen by Dismiss or Keep
Showing.

While a step is drawn, the tray glides the Orb beside the highlighted shape
(`emit_overlay_draw_shape` → `draw_shape_model_anchor` →
`model_anchor_to_mascot_origin` → `glide_mascot_to`) at most once per 900 ms,
best-effort, docking on clear only if it glided. Kill switch:
`host_gateway.mascot_follows_draw` (default `true`). Product behaviour:
[personal-tutor-overlay.md](../unified-ui/personal-tutor-overlay.md),
[personal-tutor.md](../magician/personal-tutor.md).

## Architecture

```
desktop/
  src-tauri/src/          # Rust backend
    main.rs               # App entry, async setup, runtime detection
    tray.rs               # macOS menu bar / Windows notification area / Linux panel menu
    health.rs             # Per-service health monitor (polls every 5s)
    host_gateway.rs       # Host-native gateway and speech/automation bridges
    orb_state.rs          # Process-owned ambient voice lifecycle + phase contract
    orb_window.rs         # Notch-aware NSPanel, motion, power, hotkey, IPC
    overlay.rs            # Global hotkey, quick overlay window, desktop-side API client
    commands.rs           # Tauri IPC command handlers (frontend ↔ backend)
    config.rs             # TOML config read/write with atomic saves
    env_file.rs           # Settings Environment tab dotenv parser/editor (known provider keys incl. SARVAM_API_KEY)
    container/            # Container runtime abstraction layer
      mod.rs              # ContainerRuntime trait, types, admin escalation
      detect.rs           # Platform detection → Apple or Docker runtime
      apple.rs            # Apple container CLI adapter (macOS >= 26, ARM)
      docker.rs           # Docker/Colima adapter (macOS 14–25, Linux)
      tests.rs            # MockRuntime, orchestration tests
    setup.rs              # First-run setup flow
    permissions.rs        # macOS privacy permission status + Settings deep links
    updater.rs            # Dual-channel updates (container image + app binary)
    cleanup.rs            # Uninstall: tiered cleanup with path validation
    manifest.rs           # Install manifest (tracks pre-existing vs installed state)
  src/                    # Svelte 5 frontend
    App.svelte            # Root: setup/settings/orb routing
    lib/Settings.svelte   # Settings window, This Mac tab: categories, search, permissions, Orb, network, keys, environment
    lib/DesktopSettingsNav.svelte  # Sticky category panel + search (chip bar in a narrow window)
    lib/desktopSettingsIndex.js    # This Mac categories, cards and search keywords
    lib/Setup.svelte      # First-run progress UI with step indicators
    lib/Overlay.svelte    # Hotkey-summoned overlay for launch + follow-up input
    orb/                  # WebGL2 aurora renderer + interactive conversation card
  package.json            # Svelte/Vite frontend deps
```

Desktop Settings is one window with two tabs. **This Mac** (the bundled page)
holds controls tied to the installed app, local runtime and this desktop, on one
page with a sticky category panel, scroll-spy and live-filter search
(`DesktopSettingsNav.svelte`, categories in `desktopSettingsIndex.js`):
Permissions & privacy, Orb & assist, Engine & connection, General, and Keys &
environment. **Magican** switches the same window to web Settings
(`open_shared_settings` → `<ui>/settings?embed=1&host=desktop&device=Mac`), so
account, trust, provider, routing, notes, storage and device configuration stay
web-owned and are not duplicated here. The window gets a capability bound to the
selected server's UI origin (`settings_ui_capability`, like the Attention
window's), so the page is signed in through the desktop session; it can also go
back (`magician-settings:open_desktop_settings`) and send other app links to
`open_application_path` instead of navigating away. With no server selected the
tab falls back to the browser.

`desktop/src-tauri` declares its own workspace boundary and is excluded from
root `cargo check --workspace`. This also keeps Cargo from selecting an outer
checkout's workspace when the checkout is a nested worktree. `make check-desktop`
compiles all targets, enabling `native-wake` on macOS, and is part of
`make check-all`. Check/test do not link `libvosk`, so this gate needs no library
installation.

## Key Concepts

### Container Runtime Abstraction

The `ContainerRuntime` trait has 13 async methods (start, stop, logs, pull_image,
image_digest, tag_image, …) plus sync `name()`; `detect_runtime()` picks the
implementation from OS version and CPU:

- **AppleContainerRuntime** — Apple's `container` CLI on macOS ≥ 26 + Apple
  Silicon (per-container lightweight Linux VMs). CLI availability and
  `container system status` are separate checks; setup starts the system service;
  it uses Apple image/log commands, parses JSON inspect, and resolves remote
  digests through the OCI registry.
- **DockerRuntime** — Colima on macOS, Docker Engine on Linux, Docker Desktop on
  Windows. Linux and Windows require `docker info` to succeed before custody.

Host aliases: Docker Desktop `host.docker.internal`; Apple Container
`host.container.internal` via localhost forwarding (created idempotently through
the native admin prompt, ports published back to loopback). Linux Docker keeps
host networking.

### Health Monitoring

`health_monitor()` polls both services concurrently every 5 s:

| Endpoint | Service |
|----------|---------|
| `http://localhost:{magician_port}/health` | magician |
| `http://localhost:{magicutor_port}/health` | magicutor |

All healthy → Running (green); partial, or container up with no services →
Starting (yellow); all down → Stopped (red). Unchanged state is skipped; a
status-only change updates the disabled status item in place, because macOS
dismisses an open status menu when its menu object is swapped.

### Host Gateway

The tray is the host-native gateway for services that must run in the user's
session. It listens on `host_gateway.bind_host:port` (`127.0.0.1:3017`) and
exposes `MAGICIAN_HOST_GATEWAY_URL` to runtime services:

| Endpoint | Purpose |
|----------|---------|
| `GET /health` | Gateway liveness |
| `GET /host/status` | Gateway URLs; `gateway.ui_url` and `gateway.app_url` (`ui_url` normalized to `/home`) |
| `GET /host/runtime/endpoints` | Versioned loopback Magician/Magicutor API and bridge URLs for extension discovery |
| `GET /host/presence/status` | Deprecated alias of `/host/status`; does not describe or own the orb |
| `GET /host/speech/status` | Speech helper / TTS helper presence and STT authorization |
| `POST /host/speech/transcribe` | Host-native recorded-audio STT via the macOS Speech helper |
| `POST /host/speech/synthesize` | Host-native local TTS via the helper and AVSpeechSynthesizer |
| `POST /host/reminders/create` | Idempotently create a native Apple Reminder and bring Reminders forward |
| `GET /host/automation/status` | Read-only Apple Event probe; `available` means automation works, `accessibility` reports `AXIsProcessTrusted` (`null` off macOS) |

- The device origin (`connect.<zone>`) routes `/host/*` here; `/health` and
  `/api/*` go directly to the selected native (`:3002`), container (`:13002`) or
  remote backend. The gateway does **not** proxy Magician API traffic. Settings →
  Network changes only those two routes, so CUA/browser/iMessage stay on this
  desktop. A native and a container backend must not share one runtime root.
- When the tray manages the stack, it binds the gateway before starting services
  so provider discovery can reach it.
- The packaged CSP allows Settings/overlay WebViews loopback HTTP/WebSocket to
  `localhost` and `127.0.0.1`; keep `tauri.conf.json` `connect-src` aligned with
  `Settings.svelte`.
- **Speech helper resolution:** `MAGICIAN_MACOS_SPEECH_HELPER_BIN` (operator
  override), `host_gateway.macos_speech_helper_bin`, the exe-sibling
  `magician-macos-speech-helper.bin` (carried inside the debug bundle's
  `Contents/MacOS/`, so every launch path finds it), then
  `$CARGO_TARGET_DIR/macos-presence-host/debug/…`, then PATH. No machine-specific
  default. Launch targets do not point the env at the repo-root copy, which may be
  ad-hoc signed — a different code identity for the Speech Recognition grant.
  Built from `native/macos-speech-helper` by `make build-macos-speech-helper`.
- **CUA permissions row:** asks the daemon (`cua-driver call check_permissions {}`)
  because grants belong to `com.trycua.driver`; it reads the 0.28 JSON object and
  older "granted" phrases.
- **Typed Apps macOS host** (`/host/apps/macos/action`) runs a private embedded
  CuaDriver daemon (`cua-driver serve --embedded --socket <app-data>/cua-run/d.sock`)
  from a pinned, digest-bound copy of `CuaDriver.app` staged at pairing, so it uses
  Magican's own grants and exits with Magican. Pair with
  `/Applications/CuaDriver.app/Contents/MacOS/cua-driver` (the `~/.local/bin`
  symlink is rejected); re-pair after upgrading. Before each action it
  re-snapshots and checks that action's fence only (target element and ancestors,
  or window row for a key press). See [Apps macOS host owner](../magician/app-macos-host.md).
- **Reminders bridge:** bounded fields, loopback only, passed to JXA as arguments
  (never interpolated); creation is serialized and receipts persist in
  Application Support so replays do not duplicate. An AppleScript still waiting on
  the Automation prompt at its timeout (default 30 s) is killed and the error says
  a prompt may be showing.
- **Apple Events** need all three of `NSAppleEventsUsageDescription`, the
  `com.apple.security.automation.apple-events` entitlement, and a real signing
  identity (TCC keys to stable code identity); otherwise `-1743`. Launch as a
  bundle; new grants apply only to a fresh process. `System Events` input also
  needs Accessibility (error 1002), which re-signing resets.
- **AX audit log:** every `/host/ax/<action>` is logged with addressing fields
  only (`pid`, `window_id`, `element_index`, `element_token`, `snapshot_id`,
  `bundle_id`, `key`) — the route is plain loopback HTTP and any agent with a
  generic `http` tool can drive it, so the gateway (not the skill controller) must
  keep the ledger. `text` and `value` are excluded by construction.
- **Container reverse relay:** managed local containers get a private
  `container exec --interactive` (or Docker exec) pipe; an embedded Python worker
  listens at **guest** `127.0.0.1:3017` and forwards bounded requests to the Mac's
  loopback gateway — no Mac listener, IP allowlist or network bearer. Only the
  configured managed container gets it; typed Apps routes are excluded. New
  containers wait up to 20 s for relay readiness before booting services;
  containers with an old host-alias URL need it updated at the next replacement.
  The relay also serves `/host/imessage/query` inside Magican: read-only Messages
  database with a SQLite authorizer rejecting writes, attaches and extensions;
  limits 30 s, 1,000 rows, 2 MiB; needs Full Disk Access. `imessage_send` keeps
  the host AppleScript path and approvals. Tests: `make test-container-host-relay`,
  `make test-desktop-container-routing`.
- **Native Messages reads.** The loopback gateway serves the same
  `POST /host/imessage/query` handler to a native macOS backend whose own
  `chat.db` open was refused. macOS judges Full Disk Access on the process it
  holds responsible (launchd's `magic-supervisor.bin`, or a developer's
  terminal), never `magician.bin` itself, so a grant on the backend binary does
  nothing; the signed desktop is its own responsible process. Any local
  process can reach this listener, so the route requires the
  `x-magician-imessage-token` header matching a per-launch token the desktop
  writes to `<runtime root>/run/host-imessage.token` (0600) when the gateway
  starts; without it the answer is 403. That file is the device-local path
  `host_imessage_token`, a rendezvous for a co-located backend, not a read of
  engine-owned storage. `GET /host/imessage/status` answers
  `{"readable": true|false|null}` — whether the desktop can open `chat.db`, no
  content, no token — for the setup wizard's Full Disk Access step; the
  container relay allows it too.

### Voice

The Ambient Orb is the sole host-wide desktop voice entry ([Mac Notch
Orb](mac-notch-orb.md)); it is not a gateway child process. Legacy voice TOML
fields stay readable, but startup unregisters old shortcuts and does not admit the
old gesture. Native conversation is Dictation, Hands-free or Live, seeded once
from backend media preferences.

Settings groups by ownership: **Ambient Orb** (mode, wake, summon shortcut,
follow-up, routing, backend-proxied realtime profile), **Audio Privacy &
Diagnostics** (raw recording retention, assistant-output mute, local mic/playback/
Speech test), **Key Mappings** (`get_hotkey_mappings`). Shared voice preferences,
FluidAudio enablement, per-surface profiles and auto-speak live in Web Settings;
saving desktop settings never writes `/media/preferences` and keeps the live
`voice.voice_mode` mirror. Disabling FluidAudio cancels owned sessions, clears its
TTS cache, stops an owned sidecar and unloads idle models from an external one.

- Recorded Orb turns post WAV to `/api/magician/v2/media/voice-notes`; Magician
  owns STT, chat insertion, artifacts, audit and provider choice. Notes are
  downsampled to 16 kHz mono PCM16 at finalize (`downsample_wav_for_transcription`)
  — the native 96 kHz stream is six times the bytes and is uploaded twice on its
  way to STT; a conversion failure keeps the native bytes.
- Dictation's end-of-speech gate (`AmbientDictationSilenceGate`) is relative to
  the room: a noise-floor estimate (300 ms calibration after wake, fast to fall,
  slow to rise, never raised by speech, fed by lulls well below the talker) with
  speech above 2.5× the floor, bounded by the fixed −42 dBFS line below and 0.25
  above. A fixed line alone sits under ordinary room noise, so questions would
  wait for a quiet dip and empty rooms would run the 45 s cap. Every turn logs its
  boundary, bytes, round trip, outcome, unspoken-reply reason, playback time and
  end reason at INFO.
- Orb Live creates a scoped media session, opens the voice-control WebSocket,
  sends `session.start` with `voice.live_ptt_realtime_profile` (default
  `voice_realtime_openai_backend`; browser calls still use direct WebRTC),
  streams 24 kHz mic PCM on a bounded audio queue with a separate prioritized
  control queue, and plays backend PCM. **Realtime Voice Engine** is a dropdown of
  the backend's `/media/providers` `realtime_voice_profiles` (backend-proxied
  assistant profiles in picker order; unavailable rows greyed with reasons; a
  persisted id no longer offered stays selectable and labelled), degrading to a
  free-text id only when the catalog fetch fails. Muting assistant audio is local
  to the tray.
- Settings provider selectors show only providers verified by `/media/providers`.

**Environment** tab: debug edits `<repo>/.env.development`, release edits
`<repo>/.env` when the repo is discoverable, else the Application Support env file
(`MAGICIAN_DESKTOP_ENV_FILE` pins it). Known keys plus custom ones; secret-looking
values stay redacted until revealed. Writes preserve comments/order, deduplicate
the edited key, append new keys under a Magician marker, and replace atomically;
running services keep their environment until restarted. `CLOAKBROWSER_LICENSE_KEY`
is env-only. Envoy owner identity variables (`MAGICIAN_OWNER_KAPSO_IDENTITIES`,
`…_TELEGRAM_…`, `…_AGENTMAIL_…`) are redacted **Access** entries;
`envoy.owner_identity_envs` stores only the names. **Env Map** is the read-only
`get_environment_snapshot` with optional `config_path` provenance.

### Theme Sync (shared across windows)

The main app surface (unified-ui via `WebviewUrl::External`) and Settings/setup
(tray Svelte on `desktop-app://`) are different origins and cannot share
`localStorage`:

- **Selected theme** is a backend preference (`/api/magician/v2/ui/preferences`,
  per principal/workspace in `ui/preferences.json`). The unified-ui store hydrates
  from it, saves every change, applies `ui.preferences.updated` realtime events,
  seeds it from the local theme when absent, and refreshes on focus.
- **Broker** (`src-tauri/src/theme.rs`) caches `{name, tokens}` in memory:
  `set_app_theme` (publish + broadcast `app-theme-changed`), `get_app_theme`
  (first paint).
- **Publisher** (unified-ui `themeStore`, guarded on `__TAURI_INTERNALS__`) reads a
  fixed 21-variable token contract (`--bg-*`, `--text-*`, `--accent-*`,
  `--border-*`, `--color-*`, `--font-*`) on init, every switch and every remote
  apply.
- **Subscriber** (desktop `App.svelte`) applies tokens as inline CSS variables;
  `styles.css` maps semantic vars onto them with dark fallbacks. Desktop-only cards
  must use semantic vars only (`--bg-secondary`, `--border`, `--text-muted`,
  `--success`, `--danger`, `--warning`); palette literals do not follow the theme.

The Tauri origin bundles every font family used by the 22 web theme variants
(`--font-primary`, `--font-display`, `--font-mono`, `--font-brand`), so it never
falls back to system fonts or needs Google Fonts.

### Native wake word (Vosk in Rust)

Orb wake runs natively (`voice_wake.rs`, the `vosk` crate), because an in-page
audio graph is throttled when the window is backgrounded — exactly when wake is
wanted. A `cpal` stream feeds a background Vosk recognizer (Vosk resamples) and
hands the utterance to the Orb. Only the **Microphone** grant is needed.

Orb Settings are the sole authority: `wake_enabled` defaults off and is
independent of `enabled`; with wake off the Orb is press-to-talk (long-press Left
Option). Active native voice leases suspend the detector so two microphone owners
never overlap.

- `make setup-desktop-vosk` (prerequisite of `build-desktop-tray` /
  `release-desktop`) fetches `libvosk` into `desktop/src-tauri/vendor/vosk/`
  (dylib id `@rpath/libvosk.dylib`; bundled into `Contents/Frameworks`) and the
  model into `<MAGICIAN_ROOT_DIR>/vosk-model` plus a staging copy bundled into
  `Contents/Resources/vosk-model`. Runtime resolution: `MAGICIAN_VOSK_MODEL_DIR` →
  runtime root → bundled resources → `<exe dir>/vosk-model`, skipping incomplete
  directories; with no model, wake logs the path and stays off. macOS-only.
- Behind the **`native-wake`** feature (off by default; the listener is an inert
  no-op without it). Desktop-wake builds, `dev-desktop-build` and macOS CI pass
  it. Because `tauri_build` copies resources and frameworks on every macOS build,
  `build.rs` creates empty stand-ins so non-wake builds stay green; setup validates
  `am/final.mdl` and `conf/model.conf` instead of trusting the directory.

### Permissions

Settings and Setup share a **Desktop Permissions** checklist (Microphone, Input
Monitoring, Accessibility, Speech Recognition), marking which the current voice
configuration requires and deep-linking each Privacy pane. Accessibility's
**Request Access** calls the native trust prompt for the running build (an entry
left by an older build proves nothing). Speech Recognition status and requests go
through `magician-macos-speech-helper status|authorize`; the Desktop bundle also
carries `NSSpeechRecognitionUsageDescription`. Recorded-STT `auto` prefers macOS
Speech, so it needs that grant too; System Voice needs only the gateway and
helper. Providers resolve at boot, so grant access before restarting.

iMessage rows (same checklist, `only` filter): **Full Disk Access** for
`~/Library/Messages/chat.db` (`EACCES` = missing), granted to Magican Desktop
itself because native backends read Messages through it, and **Automation – Messages**
(a harmless test AppleScript triggers the first prompt); required only when
iMessage is enabled. The checklist names Magican Desktop as the app reading the
database and instructs you to quit and reopen it after granting access. macOS
forbids programmatic grants.

### Windows and routing

**Open App** opens the configured unified-UI base at `/today` in the default
browser; general route targets and `magican://` deep links also open in the
browser. Only actionable `/attention` and `/approvals` use the singleton
`magician-attention` window (820×760, 620×560–1100×900) without the general shell.
Notification launches are acknowledged only after the open succeeds; failed opens
undo foreground activation when no other window exists.

Setup and Settings bypass the unified-UI URL (debug:
`magician-desktop://localhost/...` serving `desktop/dist`; bundled: Tauri's asset
protocol), apply the scripted-surface navigation guard, and are transient
foreground windows. Settings opens at 720×640 (min 640×540) with a sticky save
bar.

With local `make run-all` (`MAGICIAN_DESKTOP_MANAGE_RUNTIME=0`), Settings derives
status from local health and shows `Managed by local stack`; top-level
Start/Stop/Restart dispatch Makefile supervisor targets, and Services uses
`supervisor-ctl` locally or `magic-supervisor client` in a managed container. A
read-only tray row shows the mode.

### Tray Menu

The menu-bar item uses the Magican star as a 36-pixel template image displayed
at 18 points:

```
Magician vX.Y.Z — 🟢 Running
magician 🟢 · magicutor 🟢
─────────────────────────────
Quick Automate... (Double Left ⌥)
Ambient Orb
  Status: Ready / Listening / Thinking / Speaking / Paused
  Show Orb / Hide Orb
  Talk Now
  Pause for 1 Hour / Resume Listening
  Let Orb Rest / Wake Orb
Screen
  Screenshot + Ask            ⇧⌥S
  Pick Region + Ask           ⇧⌥A
  Record Clip (toggle)        ⇧⌥R
  Watch Screen (toggle)       ⇧⌥W
Open App (/today in browser) ⌘D
─────────────────────────────
Start
Stop
Restart
─────────────────────────────
Settings...           ⌘,
View Logs...
─────────────────────────────
Update Available (vX.Y.Z) — Install
Install Browser Extension
─────────────────────────────
Quit Magician         ⌘Q
```

Show/Hide Orb affects only the voice control for this app session, independently
of listening. The mascot notch and its question drawer remain available even
when empty. Explicit talk/summon reveals the voice control again; the configured
Orb shortcut summons/settles it. The obsolete Show/Hide Notifications menu action
is removed. The second notch uses the shared star mascot brand asset.

### Quick Overlay

A **double-tap of Left Option** (`quick_overlay_gesture`, native CGEventTap in
`voice_gesture.rs`) opens the overlay; a **long press** instead brings the Orb
forward and listens only while held. An optional chord
(`quick_overlay_shortcut`) is an extra trigger; both route through
`overlay::trigger_overlay_toggle`. The hidden overlay window loads the desktop app
root and picks its view from the window label (no `/overlay` route needed); close
is intercepted and hidden, and Escape stays scoped to it. In local dev it can open
`/chat` on the shared Vite server. Window details:
[hud-overlay-window.md](hud-overlay-window.md).

Capabilities: start a direct run (`POST /api/magician/v2/executions` with
`skip_planning=true`); track multiple overlay runs and reopen when one pauses;
switch between paused runs; render typed pause prompts (`text`, `password`,
`choice`, `multi_choice`, `confirmation`, `external_action`, `file_path`,
`guidance`); surface clarifications and approvals inline; apply shortcut and
Launch-at-login changes without restarting. It calls desktop-side Rust commands
rather than `fetch()` so it honours the configured port. See
[contextual-assist.md](contextual-assist.md) and
[screen-capture-and-ask.md](../magician/screen-capture-and-ask.md).

### Update System

Dual-channel: container image (digest comparison) and app binary (Tauri updater,
Ed25519 signatures).

- **Check loop:** 30 s after startup, then every 6 hours.
- **Container update:** tag current as `magician:previous` → pull → stop → remove
  → start → health check → roll back if unhealthy.
- **App update:** download, verify signature, install, restart. **Combined:**
  container first, then app (the app restart ends the process).

Production tag builds are fail-closed: `TAURI_UPDATER_PUBLIC_KEY` is injected and
the private key required in CI; macOS also requires Developer ID and notarization
inputs; the committed config has no production key. `latest.json` is generated
only when every macOS, Linux and Windows updater archive has a non-empty
signature. Installer assets carry SHA-256 sidecars; the macOS user installer
verifies checksum, disk image, signature and Gatekeeper before copying. The
Windows lane checks out natively (tracked paths must be Windows-compatible),
clears the Tauri bundle directory, and requires exactly one NSIS installer named
with the current version; it clears GNU Make job flags before vendored OpenSSL
calls `nmake`. Agent IDs like `system:scheduler` keep their colon in YAML while the
template directory uses `system-scheduler`.

### Config

TOML lives in the desktop Application Support config dir with atomic writes
(`.toml.tmp` then `rename()`); the home directory is resolved via the platform
(Windows has no `HOME`). Backend runtime state is separate: `MAGICIAN_ROOT_DIR` /
`MAGICIAN_STORAGE_PATH`, default `~/MagicianNotes`. Managed container startup
seeds a missing `magician-config.yaml` (and harness seed files) before mounting
`/data`, preserving existing files ([runtime config seeding](runtime-config-seeding.md)).

Sections: `[general]` (lifecycle ownership, container name/image, overlay
shortcut, screen-ask chords), `[network]` (ports, `engine_base_url`),
`[host_gateway]` (bridge URL, port, speech-helper fields, `mascot_follows_draw`),
`[container]` (CPU/memory), `[voice]` (voice-note/live-PTT settings, recording STT
preference), `[orb]` (lifecycle, wake, power, presentation),
`[contextual_assist]`, `[updates]`. Provider API keys are never in desktop TOML.

For local development without a container, set
`general.manage_runtime_stack = false` or launch with
`MAGICIAN_DESKTOP_MANAGE_RUNTIME=0`; the gateway still starts.

- `make build-all-debug` builds runtime binaries, the UI bundle, the native audio
  engine, the Tauri tray/gateway and the iOS debug app, staging `magician.bin`,
  `magicutor.bin`, `magic-supervisor.bin`, `magician-desktop.bin` and the audio
  engine at repo root.
- `make run-all` starts Vite, the supervisor, then the tray with stack management
  disabled; logs tee to `magician-ui-dev.log` and `magician-host-tray.log`
  (`make tail-service-log SERVICE=stack|magician|magicutor|ui|host-tray`,
  `make open-service-log SERVICE=host-tray`). `make stop-all` stops tray and Vite
  before the supervisor.
- macOS packages `magician-macos-audio-engine.bin` as a Tauri resource; Magician
  owns its lifecycle (lazy start for FluidAudio VAD, loopback auth, terminates only
  its own child). Linux/Windows omit it.

### First-Run Setup

Orchestration in `setup.rs`, streaming progress events to Svelte:

1. CheckingPrerequisites — detect runtime, check if install needed
2. AwaitingConsent — show install plan, wait for approval
3. InstallingPrerequisites — install the reviewed, data-declared macOS host tools
4. InstallingRuntime — install/start Apple Container or Docker through Colima
5. PullingImage
6. CreatingDataDirs — app-support dirs plus the backend runtime root
7. StartingContainer
8. WaitingForHealth → Ready

### Uninstall & Cleanup

| Mode | Removes |
|------|---------|
| Tools & Data | Container + image + runtime (if we installed it) + data dirs |
| Tools Only | Container + image + runtime (if we installed it) |
| Data Only | Data directories only |

Paths are validated via `canonicalize()` / `realpath`; paths outside `$HOME` or
containing `..` are refused; the install manifest ensures pre-existing tools
(Docker, Homebrew) are never removed. See
[install-manifest-cleanup.md](install-manifest-cleanup.md).

### Port Conflict Detection

Before every container start, `port_check.rs` tries to bind each configured host
port (holders found via `lsof -i :PORT -t -sTCP:LISTEN`):

1. **Own stale process** (docker-proxy, com.apple.container) — SIGTERM, then SIGKILL.
2. **Foreign process** — `port-conflict` event with PID and name.
3. Settings shows **Free Ports & Start** (`free_ports` command) or guidance to
   change ports.

## Local Dev Testing

**Service code** runs in the container in managed mode. Fast iteration without a
container: `make build-all-release` then `make run-supervisor`. Container-specific
behaviour: `make dev-container-rebuild` (rebuild image and cycle the container;
the tray sees health within ~5 s).

**Tray app** updates via the Tauri updater:

```
make dev-desktop-setup     # once: Ed25519 keypair at ~/.tauri/magician.key
make dev-desktop-build     # build .app/.dmg with the local update endpoint; install once
# change code, bump version in desktop/src-tauri/tauri.conf.json, rebuild
make dev-update-server     # serves latest.json + signed artifact on :8432
# Tray → "Check for Updates"
```

`dev-desktop-build` overrides the updater endpoint and public key via
`TAURI_CONFIG` at build time (no `tauri.conf.json` edit). Ed25519 updater signing is
not Apple notarization and needs no Apple Developer account.

## Async Safety

Tauri command handlers clone-and-drop `tokio::sync::Mutex` guards so none is held
across an `.await`:

```rust
let config = state.config.lock().await.clone();
let runtime = state.runtime.lock().await;
if let Some(ref rt) = *runtime {
    let rt = Arc::clone(rt);
    drop(runtime); // release lock before any .await
    rt.some_async_operation().await?;
}
```

Read the config in a single statement, never as a block's tail expression:

```rust
// Good — the guard is a statement temporary and drops with the statement.
let config = app.state::<AppState>().config.lock().await.clone();

// Broken — E0597. A block's temporaries drop AFTER its locals, so the guard
// outlives the `state` binding it borrows from.
let config = {
    let state = app.state::<AppState>();
    state.config.lock().await.clone()
};
```

## Contextual assist: save to Notes

`save_to_notes` files the selection and stops — it bypasses the
select-then-generate flow because keeping something is not a draft to approve.
It is a Rust const and a separate Svelte const in the overlay; a rename on either
side would silently fall through to generate, so `make check-notes-capture-surface`
fails the build if they disagree. The overlay posts to `/notes/capture-selection`
with the selection and the target's app, window title and URL, and reports the
path it actually landed on (a fallback provider may differ from the configured
one).

## Canonical References

- [CHANGELOG](../../../desktop/CHANGELOG.md)
- [Mac Notch Orb](mac-notch-orb.md)
- [Contextual Assist](contextual-assist.md)
- [Notification Overlay](notification-overlay.md)
- [HUD Overlay Window](hud-overlay-window.md)
- [Install Manifest And Cleanup](install-manifest-cleanup.md)
- Containerized Deployment Release Readiness
- [Tauri Config](../../../desktop/src-tauri/tauri.conf.json)
- [Container Dockerfile](../../../Dockerfile)
- [Cleanup Script](../../../scripts/cleanup.sh)

The tray/application bundle uses the shared star mark through the PNG, ICO
and ICNS assets in `desktop/src-tauri/icons/`. Update the platform formats
together so installed applications and the development tray share the mark.

## Speaking voices

Native Settings → Conversation Routing → Choose speaking voices opens shared
Web Settings, which exposes TTS and realtime voice catalogs and scoped saved
choices used by Orb calls. Host speech status now forwards the installed macOS
voice IDs reported by the speech helper; the `macos_tts` binding advertises
those choices alongside FluidAudio/Kokoro and online providers. An older helper
without voice metadata keeps the system default until it is updated.
See the [voice selection contract](../magician/realtime-media-rails.md#speaking-voice-selection).

Account setup supports governed MCP OAuth and declarative CLI-owned browser login through [browser sign-in](../magician/browser-signin.md).
