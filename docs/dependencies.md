# Project-wide Dependency Inventory

This page is the dependency map for the whole repository. It covers every
source-controlled package manifest and resolution file currently used by the
Rust, JavaScript, Swift, Android, ESP-IDF and Python build graphs.

The linked manifests are authoritative for direct dependencies. A linked lock
or `Package.resolved` file is authoritative for the transitive versions in
that install domain. The [GitHub dependency
graph](https://github.com/MagicBeansAI/MagicianAI/network/dependencies) is a
convenient hosted view of the formats GitHub recognizes, but this page also
covers repository surfaces that graph may not resolve.

For first-party package numbers, see [Project Versions](project-versions.md).
Host prerequisites, model downloads and external services are installation
requirements rather than source-package dependencies; those live in the
[Quick Start](quickstart.md) and [API-key setup](setup/api-keys.md).

## Coverage at a glance

| Ecosystem | Declared dependency surfaces | Committed resolution state |
| --- | --- | --- |
| Rust / Cargo | 30 packages across 31 `Cargo.toml` files, including the workspace root | No committed `Cargo.lock`; the workspace, desktop and Kindle locks are gitignored |
| JavaScript | 22 `package.json` files | Six committed npm/pnpm locks; the Research Planner example has no committed lock |
| Swift | Native macOS Swift package and Magios XcodeGen/Xcode project | Native macOS has a committed `Package.resolved`; Magios resolution is under an ignored `.xcworkspace` |
| Android / Gradle | Root plugin graph plus `:app` and `:bridge` modules | Version constraints are committed; no Gradle dependency lock or verification metadata is committed |
| ESP-IDF | Four component manifests | `dependencies.lock` is generated locally and gitignored |
| Python | One generated aggregate requirements file with 15 requirements | No resolved, hash-locked Python lockfile; two requirements are exact pins |
| Containers and toolchains | Docker build stages, container install script, Node selectors and Gradle wrapper | Image bases and several tools are pinned; host prerequisites are capability-probed rather than globally locked |

## Rust / Cargo

The root [`Cargo.toml`](../Cargo.toml) declares the 30-member workspace and its
shared dependency constraints. Every member manifest, version and source link
is listed in [Project Versions](project-versions.md#rust-crates). The two
separate Cargo resolution domains are:

- [`desktop/src-tauri/Cargo.toml`](../desktop/src-tauri/Cargo.toml), which also
  has one path dependency on `runtime-core`.
- [`kindle/Cargo.toml`](../kindle/Cargo.toml).

`cargo metadata --no-deps` reports 580 direct dependency edges in the root
workspace, including 72 local path edges, plus 41 edges in the desktop crate
and two in the Kindle crate. Cargo's resolved lockfiles exist after local
resolution as `Cargo.lock`, `desktop/src-tauri/Cargo.lock` and
`kindle/Cargo.lock`, but the repository-wide `Cargo.lock` ignore rule means
none is committed.

Inspect the complete local resolution with:

```bash
cargo tree --workspace --all-features
cargo tree --manifest-path desktop/src-tauri/Cargo.toml --all-features
cargo tree --manifest-path kindle/Cargo.toml --all-features
```

## JavaScript / TypeScript

Across the 22 package manifests there are 52 production and 36 development
dependency edges before workspace deduplication. These are the independent
install and lock domains:

| Install domain | Manifest | Resolution |
| --- | --- | --- |
| Desktop shell | [`desktop/package.json`](../desktop/package.json) and [`pnpm-workspace.yaml`](../desktop/pnpm-workspace.yaml) | [`desktop/pnpm-lock.yaml`](../desktop/pnpm-lock.yaml) |
| Research Planner example | [`examples/reference-apps/research-planner/package.json`](../examples/reference-apps/research-planner/package.json) | Not committed |
| MCP conformance runner | [`scripts/mcp-conformance/package.json`](../scripts/mcp-conformance/package.json) | [`package-lock.json`](../scripts/mcp-conformance/package-lock.json) |
| TypeScript Apps SDK | [`sdk/typescript/package.json`](../sdk/typescript/package.json) | [`package-lock.json`](../sdk/typescript/package-lock.json) |
| Skillshub workspace | [`skillshub/package.json`](../skillshub/package.json) | [`skillshub/package-lock.json`](../skillshub/package-lock.json) |
| Browser skill standalone install | [`skillshub/browser/package.json`](../skillshub/browser/package.json) | [`skillshub/browser/package-lock.json`](../skillshub/browser/package-lock.json) |
| Unified UI | [`ui/unified-ui/package.json`](../ui/unified-ui/package.json) | [`ui/unified-ui/package-lock.json`](../ui/unified-ui/package-lock.json) |

The Skillshub lock covers its root plus 15 declared npm workspaces. Their
manifests are grouped here so no package is hidden behind the workspace root:

- Core tools: [`rg`](../skillshub/rg/package.json),
  [`dugite`](../skillshub/dugite/package.json), and the separately locked
  [`browser`](../skillshub/browser/package.json).
- Media and search: [`analyze-image-via-minimax`](../skillshub/analyze-image-via-minimax/package.json),
  [`image-generation-via-minimax`](../skillshub/image-generation-via-minimax/package.json),
  [`music-generation-via-minimax`](../skillshub/music-generation-via-minimax/package.json),
  [`video-generation-via-minimax`](../skillshub/video-generation-via-minimax/package.json),
  and [`web-search-via-minimax`](../skillshub/web-search-via-minimax/package.json).
- Bots: the [`bots` workspace](../skillshub/bots/package.json),
  [`bot SDK`](../skillshub/bots/sdk/package.json),
  [`AgentMail`](../skillshub/bots/agentmail/package.json),
  [`Gmail`](../skillshub/bots/gmail/package.json),
  [`Kapso`](../skillshub/bots/kapso/package.json),
  [`Telegram`](../skillshub/bots/telegram/package.json),
  [`Telegram self`](../skillshub/bots/telegram-self/package.json), and
  [`WhatsApp`](../skillshub/bots/whatsapp/package.json).

Use `npm ls --all` inside an npm install domain, or
`pnpm --dir desktop list --depth Infinity` for the desktop domain, to expand a
lock into its installed dependency tree.

## Swift and Apple clients

The native macOS audio engine declares three exact top-level Swift packages in
[`Package.swift`](../native/macos-audio-engine/Package.swift). Its committed
[`Package.resolved`](../native/macos-audio-engine/Package.resolved) records 30
resolved packages including transitive dependencies.

Magios declares MarkdownUI from `2.4.0` in the canonical XcodeGen
[`project.yml`](../magios/project.yml); the checked-in generated
[`project.pbxproj`](../magios/Magios.xcodeproj/project.pbxproj) carries the same
constraint. Xcode writes the resolved package under the project workspace, but
the repository ignores `*.xcworkspace`, so that resolution is not committed.

Inspect SwiftPM resolution with:

```bash
swift package show-dependencies --package-path native/macos-audio-engine
```

## Android / Gradle

- [`magdroid/android/build.gradle.kts`](../magdroid/android/build.gradle.kts)
  owns Android, Kotlin, Compose compiler and Google Services build plugins.
- [`magdroid/android/app/build.gradle.kts`](../magdroid/android/app/build.gradle.kts)
  owns the application, Compose, Firebase, lifecycle, work, security and QR
  dependencies.
- [`magdroid/android/bridge/build.gradle.kts`](../magdroid/android/bridge/build.gradle.kts)
  owns connection, Ktor, automation, wake-word and Play Integrity dependencies.
- [`settings.gradle.kts`](../magdroid/android/settings.gradle.kts) fixes the
  plugin and artifact repositories; [`gradle-wrapper.properties`](../magdroid/android/gradle/wrapper/gradle-wrapper.properties)
  fixes the Gradle distribution.

There is no committed Gradle dependency lock or dependency-verification
metadata. Expand the resolved configurations locally from `magdroid/android`:

```bash
./gradlew :app:dependencies :bridge:dependencies
```

## ESP-IDF

The firmware dependency graph is split across the application and its checked-in
component copies:

- [`magesp/CMakeLists.txt`](../magesp/CMakeLists.txt) owns the ESP-IDF project
  and firmware version, while
  [`magesp/main/CMakeLists.txt`](../magesp/main/CMakeLists.txt) owns the local
  component links and built-in IDF requirements.
- [`magesp/main/idf_component.yml`](../magesp/main/idf_component.yml)
- [`esp32_c6_touch_amoled_1_8/idf_component.yml`](../magesp/components/esp32_c6_touch_amoled_1_8/idf_component.yml)
- [`esp_lcd_touch_ft5x06/idf_component.yml`](../magesp/components/esp_lcd_touch_ft5x06/idf_component.yml)
- [`FT5x06 test-app manifest`](../magesp/components/esp_lcd_touch_ft5x06/test_apps/main/idf_component.yml)

The project deliberately overrides the upstream FT5x06 component with its
local patched copy. ESP-IDF generates `magesp/dependencies.lock`, but that file
and downloaded `managed_components/` tree are gitignored.

## Python

[`skillshub/requirements.txt`](../skillshub/requirements.txt) is generated from
the `metadata.magician.requires.python_packages` frontmatter in shipped skills.
It is the aggregate declaration for 15 Python requirements installed into the
Skillshub virtual environment. `cloakbrowser[geoip]` and `maigret` are exact
pins; the remaining requirements currently float, and there is no committed
hash-locked transitive resolution.

## Containers and toolchains

The root [`Dockerfile`](../Dockerfile) is a dependency surface in its own right.
It pins Rust `1.92` on Debian Bookworm for the main builder and the patched
agent-browser build, Node `20` on Alpine for the UI, Go `1.23` on Debian
Bookworm for Go-built tools, the
agent-browser upstream tag `v0.38.1`, and Debian Bookworm Slim for the runtime.
[`scripts/install-container-tools.sh`](../scripts/install-container-tools.sh)
owns the runtime OS packages and downloaded command-line tools layered onto
that base.

For host and app toolchains:

- [`skillshub/.nvmrc`](../skillshub/.nvmrc) and
  [`skillshub/.node-version`](../skillshub/.node-version) both select Node
  `24.20.0`; individual JavaScript roots also declare their package-manager
  versions in `package.json` where required. A Node **major** is also a
  native-addon ABI (`process.versions.modules`), and the lockfile does not
  move when `skillshub/.node/` is swapped, so `make -C skillshub setup-deps`
  keeps `node_modules/.node-abi` and asks the runtime — via
  [`skillshub/scripts/native_addons_needing_rebuild.mjs`](../skillshub/scripts/native_addons_needing_rebuild.mjs),
  a `process.dlopen` of every installed `*.node` — which addons no longer
  load, rebuilding exactly those. Bump order is `.node-version` →
  `make -C skillshub setup-node` → `make -C skillshub setup-deps`; skipping the last step is how the
  WhatsApp bot's `better-sqlite3` (compiled for Node 22 / ABI 127) ran under
  Node 24 / ABI 137 for three weeks and failed every database call with
  `ERR_DLOPEN_FAILED` (2026-09-17).
- [`gradle-wrapper.properties`](../magdroid/android/gradle/wrapper/gradle-wrapper.properties)
  selects Gradle `8.13` for Magdroid.
- [`scripts/install-prerequisites.sh`](../scripts/install-prerequisites.sh)
  probes and installs host prerequisites. It does not define one globally
  locked host-toolchain snapshot.

## Reproducibility boundaries found in this review

The repository has strong committed resolution for the main Node install
domains and the native macOS Swift package. Complete, checkout-reproducible
transitive versions are not yet committed for Rust, Magios SwiftPM, Android,
ESP-IDF or Python. The Research Planner example also lacks a committed
JavaScript lock. Those are explicit facts about the current
tree, not claims that those package managers lack local resolution.
