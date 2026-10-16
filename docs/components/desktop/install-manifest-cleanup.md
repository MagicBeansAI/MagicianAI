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

## Safety Rules

- Cleanup is manifest-driven. If the manifest says a runtime or dependency was
  pre-existing, cleanup keeps it.
- Path deletion is constrained to validated user/app-support locations.
- Homebrew is never auto-removed, even if Desktop installed it, because it is a
  shared system dependency with too much blast radius.
- The Tauri cleanup module and `scripts/cleanup.sh` use the same ownership model.

## Container Tool Baseline

Container capability tools are installed from a single in-image script so the
runtime image and cleanup expectations stay aligned. The container image remains
the canonical place for bundled CLI dependencies used by capability execution.
