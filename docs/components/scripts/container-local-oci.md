# Local prebuild and OCI packaging

Produce a standard single-platform OCI image locally, keeping the slow runtime
layers (Debian, skill dependencies, browsers, configs) separate from the fast
application layer (service binaries, UI, manifests). Application code is always
compiled on the host; BuildKit only builds dependency layers.

## One command on this Mac

Commit the reviewed source changes, then run:

```sh
make prepare-container-image-local
make prepare-container-image-local ARGS=--dry-run   # prints mode: adopt | artifacts | full; no writes
```

This Apple Container wrapper snapshots **committed HEAD** on SSD1, prepares and
imports a versioned image, verifies the OCI archive, and checks non-root
execution, service linkage, the UI index, Python imports and required Linux
skill binaries. It never copies working-tree edits, untracked files or the live
runtime root. `ARGS='--revision <commit>'` selects another commit. The
lower-level `prepare-container-image` target takes all Python options directly.

### Base reuse

- First run with no cached base: adopts the stable local
  `magician:qualified-runtime` alias (override with
  `CONTAINER_LOCAL_BASE_IMAGE=<reviewed-tag>`), or with no alias builds only the
  Dockerfile's dependency-only `runtime-base` target (dependency, skill,
  browser, config and seed layers) and caches it.
- Later runs reuse the matching verified base. Service/UI-only revisions
  cross-compile Linux artifacts on the Mac with Zig and persistent SSD caches,
  then package, verify and import — no compiler container or BuildKit VM.
- Runtime-layer inputs (non-manifest Skillshub inputs, packaged configs/data,
  helper scripts, the Dockerfile, `Cargo.lock`, unknown paths) are fingerprinted
  from Git objects; any change triggers a full base refresh (intentionally
  conservative). Declarative `SKILL.md` changes ride the artifact layer and do
  not refresh installed skill dependencies. A base refresh may compile
  Linux-only skill helpers in BuildKit, never the application.
- `--adopt-base-image` exports an already reviewed local runtime image; the
  wrapper verifies and smoke-tests it but cannot prove its runtime-layer sources
  match the commit, so adopt only qualified layers. An old base lacking
  writable-layer tool repairs is never silently adopted.
- `ARGS=--refresh-base` refreshes unchanged inputs (e.g. upstream packages);
  `ARGS=--force-rebuild-from-scratch` discards base reuse entirely.

### Outputs and resources

Outputs and numbered command logs go to
`/Volumes/SSD1/magician/image-preparation/releases/<revision>-<timestamp>/`:
`image.oci.tar`, digest receipt, `plan.json` and, only after success,
`result.json` (records the imported tag). Source snapshots, compile caches and
base archives stay beside `releases/`; the refreshed base and its cache entry
share storage through a hard link. **At least 20 GiB free** is required.

- Host cross-compile: two Cargo/native jobs by default, sharing the machine-wide
  rustc gate. Target, temp files, Cargo/npm downloads and Zig wrappers stay on
  SSD1.
- BuildKit: new builders get **2 CPUs / 12 GiB**; an existing stopped builder's
  values are passed explicitly, because Apple 1.0.0 resolves omitted flags from
  system defaults and can replace a differently configured builder (losing its
  cache). A running builder, or one above the cap, stops a base refresh before
  building. The wrapper stops only a builder it started; a work-directory lock
  rejects concurrent preparation runs.
- Apple 1.0.0 can close the build RPC before Dockerfile progress begins; the
  wrapper retries that exact transport failure twice. Dockerfile, dependency and
  compiler failures are not retried.
- The main Dockerfile must stay **≤ 14,500 bytes** (preflight-enforced): Apple
  Container 1.0 drops the build RPC for larger definitions without reporting an
  error. Keep extended rationale in this doc, not the Dockerfile.

This prepares an image only; it does **not** switch the desktop, replace the
running container, boot agents, or qualify host/browser/provider workflows. Boot
with isolated data and qualify before deployment.

Offline regressions: `make test-prepare-container-image` (also in
`make test-container-tooling`). Its synthetic repo names a real skill manifest
(`skillshub/browser/SKILL.md`), because `make test-tool-skills` requires every
`skillshub/` path referenced under `scripts/` to resolve.

## Individual pipeline steps

Packaging needs Python 3.10+ and an already qualified Debian/glibc runtime
image; no recompilation, dependency installation, browser download, BuildKit or
registry account.

```text
Qualified runtime image ── export once ── base.oci.tar ─────┐
                                                         ├─ package ─ final.oci.tar ─ load/run
Linux source + cached SDK ── compile ── artifacts.tar ─────┘
                           or capture from existing image
```

The base holds Debian, skill dependencies, Python/Node environments, browsers,
seed data, configs and startup scripts. The artifact bundle holds the three
Linux service executables, the built UI, runtime scripts and declarative
Skillshub manifests. Lifecycles:

- **New container:** reuse the final image.
- **New backend/UI version:** build Linux artifacts once, package with the same
  base (file I/O, hashing and gzip only).
- **Runtime dependencies, executable skill sources, browser patches, config
  seeds or startup scripts:** refresh and qualify the base via the main
  Dockerfile (or the tools-only repair below). The fast lane is not an
  arbitrary source-to-image replacement.
- **Rust/Node/compiler dependencies:** rebuild/version the SDK and use a
  separate matching artifact cache. SDK and runtime both use Debian Bookworm
  glibc.

A macOS executable cannot run in the image; the packager rejects Mach-O and
wrong-architecture ELF. The default lane uses `cargo-zigbuild` for Bookworm
glibc 2.36 ELF; the SDK container is an explicit fallback.

## Repairing a cached base's missing tool executables

`make build-container-tools-image` layers the document converter, Metabase CLI,
OfficeCLI and Higgsfield onto an existing runtime image. It compiles only the
standalone document adapter and generated Metabase CLI (one Cargo/Go job,
persistent caches) and installs pinned, checksum-verified OfficeCLI and
Higgsfield releases; no account state or provider calls. The main Dockerfile
includes the same steps for clean builds.

```sh
make build-container-tools-image CONTAINER_TOOLS_BASE=<base-tag> CONTAINER_TOOLS_IMAGE=magician:tools-ready
# For a Linux Docker builder, add CONTAINER_TOOLS_ENGINE=docker.
```

Run it from an isolated SSD1 source snapshot after other heavy builds finish.
Like the SDK/keyring/prebuilt targets it uses `scripts/build-container-layer.py`
to repeat an idle Apple builder's exact resource limits and stop it afterward;
builder-image or managed color-environment changes are refused because Apple
would recreate the VM. Resource/DNS overrides are rejected on Apple; let the
wrapper start the builder. After building, qualify the image, export it as the
new base, and resume normal OCI packaging.

Both recipes run `verify-container-skill-bins.py` as the final non-root user:
missing executables, wrong-architecture ELF, host Mach-O and missing
interpreters fail assembly. Mac-only skills are excluded; MCP definitions stay
uncalled. It proves executable presence, not working operations.
`make test-container-readiness` covers this gate, pinned artifact integrity and
Apple host-forwarding detection (needs PyYAML).

For already built and qualified executables,
`make build-container-prebuilt-tools-image` packages their exact bytes. Prepare
an isolated SSD1 context with `prebuilt-skill-tools/{agent-browser,
document-to-markdown,metabase-pp-cli,higgsfield,officecli,SHA256SUMS}` (the
checksum file lists all five), plus `containers/tools/prebuilt.Dockerfile`,
`scripts/verify-container-skill-bins.py`, and the CUA skill's `SKILL.md` and
`bin/macos-ui-controller`, preserving paths. No runtime data or credentials.

```sh
make build-container-prebuilt-tools-image \
  CONTAINER_PREBUILT_TOOLS_CONTEXT=/Volumes/SSD1/magician/prebuilt-tools/context \
  CONTAINER_TOOLS_BASE=magician:keyring-ready \
  CONTAINER_TOOLS_IMAGE=magician:tools-ready
```

It checks hashes, installs the tools and OfficeCLI links, and runs the
executable inventory — no compile, download, service start or builder resize.
Hashes prove byte identity, not trust or compatibility with a different base;
keep input provenance beside the context.

## Bootstrap from the working Apple Container image

Large outputs belong on SSD1 (canonical `/Volumes/SSD1` spelling). Commands
refuse to overwrite existing artifacts. The CLI is also
`python3 scripts/container-oci.py …`.

```sh
# One-time cached base. Saves an OCI image, not a container's writable /data.
make container-oci CONTAINER_OCI_ARGS='export --image <image> --arch arm64 --output /Volumes/SSD1/magician/local-oci/base.oci.tar'
# Reuse the already compiled application.
make container-oci CONTAINER_OCI_ARGS='capture --image <image> --arch arm64 --output /Volumes/SSD1/magician/local-oci/artifacts.tar'
# No container, compiler, dependency installation or network needed.
make container-oci CONTAINER_OCI_ARGS='package --base …/base.oci.tar --artifacts …/artifacts.tar --tag magician:local-oci-<rev> --arch arm64 --output …/magician-<rev>.oci.tar'
make container-oci CONTAINER_OCI_ARGS='verify …/magician-<rev>.oci.tar --arch arm64'
container image load --input …/magician-<rev>.oci.tar
```

`export` and `capture` target Apple Container. `capture` starts a temporary
2-CPU / 512 MiB container with `/bin/tar` as entrypoint (no mounts, ports,
credentials or services) and reads only `/app/magician.bin`,
`/app/magicutor.bin`, `/app/magic-supervisor` and `/app/ui`. A running Apple
Container service is needed for these two and for load/run, not for `bundle`,
`package` or `verify`.

The artifact is an
[OCI image layout](https://github.com/opencontainers/image-spec/blob/v1.1.1/image-layout.md)
in an uncompressed tar (content-addressed blobs, index, manifest, config, gzip
layers) — not a flattened rootfs or `docker save` archive. Other runtimes import
it with OCI-aware tools (e.g. `skopeo copy oci-archive:… docker-daemon:magician:local`).

Use a new tag/output per candidate and run it with **2 CPUs / 4 GiB**. The
pipeline never resizes, restarts or replaces an existing runtime; boot still
needs a prepared runtime root, provider credentials and the integration
harness's schedule isolation. See the
build runbook and
[integration harness](container-integration-harness.md).

## Producing a new Linux application bundle

Host cross toolchain, once:

```sh
brew install zig llvm
cargo install --locked cargo-zigbuild
rustup target add aarch64-unknown-linux-gnu
```

`prepare-container-image-local` uses `--artifact-engine zig` by default
(`--artifact-engine apple` selects the SDK container). Zig replaces the compiler
VM, not the Debian runtime layers.

Fallback SDK (Rust 1.92, Node 20, C/C++, Clang, CMake, protoc; compiles nothing
itself):

```sh
make build-container-sdk
make build-container-sdk CONTAINER_SDK_ENGINE=docker   # Linux Docker builder
```

It uses the same builder-preservation wrapper (new builders 2 CPUs / 12 GiB).
Version `CONTAINER_SDK_IMAGE` when toolchain inputs change (mutable upstream
tags mean rebuilds are not byte-identical), pass the matching `--sdk-image` to
`prebuild-container-artifacts`, and use a new cache directory per SDK tag. A
preserved small packaging VM is not necessarily big enough for Rust.

Build from an isolated SSD1 source snapshot, never the shared checkout
(`git archive <revision> | tar -x -C <empty-dir>`). Include required uncommitted
fixes explicitly and name the snapshot accurately with `--revision`. Reuse the
Linux-only directory for incremental builds; do not mix host deps or edit it
while building.

```sh
make prebuild-container-artifacts CONTAINER_ARTIFACT_ARGS='--engine zig --source /Volumes/SSD1/magician/linux-source --cache /Volumes/SSD1/magician/linux-cache-arm64 --output /Volumes/SSD1/magician/local-oci/artifacts-next.tar --revision my-reviewed-snapshot --dry-run'
# Inspect the printed plan, then run without --dry-run.
```

Zig mode runs Node and Cargo on macOS, sequentially, with two Cargo/native jobs;
Cargo, native jobs, temp files, Zig wrappers and npm downloads use the
persistent cache, and npm install is skipped when lockfile/tool versions are
unchanged. Compiler temp files live in a short `tmp/zig-<hash>` directory on the
cache filesystem to keep sccache's macOS Unix socket under the path limit
(`MAGICIAN_CONTAINER_BUILD_TMP_ROOT` overrides). `make build-container-runtime-zig`
owns the cross-compiled package selection (`make build-container-runtime` in the
SDK). Outputs are stripped copies. No automatic swap or retries.

`--engine native` suits a Linux machine with the same toolchain;
`--engine apple`/`docker` use the SDK. Native and Zig modes cap job counts but
have no VM RAM boundary. Cache/source locks reject concurrent writers; cache
metadata rejects another architecture or toolchain.

For binaries/UI built elsewhere, `bundle` directly and transfer the tar with its
JSON receipt:

```sh
make container-oci CONTAINER_OCI_ARGS='bundle --binaries /path/to/linux/release --ui /path/to/ui/build --arch arm64 --revision my-reviewed-snapshot --output /path/to/artifacts.tar'
```

AMD64 needs AMD64 artifacts and base (`--arch amd64`). Output is one platform,
not a multi-arch index. Artifacts with new system-library requirements need a
compatible refreshed base and linkage/boot checks.

## Integrity, space and verification

- Publishing is an atomic no-overwrite link with adjacent JSON receipts (SHA-256,
  sizes, platform, parent manifest, artifact identity). Packaging checks an
  artifact receipt's digest when present and never reuses the base's
  source-revision label for changed binaries. Labels/receipts are provenance
  hints, not signed attestations — never pass an inaccurate `--revision`.
- The packager preserves base env, user, ports, volumes, command and
  entrypoint; requires a non-root user; replaces service binaries; and uses an
  OCI opaque whiteout so removed UI assets do not survive. It rejects links,
  special files, duplicate paths, traversal and env files in the bundle.
- The base must be trusted: its layers remain, including superseded application
  bytes. Regular base refreshes avoid accumulating them.
- `verify` checks OCI structure, platform, metadata and every selected
  compressed blob's SHA-256; it does not unpack base layers or replace
  boot/skill tests.
- Packaging streams, but needs space for the artifact tar, a temporary
  uncompressed layer, its compressed copy and the final archive. Avoid
  packaging while SSD1 is near full.

`make test-container-oci` runs offline fixture tests without Cargo or
containers. Repackaging an existing image does not exercise the compile lane.
