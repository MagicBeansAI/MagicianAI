# Quick Start

This quick start reflects the **no-magictunnel** stack. Default durable
storage is local embedded (`StorageRuntime::open_local`). Magician-bin
installs that runtime process-wide; libraries use `StorageRuntime::current()`
instead of resolving `MAGICIAN_ROOT_DIR`. Compute placement
and storage placement are independent only through the explicit Track B
workflow in [Storage Abstraction](components/magician/storage-abstraction.md).
Copying a scope directory is not a supported migration.

## One-command install

`make install` (a.k.a. `bash scripts/install.sh`) is a single composed installer
that stands up the whole stack — host prerequisites, the runtime data root,
Ollama + models, the Pi coding agent CLI (required; local flow),
the magician/magicutor/supervisor binaries (or a container), meeting audio
(BlackHole 16ch on macOS, Pulse tools and Xvfb on Linux), the macOS desktop tray + presence host, and an optional public
Cloudflare Tunnel — then runs a health sweep.

```bash
# Build-from-source, local-native (the dev default; prompts for data dir + flow)
make install

# Fully non-interactive (CI): all answers via env, no prompts
MAGICIAN_INSTALL_MODE=dev MAGICIAN_INSTALL_FLOW=local \
  MAGICIAN_ROOT_DIR="$HOME/MagicianNotes" MAGICIAN_INSTALL_YES=1 \
  bash scripts/install.sh

# Health sweep only (no install)
bash scripts/install.sh --verify
```

### Guided TUI alternative (current preview)

After cloning the repository, you can take the capability-first terminal path:

```bash
make setup-wizard
```

The Ratatui wizard first probes what already works, asks how to install, then
asks what you want Magican to do. It derives the required components from the
shared component graph, explains dependencies and omissions, remembers prior
choices, and re-probes each setup step before calling it complete.

This is an alternate guided path, but it is not yet a complete replacement for
`make install`. The from-source path is currently available on macOS; prebuilt
requires an existing local package supplied through `MAGICIAN_PACKAGE`, and
the container option remains visible but disabled. The wizard can build,
package and install the runtime binaries and selected capability dependencies,
but it does not yet invoke the core data-root/stack install or final health
sweep. Finish a fresh installation with `make install` for now.

For a read-only view of installed components and the capabilities they enable:

```bash
make setup-wizard ARGS=--status
```

The implementation boundary and all current controls are documented in the
[guided installer reference](components/magician-setup/README.md).

### MODE × FLOW × RUNTIME matrix

| Input | Values | Meaning |
|---|---|---|
| **MODE** | `dev` (default) \| `user` | `dev` builds everything from source (cargo / npm / swift / `docker build`). `user` pulls the released backend image with `FLOW=container`; `MAGICIAN_RELEASE_URL` optionally installs the checksummed, signed, and notarized macOS DMG. A native user-mode backend bundle is not published, so `MODE=user FLOW=local` is rejected. The current production distribution scope is macOS and Linux; Windows is deferred. |
| **FLOW** | `local` (default) \| `container` | `local` runs the native supervisor stack (`make run-all`). `container` runs the backend in a `debian-slim` glibc image; host services (Ollama and the desktop tray) stay native and the container reaches them per-runtime. |
| **RUNTIME** | `docker` \| `apple-container` | Only relevant for `FLOW=container` on **macOS** (auto-selected). macOS ≥26 on Apple Silicon defaults to Apple's native `container`; setup creates Apple's `host.container.internal` localhost-forwarding domain through one administrator prompt and notes its Private Relay interaction. Otherwise macOS uses Docker/Colima. On Linux it is Docker `--network host`. |

Each input has an env var (and a flag) so any combination is reproducible —
see the non-interactive table below.

### Browser modes

The browser skill resolves its driver by config, not by the installer:

- **default (cdp)** — drives the user's **profiled, signed-in host Chrome**
  (agent-browser → magicutor `:3003` → the Chrome extension). Under
  `FLOW=container` magicutor runs in-container and the host Chrome extension
  dials **into** the container's `:3003` (the host-native magicutor is stopped
  first so the extension binds the container's port).
- **headed / headless** — drives its **own** browser: **obscura** (librowser
  stealth Chromium, the config default) if present, else **Chrome-for-Testing
  (CfT)**. In `FLOW=container` both are baked into the image.

The pinned driver is `0.38.1-Magician.0` on upstream `v0.38.1`.
`make -C skillshub rebuild-agent-browser` builds it from
`.cache/agent-browser-src-v0.38.1-magician`, which is excluded from the Cargo
workspace so that cache does not join the Magician crate graph.

### Data-dir non-clobber guarantee

The runtime data root (`MAGICIAN_ROOT_DIR`, default `$HOME/MagicianNotes`) is
**never clobbered**. The installer seeds only files that are **absent**
(`magician-config.yaml`, `.env`, `operator-config.yaml`, the Space skeleton) and
**never overwrites** an existing config, secret, note, or scope. Re-running the
installer on a populated root is a safe no-op for your data. (A fresh root is
fully initialised; an already-populated root skips the seed script entirely so
its unconditional config copy can't overwrite an operator-edited config.)

### Container release qualification

Use an isolated runtime root for release evidence. The harness defaults to
non-standard loopback ports and records health, non-root execution, bind-mount
R/W, inspect/log access, CPU/memory limits, config/data preservation, cold-start
time, and Docker crash recovery:

```bash
make qualify-container-release \
  CONTAINER_IMAGE=ghcr.io/magicbeanbs100x/magician \
  CONTAINER_TAG=1.2.3 \
  CONTAINER_RUNTIME=docker

make measure-container-image \
  CONTAINER_IMAGE=magician \
  CONTAINER_TAG=1.2.3
```

JSON evidence is written under `coverage/container-qualification/`. Optional
`CONTAINER_MAX_COLD_START_MS`, `CONTAINER_MAX_COMPRESSED_BYTES`, and
`CONTAINER_MAX_EXPANDED_BYTES` values turn measurements into release gates.

### Real local end-to-end container qualification

Run a disposable real-container build and qualification without touching the
live runtime root or standard service ports:

```bash
make qualify-container-e2e
```

To exercise the complete personal installation, explicitly confirm the live
run. This stops the current native supervisor/tray, runs the composed installer
against the selected root, validates host/container service routing, restarts
the installed container, verifies persistence and config non-clobbering, then
runs the disposable qualification against the same image:

```bash
make qualify-container-e2e-live \
  CONTAINER_E2E_LIVE_CONFIRM=1 \
  CONTAINER_E2E_ROOT="$HOME/MagicianNotes"
```

The installed stack remains running. Consolidated JSON, stage logs, runtime
endpoint data, container inspect/log output, and the isolated qualification
report are retained under
`coverage/container-qualification/local-e2e/<run-id>/`. Use
`CONTAINER_E2E_RUNTIME=docker|apple-container`,
`CONTAINER_E2E_IMAGE=<ref>`, or `CONTAINER_E2E_REPORT_DIR=<path>` to override
the defaults. The live command is intentionally not suitable for parallel dev
sessions because it stops and replaces the active backend/tray. macOS TCC
approval, packaged Chrome-extension pairing, and an actual user Chat/memory/
browser workflow remain manual acceptance gates because shell automation cannot
grant privacy permissions or safely substitute for the signed-in browser flow.

### Cloudflare Tunnel — Kapso webhook (opt-in)

Kapso's WhatsApp callbacks need a public URL. The tunnel is **off by default**;
set `MAGICIAN_ENABLE_FUNNEL=1` to bring up a [cloudflared](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/)
named tunnel pointed at the Kapso webhook port (native or container-mapped). It
uses a **stable** URL on your own Cloudflare zone — `https://webhook.<zone>/webhook`
(`MAGICIAN_TUNNEL_ZONE`; no zone is defaulted) — that you set in Kapso **once**, written to
`$DATA_DIR/funnel-url` and printed at the end of the install.

There are **two auth modes** (`MAGICIAN_TUNNEL_MODE`, default `browser`); the
script logs which one is in effect and falls back gracefully:

**`browser` (default) — locally-managed named tunnel.** First use is a one-time,
interactive operator setup (browser OAuth + a tunnel create); the script
**detects** that it's not set up and prints the exact steps rather than scripting
the browser:

```bash
cloudflared tunnel login                              # browser OAuth; pick your zone
cloudflared tunnel create magician
cloudflared tunnel route dns magician webhook.<zone>  # e.g. webhook.example.com
cloudflared tunnel route dns magician ui.<zone>       # optional: dev UI (gate w/ Cloudflare Access)
```

Re-run with `MAGICIAN_ENABLE_FUNNEL=1` afterwards and it writes the config,
ensures the DNS routes, and starts the tunnel as a `brew services` daemon.

**`token` — dashboard-managed headless connector.** If you created the tunnel in
the Cloudflare Zero Trust dashboard and have its **connector token**, set
`MAGICIAN_TUNNEL_MODE=token` and put the token in `CLOUDFLARED_TOKEN` (env, or your
runtime `.env` / `.env.development` — the script reads it from there and never
logs the value). The script then runs `cloudflared tunnel run --token …` directly
(no `cloudflared tunnel login`). **If `MAGICIAN_TUNNEL_MODE=token` but
`CLOUDFLARED_TOKEN` is empty, it falls back to `browser` login and says so.**

A token (dashboard-managed) tunnel stores its **public hostname + DNS in
Cloudflare, not in `config.yml`**, so those need configuring — two ways:

- **Auto (recommended) — give the script a Cloudflare API token.** Create one at
  **dash.cloudflare.com → My Profile → API Tokens → Create Token → Custom token**
  with **Account · Cloudflare Tunnel · Edit** *and* **Zone · DNS · Edit** (zone
  `<zone>`); save it in your runtime `.env.development` under the key
  **`CLOUDFLARE_API_TOKEN`**. On the next `MAGICIAN_TUNNEL_MODE=token` run the
  script sets the public hostname (`webhook.<zone> → http://localhost:3010`, plus
  `ui.<zone> → http://localhost:5173` unless `MAGICIAN_TUNNEL_UI=0`) **and** creates
  the proxied DNS CNAME(s) via the API — no dashboard clicks. (The account/tunnel
  ids are decoded from `CLOUDFLARED_TOKEN`. A scoped Tunnel+DNS token legitimately
  fails `/user/tokens/verify` with "Invalid token" — that endpoint needs User
  scope; the token still works here.)
- **Manual — the dashboard (no API token).** **Zero Trust → Networks → Tunnels →
  your tunnel → Public Hostname → Add:** `webhook.<zone>` → **HTTP**
  `localhost:3010` (saving auto-creates the DNS record).

For reboot persistence, install the connector as a service once:
`sudo cloudflared service install <CLOUDFLARED_TOKEN>`.

> ⚠ **Never put `webhook.<zone>` behind Cloudflare Access.** Webhooks can't
> complete an interactive login, so Access returns a `302` to its login page and
> Kapso never reaches the endpoint (the webhook is secured by its signing secret,
> not a login). The script self-verifies after configuring and warns loudly if it
> detects an Access redirect on the webhook host. Access belongs **only** on the
> dev UI (`ui.<zone>`).

Either mode, the same tunnel also serves the dev UI at `https://ui.<zone>/` via
`make serve-ui-tunnel` (phone-reachable, real HTTPS cert for audio/voice; gate it
behind a Cloudflare Access policy so it isn't world-open).

### Non-interactive / CI env vars

With `--yes` / `MAGICIAN_INSTALL_YES=1` every prompt takes its default, so no
`read` ever blocks (a `</dev/null` run completes cleanly). Every prompt has a
corresponding env var:

| Env var | Flag | Prompt it answers / controls | Default |
|---|---|---|---|
| `MAGICIAN_INSTALL_MODE` | `--mode` | dev (build) \| user (prebuilt) | `dev` |
| `MAGICIAN_INSTALL_FLOW` | `--flow` | local \| container | `local` |
| `MAGICIAN_ROOT_DIR` | `--data-dir` | runtime data dir (config + secrets + notes) | `$HOME/MagicianNotes` |
| `MAGICIAN_CONTAINER_RUNTIME` | `--runtime` | docker \| apple-container (Mac container only) | auto-detected |
| `MAGICIAN_INSTALL_YES` | `--yes` | take all defaults; never prompt | `0` |
| `MAGICIAN_ENABLE_FUNNEL` | — | bring up the public Kapso Cloudflare Tunnel | `0` (off) |
| `MAGICIAN_TUNNEL_NAME` | — | cloudflared named-tunnel name (browser mode) | `magician` |
| `MAGICIAN_TUNNEL_ZONE` | — | Cloudflare zone for the stable webhook URL | none — required for the tunnel; no script defaults a zone |
| `MAGICIAN_TUNNEL_MODE` | — | cloudflared auth: `browser` (login) or `token` (headless connector) | `browser` |
| `CLOUDFLARED_TOKEN` | — | connector token for `MAGICIAN_TUNNEL_MODE=token` (env or runtime `.env`/`.env.development`) | unset |
| `CLOUDFLARE_API_TOKEN` | — | Cloudflare API token (Account·Tunnel·Edit + Zone·DNS·Edit) so token mode auto-sets the public hostname + DNS; runtime `.env.development` | unset |
| `MAGICIAN_TUNNEL_UI` | — | `0` = webhook-only tunnel (skip the dev-UI host `ui.<zone>`) | `1` (UI host on) |
| `MAGICIAN_CONNECT_HOST` | — | stable protected origin learned by iOS, Android, and ESP32 during enrollment | `connect.<zone>` |
| `MAGICIAN_CONNECT_BACKEND` | — | backend selected behind that stable origin: `local` or `container` | `local` |
| `MAGICIAN_CONNECT_LOCAL_API_PORT` | — | host port for `make connect-local` | `3002` |
| `MAGICIAN_CONNECT_CONTAINER_API_PORT` | — | host-published port for `make connect-container` | `13002` |
| `MAGICIAN_IMAGE_REF` | — | container image to build/pull | `ghcr.io/magicbeanbs100x/magician:latest` |
| `MAGICIAN_RELEASE_URL` | — | release bundle URL for `MODE=user` (local) | unset (required for user/local) |
| `MAGICIAN_RELEASE_SHA256` | — | optional explicit SHA-256 for the macOS DMG | unset |
| `MAGICIAN_RELEASE_SHA256_URL` | — | checksum sidecar URL; defaults to `<MAGICIAN_RELEASE_URL>.sha256` | inferred |
| `MAGICIAN_INSTALL_DRYRUN` | — | announce heavy phases instead of running them | `0` |

Model tags are config-sourced: setup resolves generation models from Ollama
profiles referenced by `llm.router.operation_mapping` and reads the embedding
contract from `runtime.ollama`. Missing configuration or unavailable models
fail setup/prewarm; the installer has no model fallback.

Provision the Access application and stable route once with `make connect-setup`.
When both backends are installed, use `make connect-local` or
`make connect-container`; each command refuses an unhealthy target and keeps the
hostname unchanged. `make connect-status` reports the saved choice and both
listener states. Merely starting a second backend never changes public routing.
The selector does not stop either process. Run both only with separate runtime
roots; the desktop-managed container bind-mounts its configured host root as
`/data`, so native and container backends must not write the same root at once.

Steps that may still prompt regardless of `--yes` (delegated to sub-scripts, by
design): `setup-container-runtime.sh` → `container system start` can ask for an
admin password once on first run (macOS Apple `container`); and the Cloudflare
Tunnel needs a one-time `cloudflared tunnel login` + `tunnel create` (the script
prints the steps and exits cleanly until that's done — it never scripts the
browser).

### macOS desktop tray + presence host

On macOS, `dev` mode builds and launches the Tauri desktop tray (the host
gateway `:3017`) + the native presence/voice host, then prints the macOS TCC
grants you must approve: **Screen Recording, Accessibility, Automation,
Microphone**. On Linux this phase is skipped cleanly (headless; the mac skills
are auto-hidden by the host_gateway availability gate). The host gateway's
`/host/ax/<action>` route is now live (it relays Accessibility actions to the
cua-driver daemon); macos-ui-automation uses a local-first relay — the native
cua-driver when present, otherwise the gateway. The driver is pinned to one
release: install or repair it with `make setup-cua-driver ARGS=--start`, and
`make check-cua-driver` fails when the installed version differs. Operators building the
desktop/operator app should track the build watch-list in
`docs/runbooks/2026-06-22-container-tauri-browser-local-e2e.md`.

## Prerequisites
- Rust 1.88+ (the workspace's highest declared `rust-version`; the container builds with 1.92)
- Node.js 18+

## 1. Setup & Build

```bash
# Install all sub-project deps + build SDK artifacts (one-time after clone/worktree)
make setup-all

# Build release-mode binaries used by the local supervisor flow
make build-all-release

# Optional: faster local iteration with debug binaries copied into the same .bin files
# make build-all-debug
```

For local MLX Decision Engine development, use
`make build-decision-engine-mlx-debug`. The debug profile optimizes the `mlx-sys`
numerical dependency: its CPU kernels merge and quantize Kev weights at load time.
Magician and Decision Engine application code retain the normal debug profile.

On macOS, `setup-all` also installs the JDK 21, Android SDK, and Gradle 8
toolchain needed by Magdroid. This can add several gigabytes on a fresh host;
non-macOS hosts skip the Homebrew-specific Android bootstrap. After setup,
`make test` includes Magdroid's Gradle unit tests and `make check-all` includes
its combined build-and-test gate.

The root `Makefile` defaults Rust build artifacts to
`/Volumes/build/magician/builds` (when writable; otherwise the checkout-local
`target/`), so Makefile-driven checks, builds, clippy, docs, tests, and Tauri
builds keep their Cargo artifacts on the `ssd1` drive. The tracked
`.cargo/config.toml` pins no `target-dir`, so raw `cargo` runs must set
`CARGO_TARGET_DIR` themselves (or rely on the `make link-target-dir` symlink).
VS Code / Cursor rust-analyzer flycheck is configured separately in
`.vscode/settings.json` to use `/Volumes/build/magician/builds/rust-analyzer`,
because editor-spawned Cargo checks do not inherit Makefile exports.

### Faster builds & tighter iteration

The `magician` crate is large, so a few knobs cut dev-loop time:

- **Parallel worktrees don't fight for the build cache.** The primary checkout
  builds into the shared `/Volumes/build/magician/builds`; each linked worktree
  auto-isolates to `…/builds/wt/<name>` (Makefile). This avoids cargo's
  exclusive target-dir lock and cross-worktree incremental thrash when two
  sessions build at once. Dependencies stay shared via sccache.
- **`FAST=1` — whole-workspace fast lane.** Prefix any target to compile on the
  nightly parallel rustc frontend (`-Zthreads`) in an isolated `builds-fast`
  dir; speeds up both checks and builds across every crate:

  ```bash
  make check-all FAST=1
  make build-all-debug FAST=1
  ```

  Run `make setup-fast` once first — it installs the nightly toolchain plus the
  `llvm-tools-preview` component the coverage/test lanes need under FAST (a plain
  `make test FAST=1` fails without it). Add `FAST_CRANELIFT=1` for the Cranelift
  codegen backend (~2-3× faster codegen; opt-in — may fail on `ring`; run
  `make setup-cranelift` once). Coverage lanes don't work with `FAST_CRANELIFT`
  (Cranelift can't instrument) — use stable (no FAST) for authoritative coverage.
  Keep `FAST=1` consistent across a whole invocation (build + copy/restart use
  the same target dir), and run a plain `make check-all` before committing so
  code is verified on the stable toolchain you ship.
- **Single-crate loops.** `make check-magician-fast` (bin-only check),
  `make watch-magician` (save-triggered re-check; needs `cargo install
  cargo-watch`), `make build-magician-cranelift`.

The structural fix for "one character rebuilds the whole crate" — splitting the
~850k-LOC crate — is designed in
`docs/plans/2026-07-23-magician-crate-split-design-plan.md`.

### Runtime root

Magician keeps **all live state, config, and operator secrets** under a single
**runtime root**, resolved as `MAGICIAN_ROOT_DIR` → `MAGICIAN_STORAGE_PATH` →
`$HOME/MagicianNotes` (the macOS default when neither env var is set). At its top
level it holds `magician-config.yaml` (the storage-provider selector is the
`workspace_storage:` section inside it), `.env` / `.env.development`,
`operator-config.yaml`, `client_secret.json`, `secrets/`, `wake_up_queue.json`,
and the per-scope `scopes/` tree. Mount this one directory into a container and
the deployment is self-contained — config survives image rebuilds. Remote
transactional state, when an operator later selects `remote_durable`, is
PostgreSQL (Decision Gate 1); local installs keep SQLite. The Gate 1 spike is
`make test-storage-gate1` and is not part of normal startup. S3 object/dataset
adapters exist in `magician-storage-s3` but stay dormant until an explicit
remote profile is selected. Transactional repository helpers live in
`magician-storage-state` and are also dormant. The owner-closure and
migration coordinator (`magician-storage-migration`) is dormant too; normal
startup does not plan or cut over stores.

`magician_data_v3/` (in the repo) is the read-only **seed**: agent/db/trust
templates plus `operator-config.template.yaml` and `.env.example`. The runtime
config seeds are the repo-root `magician-config.yaml` and `llm-router.yaml`. Nothing is written to the seed at runtime; the seed scripts
copy the templates to the runtime root for a real run.

### Operator-supplied config (one-time)

`make setup-all` reads two operator-supplied files from the runtime root to
populate per-skill `.env` files, per-bot `.env.<account>` files, and per-account
gws auth dirs:

- **`$MAGICIAN_ROOT_DIR/operator-config.yaml`** — copy from
  `magician_data_v3/operator-config.template.yaml`, keep API keys as `${VAR}`
  references in the `secrets:` section, put their real values in `.env` /
  `.env.development`, and configure selectable Google Workspace accounts in
  `gws_accounts:`. Declare service-owned fixed identities such as Presto separately
  in `gws_fixed_profiles:`; fixed profiles are not exposed through the public
  `account` selector. Mirror those entries in the provider-neutral
  `tool_runtime_profiles:` list using `provider: google-workspace`,
  `storage_namespace: gws`, `alias: <name>`, and optional
  `expected_identity: <email>`. The governed tool runtime reads only this generic
  registry; the legacy GWS keys remain temporarily for Gmail bot/setup consumers.
  skillshub setup reads it from the runtime root (`MAGICIAN_ROOT_DIR`), falling
  back to an in-tree `skillshub/operator-config.yaml` for dev checkouts.
  Each entry in either registry takes a `name:` and an optional `expected_email:`.
  When present, the governed runtime performs a fresh CLI status check and refuses
  to execute if the active Google identity differs. A selectable entry may also set
  `default: true`; fixed entries never participate in default account selection.
  `make setup-all` creates the isolated `auth/gws-<name>` directory for entries in
  both registries, but only `gws_accounts` are projected into shared tool selectors
  and Gmail bot account configuration.
- **`$MAGICIAN_ROOT_DIR/client_secret.json`** — your Google OAuth Desktop client JSON (in-tree `skillshub/client_secret.json` works as a dev fallback).
  Download once from
  <https://console.cloud.google.com/apis/credentials> (Create OAuth Client →
  Desktop app → Download JSON).

Both files are gitignored. The legacy locations
(`<repo>/.env copy.development`, `<repo>/accounts.txt`,
`<repo>/client_secret.json`) are still honored as a fallback when the new
files are absent — emits a deprecation warning at setup time.

### Bot runtime dependencies (`gmail`, `telegram-self`, `whatsapp`)

Three bots spawn external CLI binaries (`gws`, `tgcli`, `wu` respectively).
These binaries are installed at `<scope>/bots/<bot>/node_modules/` by
`install-bot-bundles` (one-time `npm install --omit=dev` per bot per
scope). Each bot's resolver finds its CLI at
`<scope>/bots/<bot>/node_modules/.bin/<binary>` — no env-var override
or skillshub-relative path needed at runtime, so the scope copy is
fully portable across machines. Total per-scope cost: ~50 MB; the
install is idempotent and cached on subsequent runs.

## 2. Configure Local Tool Runtime

The default config file is `tool-runtime-config.yaml`:

```yaml
registry:
  type: file
  # Extra "system-root" directories overlaid on per-scope content.
  # Each entry is expected to contain
  # `skills/<skill>/tool_schema.yaml` and/or
  # `agent_templates/agents/<id>/definition.agent.yaml` — mirroring
  # the layout of system/. Per-scope content always wins. Within
  # extras, first-listed wins (matches PATH / XDG_DATA_DIRS).
  paths:
    - ~/team-skills
    - /opt/shared-agents

semantic_search:
  enabled: true
  similarity_threshold: 0.0
  max_results: 25
```

`semantic_search` is the historical config key name; in this branch it drives
local lexical ranking (not vector embeddings).

## 3. Run Services

```bash
# Starts magic-supervisor on :8081 and manages magician (:3002) + magicutor (:3003)
make run-supervisor
```

Re-running `make run-supervisor` is safe: it stops an existing supervisor first and
fails fast with listener details if another process still owns `8081`, `3002`, or `3003`.

## 4. Run UI

```bash
make run-ui-dev
```

Open the UI at the Svelte dev URL (usually `http://localhost:5173`).
Port `3002` is the Magician API during repository development, while `5173`
is the Vite UI. `make run-supervisor` does not start Vite; use `make run-all`
or run both commands above when Desktop's `host_gateway.ui_url` points to
`http://127.0.0.1:5173`. Packaged/container installs instead pass a built
frontend directory to Magician and can serve the UI from the engine port.

## 5. Validate

```bash
make check-magician
make check-ui
make check-bots
```

## 6. Resource Authority (optional — spend gating + budget caps)

Magician ships a double-entry ledger + spend-token system that caps tool spend (`0.7.3`). `resource_authority.enabled` is **on** in the shipped config. Commodities are free-form names (`USD`, `EMAIL_SENDS`, `INR`); matching is case-insensitive (`usd` == `USD`). A budget `id: "*"` is a **shared** pool for every principal/agent/tool at that scope, not a per-identity copy. `period: daily` (and other non-`total` windows) re-funds the ceiling at each UTC window; leftover with `carryover: none` is clawed back. An in-flight reservation still occupies the new window — a checkout that crosses midnight cannot spend a second full daily pool while the first hold is open.

### Enabling

The active `magician-config.yaml` includes an enabled `resource_authority:` section. Tailor it, for example:

```yaml
resource_authority:
  enabled: true

  system_ceilings:
    - id: usd-monthly
      commodity: USD
      ceiling: 1000.0
      relaxation: 0.02
      period: monthly
      carryover: { type: none }

  # Each budget row issues a `SpendToken` lazily on first lookup. The
  # gate stacks all matching rows at dispatch time — a USD spend by
  # `agent: presto` for tool `create_task` consults principal + agent +
  # tool budgets and rejects if ANY is exhausted.
  budgets:
    - scope: principal
      id: owner             # your principal id (look at logs, default deployments use "anonymous")
      commodity: USD
      ceiling: 100.0
      period: monthly
      carryover: { type: none }

    - scope: agent
      id: "*"                # any agent; one shared USD pool
      commodity: usd         # case-insensitive; stored/compared as USD
      ceiling: 50.0
      period: daily

    - scope: tool
      id: create_task        # cap any single tool
      commodity: USD
      ceiling: 5.0
      period: monthly
      carryover: { type: full }
```

Restart magician after editing — the resolver loads config at boot, runtime edits aren't picked up. After restart, an already-issued config token copies ceiling/period/carryover from the live YAML row (it is not re-funded).

### How a dispatch flows

1. A tool call with `execution.spend: { type: counted, commodity: USD, cost_per_action: 1 }` arrives at the gate.
2. The gate resolves the per-`(principal, workspace)` ledger / token store / ceilings / freeze bundle. First touch loads `<storage_root>/scopes/{principal}/{workspace}/resource_authority/{resource_ledger.jsonl, token_store.json, system_ceilings.json, system_freeze.json}` from disk; subsequent calls share the cached bundle.
3. `spend_session::admit` walks budget rows that match `(principal, agent_id, tool_name, commodity)` (exact id, else `*`) and lazily issues a `SpendToken` for each. Chat dispatch, Zepto/Swiggy checkout (including checkout from an app), REST reserve, and app compiled/OS-jail tool I/O all use this one writer. Matching tokens are reserved as a stack (any ceiling trip rolls the whole stack back). App MCP must not wrap a second spend gate around `dispatch_governed_mcp`.
4. Dispatch `spend:` with no matching row **fails open** (runs uncounted). Commerce checkout, REST reserve, and app spend **fail closed**. A matching budget with amount `<= 0` is rejected. Governed MCP without principal/workspace/agent fails closed.
5. After reserve/commit/rollback, `ScopedAuthorityBundle::persist_state` flushes the ledger + token store atomically (tmpfile + fsync + rename). A live `SpendHold` must be committed, rolled back, or `persist()`ed for recovery — otherwise Drop logs a leak.
6. After a remote MCP checkout has been sent, `InputRequired` / `Task` / ambiguous transport, an oversized success stub, and app label/settlement persistence faults keep a **non-retry** success so the caller does not place a second order.

### Viewing budgets and spend

Open the `/budget` UI (Svelte route under the `(app)` group). It reads the same per-scope ledger the gate writes to, so updates land in real time. The UI surfaces:

- System ceilings (read + edit via REST CRUD)
- Outstanding spend tokens, per-token detail (velocity, burn rate, projected exhaustion)
- Full transaction history filterable by commodity / token / since
- Reservations list with flag/commit/rollback actions
- Global freeze + unfreeze, per-period close, refund, vendor credit
- Audit check (conservation invariant verification)

REST endpoints live under `/api/magician/v2/resource-authority/*` — the UI is a thin client. Ceiling `reserved_in_period` is live in-flight spend (stacked `batch_id` counted once). Direct API access is documented in [`docs/components/magician/resource-authority-api.md`](components/magician/resource-authority-api.md) (`magician-api/src/resource_authority_api.rs`).

### Failure surface

When the gate rejects, you see structured `ExecutionError::Configuration` messages with caller-actionable text:

- `"No budget configured authorizing '<tool>' to spend '<commodity>'..."` — REST/commerce fail-closed when no budget row matches. Dispatch `spend:` tools **run uncounted** instead (check logs for the fail-open warning).
- `"Gated dispatch of '<tool>' has unsafe principal id..."` / `"...has unsafe workspace id..."` — id contains path separators, control chars, `..`, or is empty / longer than 255 bytes.
- `"Cannot delegate to agent '<target>': spend token '<id>' is not Active (status=...)"` — delegation carries a revoked / expired / wrong-recipient token. Variants: `InactiveSpendToken`, `ExpiredSpendToken`, `SpendTokenIssuedToMismatch`, `UnknownSpendToken`.
- `"Cannot delegate: agent '<source>' was itself delegated to (owner_stack depth=N) and its definition does not opt in to transitive delegation."` — set `constraints.coordination.allow_transitive_delegation: true` on the source agent template.

When the budget ceiling is hit:

```
budget gate: token <id> exhausted (available <X> < requested <Y>)
```

### Adding `spend:` to a pack

Edit the pack's `tool_schema.yaml` (or the embedded YAML under `magician/src/magician_v2/execution/embedded_pack_defs/`):

```yaml
execution:
  spend:
    type: counted          # counted | committed | metered
    commodity: USD
    cost_per_action: 1
```

Three declaration shapes:

- **`counted`**: fixed `cost_per_action` per dispatch. Use for unit-priced external calls (one email send, one API call).
- **`committed`**: `cost_parameter: <param_name>` — the gate reads the cost from the tool's resolved params at lower time. Use when the LLM knows the cost upfront (e.g. `budget_amount` on an ad-spend tool).
- **`metered`**: `cost_parameter: <param_name>` — the gate reserves 3× the estimated cost as a safety margin and commits the actual amount after the tool returns. Use when the actual cost is only known post-execution.

After editing, the change is picked up automatically — no Rust recompile needed for pack-side YAML edits. Confirm by dispatching the tool and checking the `/budget` transactions page.

### Crash recovery

The ledger journal is append-only JSONL. If the process crashes mid-dispatch, on next boot `reconstruct_active_reservations` walks the journal and rebuilds the in-memory `active_reservations` map for any reserve entries without matching commit/rollback. The REST API's `/api/magician/v2/resource-authority/reservations` endpoint surfaces these for operator triage (flag / commit / rollback). Phase D atomic writes guarantee no partial journal lines on disk.

## Notes
- `/magictunnel` routes are removed.
- Tool discovery and matching happen in-process inside Magician via `tool-runtime-core`.

## Decision engine on the GPU (optional, Apple Silicon)

The decision engine runs Kev on the GPU when built with its `mlx` feature:
`make build-decision-engine-mlx-release`, then `make setup-decision-models
MODELS="kev-0.8b-mlx"` and a `kev-mlx` model entry in
`<runtime root>/decision-engine.yaml` (the seed shows one). The build needs
Rust 1.95 (installed by the target if missing), CMake, and Xcode's Metal
Toolchain (`xcodebuild -downloadComponent MetalToolchain`); the default
workspace build does not. The workspace `Cargo.toml` carries a
`[patch.crates-io]` entry for `mlx-sys` that kev-rs requires; it only takes
effect when the `mlx` feature is built. See
[structured-decision.md](components/magician/structured-decision.md).

## Local audio engine operations

On supported macOS hosts, Dictation, Meeting, Listening, and Hands-free can use
the configured FluidAudio sidecar without changing their public media APIs.
Choose surface profiles and inspect engine/model state under **Settings > Voice
and audio**. Advanced mode shows model residency and active sessions and exposes
load/unload controls only when the backend allows them. The FluidAudio engine
switch uses the shared revisioned settings API, persists the live
`magician-config.yaml`, and is available from both the web and Tauri settings
surface. Turning it off immediately prevents new FluidAudio resolution, stops a
Magician-owned sidecar, closes Magician-owned FluidAudio streams, clears its TTS
cache, and leaves configured profiles to fall back to their next available
providers. An external sidecar process is never killed; Magician requests unload
of its idle configured models after active streams close. Turning the engine on
revalidates host support, restores the already-registered providers without a
service restart, and runs configured prewarm work in the background after the
settings response is ready.

```bash
make build-macos-audio-engine-debug
make audio-engine-status
make audio-model-prewarm AUDIO_MODEL=fluid-qwen3-asr-f32
make audio-model-unload AUDIO_MODEL=fluid-qwen3-asr-f32
make verify-media-audio-rollout
```

The sidecar starts lazily. A status read or unload against an idle engine does
not start it; stopping Magician or disabling FluidAudio stops only a sidecar it
owns. Containers and non-Apple hosts keep their existing providers and omit
FluidAudio capabilities.
See the [Phase 9 rollout](components/magician/fluid-audio-phase9-rollout.md) for
recorded defaults, benchmarks, packaging checks, and remaining hardware gates.

## Meeting bot (macOS and Linux, experimental)
A voice surface for the magician agent in a Google Meet — it listens, transcribes,
summarizes, and answers (via the **agent**) only when addressed by
"Hey Magican". By default the agent replies in its **realtime voice** through the
running server's `VoiceOrchestrator` (full tools + context + server-side session
rotation), so the supervisor (`:3002`) must be up. On a wake phrase it streams the
question **audio** to the model (it hears you, not just a transcript). `MEET_BOT_RESPONDER` selects a
fallback: `agent-tts` (agent over REST + OpenAI TTS), `llm-tts` (standalone
local-LLM + TTS), `realtime-direct` (bot-local OpenAI Realtime, no agent).

`make install` and `make setup-meet-bot` install the audio devices. On macOS
that is the BlackHole 16ch virtual microphone (the bot speaks into it; reboot
once after the cask), `switchaudio-osx`, and the ScreenCaptureKit helper.
Listening uses ScreenCaptureKit, so grant Screen Recording to the app that
runs the bot. Linux installs Pulse tools (`pactl`, `parec`, `pacat`) and Xvfb.
Join on Linux speaks into the `magician_meet_mic` null-sink and listens on
the browser's Pulse output, or the default sink monitor when that output
is not attributed yet. A headless host gets Xvfb. Pulse has to be running
in the same environment as the bot.

With the supervisor running (`:3002`) and Chrome's microphone set to
`BlackHole 16ch`:

```bash
make setup-meet-bot
export OPENAI_API_KEY=sk-...                    # TTS (+ optional cloud STT)
CARGO_TARGET_DIR=/Volumes/build/magician/builds cargo run -p magician --example meet_bot -- "test"
```

Say "Hey Magican, …". Setup detail: [Meetings](components/magician/meetings.md).
