# Project Versions

Magican is a multi-package project. Its components are versioned independently;
there is no workspace-wide version that can accurately stand in for every
crate, app and SDK.

## Magican version

The version displayed by the root README badge is the `[package].version` of
the [`magician` crate](../magician/Cargo.toml): **`0.7.147`**.

That choice is deliberate. [`magician-bin`](../magician-bin/Cargo.toml) builds
the executable named `magician`, but it has its own package version. The badge
must continue to follow the `magician` crate rather than `magician-bin`, a Git
tag, a GitHub release or one of the client applications.

> [!WARNING]
> All versions below are pre-v1 development versions. Compatibility is not
> implied across independently versioned components.

## Rust crates

The root [Cargo workspace](../Cargo.toml) currently contains 32 crates.

| Crate | Version |
| --- | ---: |
| [`decision-engine`](../decision-engine/Cargo.toml) | `0.4.5` |
| [`decision-engine-contract`](../decision-engine-contract/Cargo.toml) | `0.4.2` |
| [`document-to-markdown-cli`](../document-to-markdown-cli/Cargo.toml) | `0.1.0` |
| [`magic-supervisor`](../magic-supervisor/Cargo.toml) | `0.1.9` |
| [`magician`](../magician/Cargo.toml) | `0.7.147` |
| [`magician-alignment`](../magician-alignment/Cargo.toml) | `0.1.0` |
| [`magician-api`](../magician-api/Cargo.toml) | `0.3.68` |
| [`magician-app-contract`](../magician-app-contract/Cargo.toml) | `0.3.2` |
| [`magician-apps`](../magician-apps/Cargo.toml) | `0.2.8` |
| [`magician-bin`](../magician-bin/Cargo.toml) | `0.2.40` |
| [`magician-chunking`](../magician-chunking/Cargo.toml) | `0.1.1` |
| [`magician-comms`](../magician-comms/Cargo.toml) | `0.1.12` |
| [`magician-components`](../magician-components/Cargo.toml) | `0.1.9` |
| [`magician-core`](../magician-core/Cargo.toml) | `0.1.5` |
| [`magician-decision`](../magician-decision/Cargo.toml) | `0.4.9` |
| [`magician-event-taxonomy`](../magician-event-taxonomy/Cargo.toml) | `0.1.10` |
| [`magician-learning`](../magician-learning/Cargo.toml) | `0.1.7` |
| [`magician-mcp-client`](../magician-mcp-client/Cargo.toml) | `0.1.26` |
| [`magician-media`](../magician-media/Cargo.toml) | `0.1.8` |
| [`magician-pty`](../magician-pty/Cargo.toml) | `0.1.0` |
| [`magician-relentless`](../magician-relentless/Cargo.toml) | `0.1.1` |
| [`magician-setup`](../magician-setup/Cargo.toml) | `0.1.3` |
| [`magician-storage`](../magician-storage/Cargo.toml) | `0.1.0` |
| [`magician-storage-gate1`](../magician-storage-gate1/Cargo.toml) | `0.1.0` |
| [`magician-storage-migration`](../magician-storage-migration/Cargo.toml) | `0.1.0` |
| [`magician-storage-s3`](../magician-storage-s3/Cargo.toml) | `0.1.0` |
| [`magician-storage-state`](../magician-storage-state/Cargo.toml) | `0.1.0` |
| [`magician-surfaces`](../magician-surfaces/Cargo.toml) | `0.1.3` |
| [`magician-vector-index`](../magician-vector-index/Cargo.toml) | `0.1.16` |
| [`magicllm`](../magicllm/Cargo.toml) | `0.2.51` |
| [`magicutor`](../magicutor/Cargo.toml) | `0.1.92` |
| [`runtime-core`](../runtime-core/Cargo.toml) | `0.1.9` |

Extracted first-party Git dependencies are pinned in the root manifest and lock:

| Repository/package | Version |
| --- | ---: |
| [MagicRun / tool-runtime-core](https://github.com/MagicBeansAI/MagicRun/blob/e9a4fdc4d028c5d3e0d2041e4a55bff26edc1bd4/tool-runtime-core/Cargo.toml) | `0.1.82` |
| [MagicVault / magicvault-core](https://github.com/MagicBeansAI/MagicVault/blob/4902803b35211ea2a1dc7e6bdb112cb9da03000f/magicvault-core/Cargo.toml) | `0.1.6` |
| [MagicVault / magicvault-primitives](https://github.com/MagicBeansAI/MagicVault/blob/4902803b35211ea2a1dc7e6bdb112cb9da03000f/magicvault-primitives/Cargo.toml) | `0.1.2` |
| [MagicVault / magicvault-effect](https://github.com/MagicBeansAI/MagicVault/blob/4902803b35211ea2a1dc7e6bdb112cb9da03000f/magicvault-effect/Cargo.toml) | `0.7.1` |
| [MagicVault / magicvault-protocol](https://github.com/MagicBeansAI/MagicVault/blob/4902803b35211ea2a1dc7e6bdb112cb9da03000f/magicvault-protocol/Cargo.toml) | `0.7.0` |

These revisions are pinned directly in the root manifest; the workspace has no
local `[patch]` override for them.

Two first-party Rust crates are intentionally outside the root workspace:

| Standalone crate | Version | Why standalone |
| --- | ---: | --- |
| [`magician-desktop`](../desktop/src-tauri/Cargo.toml) | `0.3.24` | Tauri desktop build graph and lock domain |
| [`magkindle`](../kindle/Cargo.toml) | `0.1.0` | Kindle thin-client build graph |

## Other versioned surfaces

These are release-bearing applications and JavaScript packages, not Rust
crates. Private packages without a `version` field are still covered by the
[dependency inventory](dependencies.md).

| Surface or package | Version | Source of truth |
| --- | ---: | --- |
| Desktop application | `0.3.24` | [`tauri.conf.json`](../desktop/src-tauri/tauri.conf.json) |
| Desktop JavaScript shell | `0.3.24` | [`desktop/package.json`](../desktop/package.json) |
| Container OCI metadata label | `0.1.0` | [`Dockerfile`](../Dockerfile) |
| Unified UI | `0.1.69` | [`ui/unified-ui/package.json`](../ui/unified-ui/package.json) |
| Magios | `0.4.13` (build `224`) | [`magios/project.yml`](../magios/project.yml) |
| Magdroid | `0.6.15` (code `42`) | [`magdroid/android/app/build.gradle.kts`](../magdroid/android/app/build.gradle.kts) |
| MagESP firmware | `1.9.3` | [`magesp/CMakeLists.txt`](../magesp/CMakeLists.txt) |
| TypeScript Apps SDK (`@magician/apps`) | `0.1.0-dev.3` | [`sdk/typescript/package.json`](../sdk/typescript/package.json) |
| Research Planner reference app | `0.1.1` | [`examples/reference-apps/research-planner/package.json`](../examples/reference-apps/research-planner/package.json) |
| MCP conformance runner | `1.0.0` | [`scripts/mcp-conformance/package.json`](../scripts/mcp-conformance/package.json) |
| AgentMail bot | `0.1.2` | [`skillshub/bots/agentmail/package.json`](../skillshub/bots/agentmail/package.json) |
| Gmail bot | `0.2.3` | [`skillshub/bots/gmail/package.json`](../skillshub/bots/gmail/package.json) |
| Kapso bot | `0.2.4` | [`skillshub/bots/kapso/package.json`](../skillshub/bots/kapso/package.json) |
| Bot SDK | `0.3.8` | [`skillshub/bots/sdk/package.json`](../skillshub/bots/sdk/package.json) |
| Telegram self bot | `0.2.2` | [`skillshub/bots/telegram-self/package.json`](../skillshub/bots/telegram-self/package.json) |
| Telegram bot | `0.2.4` | [`skillshub/bots/telegram/package.json`](../skillshub/bots/telegram/package.json) |
| WhatsApp bot | `0.2.4` | [`skillshub/bots/whatsapp/package.json`](../skillshub/bots/whatsapp/package.json) |

The container label is metadata on the current image recipe, not the Magican
version contract. It is listed because it is a version-bearing project
surface; the README badge still follows `magician/Cargo.toml` exclusively.

## Verify the current tree

Cargo is the authority for the Rust tables:

```bash
make --silent cargo ARGS="metadata --no-deps --format-version 1" \
  | jq -r '.packages | sort_by(.name)[] | "\(.name)\t\(.version)"'
```

Add `--manifest-path desktop/src-tauri/Cargo.toml` or
`--manifest-path kindle/Cargo.toml` inside `ARGS` for the standalone crates.

When the `magician` crate is bumped, update its row and the root README badge in
the same change. Other rows should move only with their own source manifests.
