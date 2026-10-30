# Install Manifest And Cleanup

## Purpose

Magician Desktop records what setup provisioned so uninstall and cleanup remove
only Magician-owned artifacts. The design goal is ownership-aware cleanup:
never remove user tooling that existed before Desktop setup ran.

## Manifest

The install manifest is written after a successful first-run setup and stored
outside the scoped V3 runtime data tree so data-only cleanup can preserve
ownership metadata.

It captures:

- install timestamp, platform, and selected container runtime
- artifacts installed by Magician
- pre-existing state observed before setup
- data directories created by setup

The manifest is the source of truth for both the desktop-side cleanup flow and
the standalone `scripts/cleanup.sh` path.

## Setup Flow

Setup is manifest-aware from the start:

1. Snapshot pre-existing tools and runtime availability before provisioning.
2. Run the normal setup sequence.
3. Persist the manifest only after setup completes successfully.

This avoids recording intent that never actually landed and keeps cleanup tied
to real ownership.

## Cleanup Modes

- `tools-and-data`: remove container resources, Magician-installed tools,
  launch-at-login artifacts, and data directories.
- `only-tools`: remove runtime and container assets while preserving user data.
- `only-data`: remove data directories only.

Order: a service is stopped or unregistered before its data is removed. Full
cleanup runs container stop/remove, image, launch agent, installed tools (a
native install unregisters Desktop's backend service, clears its
`launchctl disable` override and removes its prefix), the desktop
`desktop-app.quit` marker, the data directories, then the manifest
(`only-tools` stops after the marker). The plan per mode is `cleanup_plan` in
`desktop/src-tauri/src/cleanup.rs`.

`only-data` on a native install whose service Desktop registered stops the
service, removes the data, re-seeds the root the service uses (read from its
LaunchAgent, systemd unit or logon-task launcher) and restarts it, because the
native launcher does not seed a missing `magician-config.yaml`:

- Re-seeding happens only when this run actually deleted that root or a
  directory containing it. A dangling root symlink gets its target directory
  back (the link is kept); data folders are removed at their resolved target,
  as `scripts/cleanup.sh` does.
- Seeding copies only what is missing: the packaged `magician-config.yaml` and
  `llm-router.yaml`, and from `<prefix>/share/seed` `decision-engine.yaml`,
  `.env` (from `.env.example`), `operator-config.yaml` and `logs/`. Never
  `.env.development`: both env loaders read it first and never overwrite, so
  the example's empty `KEY=` lines would shadow the owner's keys.
- A failed stop, seed or restart is reported in the summary; after a seed
  failure the `launchctl disable` override is still cleared and the service
  still starts when a `magician-config.yaml` exists.
- A service that predates Desktop setup is never touched; the summary warns
  that its data was removed under it.

## Safety Rules

- Cleanup is manifest-driven. If the manifest says a runtime or dependency was
  pre-existing, cleanup keeps it.
- Path deletion is constrained to validated user/app-support locations: a
  data folder must resolve strictly inside the home folder, so the home folder
  itself (or an ancestor of it) is refused, like `scripts/cleanup.sh`'s
  `"$HOME"/*` rule.
- Homebrew is never auto-removed, even if Desktop installed it, because it is a
  shared system dependency with too much blast radius.
- The Tauri cleanup module and `scripts/cleanup.sh` use the same ownership model
  and order. The script unregisters Desktop's `ai.magicbeans.magican.backend`
  LaunchAgent (bootout, remove, `launchctl enable`) or
  `magician-backend.service` user unit, and removes the prefix only when it
  matches `MAGICIAN_PREFIX` / `~/.magician` and holds `MANIFEST.yaml`.
  `--dry-run` shows every step (`scripts/test_cleanup_sh.py` checks the order).

## Container Tool Baseline

Container capability tools are installed from a single in-image script so the
runtime image and cleanup expectations stay aligned. The container image remains
the canonical place for bundled CLI dependencies used by capability execution.
