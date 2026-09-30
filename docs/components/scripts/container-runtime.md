# Container runtime and local desktop integration

## Persistent device pairing on headless Linux

The runtime keeps durable keys and pairing generation anchors in the Linux
Secret Service. The image includes `dbus`, `gnome-keyring` and
`libsecret-tools`; the launch follows the documented
[GNOME keyring daemon](https://manpages.debian.org/bookworm/gnome-keyring/gnome-keyring-daemon.1.en.html)
and D-Bus interfaces.

### Keyring session contract

Pass both `MAGICIAN_KEYRING_STATE_DIR` and `MAGICIAN_KEYRING_PASSWORD_FILE` at
container launch to activate an isolated, non-root service session:

- Mount state separately from `/data` (e.g. `/keyring`, owned by the runtime UID,
  mode 0700) and the unlock secret read-only (e.g.
  `/run/secrets/magician-keyring-password`, mode 0400/0600).
- The secret is 32–4096 bytes, used verbatim (newlines included). Persist both
  across recreation and protect them separately from workspace backups.
- A wrong unlock secret never replaces an existing keyring. Losing the keyring
  makes an existing sealed roster unavailable.
- Partial configuration fails startup. With neither setting the entrypoint uses
  the plain startup path, whose key/pairing readiness must be checked
  separately.

`scripts/run-linux-keyring.py` runs the supervisor after proving the login
collection is unlocked. The application inherits only the private D-Bus address,
not the service's XDG settings or secret bytes; signals reach the application
before the owned keyring and bus stop. `--password-stdin` accepts an
owner-provided pipe. It adds no secret store and changes no device-token,
roster-seal or rollback validation.

The entrypoint and launcher use umask 077 so new runtime directories meet
pairing's private-owner rule. An existing `system` directory must already be
owned by the runtime user with mode 0700; unsafe directories are refused, not
repaired, and startup checks this even with no roster, before issuing an
unusable enrollment QR. Apple VirtioFS can briefly report cached guest-root
ownership for a fresh private mount, so the entrypoint checks access before
seeding and waits up to five seconds; it never changes ownership or widens
permissions. Managed image preflight requires this entrypoint.

### Custody (managed desktop and installer)

Managed desktop creation and `scripts/install.sh` share
`scripts/prepare-container-keyring.py` (embedded in the desktop binary; host
Python 3.9+). For a fresh runtime it creates per-root custody under
`~/.magician-container-keyrings`, outside the workspace: a private state
directory, a random private unlock file and a root-bound manifest. Recreation
reuses those exact files. The unlock file is mounted read-only and never enters
arguments, environment or returned JSON. Missing, altered, unsafe or incomplete
custody is refused without regeneration. `MAGICIAN_CONTAINER_KEYRING_HOME`
(host-only; export it consistently to installer and desktop) selects another
location.

- **macOS migration:** first managed use of an existing runtime copies the
  Magician master key, app-data keyring and matching paired-device generation
  anchor from Keychain into the Linux Secret Service over container stdin, and
  verifies every value before marking migration complete. An interrupted or
  unavailable migration stays pending and the service does not start; no
  replacement key is generated. Migration from other host credential stores is
  not implemented.
- **Windows:** the native desktop app provides the same contract (the Unix
  provisioner cannot enforce NTFS permissions): one root-bound bundle under
  `%LOCALAPPDATA%\Magican\container-keyrings`, inherited access removed, full
  control only for the signed-in user and SYSTEM. Missing, changed, linked,
  partial or root-mismatched custody stops setup. Windows uses Docker Desktop;
  setup can install it via `winget`, but WSL, virtualization, license and
  restart prompts are hard prerequisites.

### Replacement guards

Before removing an existing container, installer and desktop
update/recreation paths inspect its actual mounts and keyring environment and
run a read-only image access probe (1 CPU / 512 MiB). Unknown inspection output,
unmanaged/extra mounts, extra Docker named volumes/tmpfs and duplicate mount
destinations stop replacement; an altered unlock secret is refused before the
running container is stopped. Ordinary stop/start preserves the instance. Docker
uses the non-root host UID/GID for private bind mounts, with read-only account
maps derived from the candidate image so D-Bus can resolve that UID; the host's
account database is never copied.

### Building and testing

Add the keyring packages and current launcher to an existing image without
rebuilding services or UI:

```sh
make build-container-keyring-image \
  CONTAINER_KEYRING_BASE=magician:your-verified-image \
  CONTAINER_KEYRING_IMAGE=magician:keyring-ready
```

`CONTAINER_KEYRING_ENGINE=docker` for Docker (default Apple Container). It only
prepares an image. On Apple Container, keep an existing stopped builder's
CPU/memory configuration — changing it recreates the builder and discards its
cache.

- `make test-container-keyring-provisioning` — custody loss/isolation and
  replacement guards, no containers.
- `make test-linux-keyring` — private inputs on macOS; on non-root Linux also the
  real Secret Service (session reopen, wrong-secret refusal, private app dirs,
  exit status, ordered shutdown).
- Opt-in full adapter lane (desktop adapter + packaged backend):

```sh
CARGO_BUILD_JOBS=1 RUSTC_JOB_GATE_SLOTS=1 \
TMPDIR=/Volumes/SSD1/magician/tmp make test-desktop-managed-container \
  MANAGED_CONTAINER_TEST_ROOT=/Volumes/SSD1/magician/managed-container-check-1 \
  MANAGED_CONTAINER_TEST_IMAGE=magician:keyring-ready
```

The root must be new, the image local, and Apple Container plus host forwarding
configured. It starts a UUID-named 1-CPU / 3-GiB container with no published
ports or provider credentials, enrolls synthetic Android/iOS/ESP clients via the
[mobile protocol lane](container-integration-harness.md#mobile-pairing-protocol-lane),
recreates through the desktop adapter, checks retained credentials and
revoked-token rejection, and verifies wrong-secret refusal. It pins only the
fixture's mandatory `query_analysis` route to the packaged host-Ollama profile
and retains the stopped fixture, custody and `acceptance.json`. It does not run
the composed installer, replace a deployment, or count as physical-device
acceptance.

## Architecture

```mermaid
flowchart LR
  subgraph Host[User's computer]
    Tray[Tauri desktop and native permissions]
    Chrome[Chrome and Magicutor extension]
    Root[Runtime data directory]
  end
  subgraph Linux[Linux container]
    Supervisor[magic-supervisor]
    Engine[magician]
    Bridge[magicutor]
    Skills[Linux skill executables]
    Data[/data]
    Supervisor --> Engine
    Supervisor --> Bridge
    Engine --> Skills
    Skills --> Bridge
  end
  Tray -->|API and health| Engine
  Engine -->|host automation and speech relay| Tray
  Chrome <-->|native bridge| Bridge
  Root <-->|bind mount| Data
```

The desktop manages Apple Container and Docker, seeds the runtime root, and
supplies the host gateway URL. Desktop service controls target the selected
managed container: Magician and Magicutor commands run inside it, and supervisor
restart preserves the container, mounts and writable layer.
`HostAutomationProvider` relays screen capture, AppleScript and Accessibility
requests to Tauri; native macOS permissions stay with that app. The default
browser path is the patched `agent-browser` inside Linux, Magicutor's bridge,
and the user's host extension. First-boot materialization guarantees only the
browser skill; other capability installs must be checked explicitly.

Two deployment shapes:

1. **Container on the user's computer:** the bind mount shares the selected
   runtime root as `/data`; host-service aliases and port forwarding connect it
   to the desktop and extension.
2. **Container on a separate Linux host:** a bind mount there cannot expose local
   files, and remote-engine configuration alone provides no local file transport
   or reverse connection for computer use. This needs a paired, authenticated
   outbound desktop session, per-device capability grants, explicit host file
   access (selected directory grants, bounded reads/writes, never interpreting a
   host absolute path as a container path), cancellation/timeouts and
   reconnect. Not implemented.

**The host gateway boundary:** the Tauri gateway binds **exactly
`127.0.0.1:3017`** and rejects non-loopback peers; typed macOS Apps bind their
owner proof to that listener identity. Container calls reach it through the
managed private exec relay, not host-alias forwarding. DNS resolution or host
`/health` does not prove a container can call native routes. Changing the
listener to `0.0.0.0` is not the remote design. `setup-container-runtime.sh
--check` rejects Apple DNS entries without localhost forwarding and returns
nonzero on failed readiness; configuration readiness alone does not prove the
desktop admits container requests.

## Image build constraints

For repeated local builds use the
[prebuild and OCI packaging flow](container-local-oci.md)
(`make prepare-container-image`, `make container-oci`). Constraints the main
Dockerfile encodes:

- **Toolchain:** `rust:1.92.0-bookworm`, matching the development compiler and
  the runtime's glibc family. C/C++, OpenSSL, Clang, CMake, protoc and
  `libprotobuf-dev` (Lance imports standard Protobuf schemas) are explicit.
- **Selection:** `make build-container-runtime` builds `magician-bin`,
  `magicutor` and `magic-supervisor` with the lockfile enforced. BuildKit cache
  mounts keep the Cargo registry, git sources and per-arch target dir, so a
  failed build reuses completed work.
- **Memory:** image Cargo builds default to one job (native CMake follows), and
  cross-crate release LTO is off (`CARGO_PROFILE_RELEASE_LTO=off` also disables
  in-crate ThinLTO). The container Make target uses 16 codegen units for
  `magician`, `magician-api` and `magician-bin` (overrides:
  `CONTAINER_MAGICIAN_CODEGEN_UNITS`, `CONTAINER_API_CODEGEN_UNITS`,
  `CONTAINER_BIN_CODEGEN_UNITS`) because a single unit exceeds a 12 GiB builder.
  Dependency caches do not bound compiler RAM. UI, backend Rust and browser Rust
  build sequentially per platform; the UI gets a 3 GiB Node heap for Vite.
  `--build-arg CARGO_BUILD_JOBS=N` and `CARGO_PROFILE_RELEASE_LTO=thin` override.
- **Build context:** includes the production Apps component contract (an
  `include_bytes!` input under `docs/`) and the desktop source files used in the
  backend's macOS/Android owner digests (compile-time inputs even on Linux).
  Apple Container needs explicit parent-directory exceptions for the vendored
  browser patch. Build from the canonical path (`/Volumes/SSD1`); a differently
  cased context path yields an empty transfer on Apple Container 1.0.0.
- **UI stage:** keeps `ui/unified-ui` and `sdk/typescript` as siblings and
  installs both lockfiles; the UI imports SDK source directly.
- **Config:** image and runtime root both get `llm-router.yaml`; entrypoint,
  installer and root seeder fill a missing sibling even when
  `magician-config.yaml` exists, preserving operator files. The entrypoint
  refuses to start with missing or empty config or router files.
- **Networking:** the image sets `MAGICIAN_HTTP_HOST=0.0.0.0` and
  `MAGICIAN_FRONTEND_DIR=/app/ui` (forwarded by the supervisor as CLI args) so
  port forwarding reaches Magician and the UI is served with the API. Native
  startup keeps loopback/API-only defaults.
- **Python:** uv's managed Python lives under `/opt/uv/python` (world-traversable),
  not `/root`, so the non-root user can use the skill venv; the image checks
  `import yaml` as that user because browser materialization needs it before
  supervisor startup.

## Resource policy for qualification

- `make test-container-tooling` — shell fixtures with fake runtimes, supervisor
  and network; no build, daemon, provider call or restart. Safe concurrently;
  tooling evidence only.
- `make qualify-container-integration` — the
  [host/browser/skill harness](container-integration-harness.md), attaching to
  an existing test container.
- Before a real image build, check for other Cargo/rustc, Xcode and image
  builds. Keep source snapshots, host target, compiler temp and container
  image/build storage on `/Volumes/build/magician/`. The host rustc gate does
  **not** constrain a compiler in a VM; bound the VM's CPUs/memory separately
  and build one platform at a time.
- Use a new empty test root, a unique container name and ports `13002`/`13003`.
  Never attach the live runtime root to a second supervisor — separate ports do
  not prevent concurrent writers. `qualify-container-e2e-live` stops the native
  stack.

## Entry points

- Original architecture
- Linux tools and host audit ·
  Apple Container build runbook
- [Desktop container and gateway implementation](../desktop/README.md)
- `make test-container-tooling`: lightweight regression lane
- `make qualify-container-e2e`: disposable real-container lane; **builds by default**
- `bash scripts/qualify-container-e2e.sh --skip-build --skip-runtime-setup --runtime docker --image <tested-image> --report-dir <evidence-dir>`: qualify an existing image without installation/build, still starts a disposable container
