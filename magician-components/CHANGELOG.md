# Changelog

## [Unreleased]

- The macOS native-services host profile adds ripgrep (Homebrew `ripgrep`).
- 0.1.10: Setup can move this computer's Cloudflare tunnel to a different zone when the step runs again.

- 0.1.9: Guide owner identity claims and automatic signed webhook setup across installers.

- New `workspace-project-access` setup step (`make setup-gws-project-access`) checks and grants Google Cloud project roles; `workspace-oauth` no longer asks for a Pub/Sub topic.

### 2026-10-21 — 0.1.8

- The notes viewer component replaces the SilverBullet one.

### 2026-10-15 — 0.1.7

- Optional Cloudflare and Clarity setup entries; HTTPS health checks against a configured origin refuse redirects.

### 2026-10-15 — 0.1.6

- Share source-install stages and separate software/data paths across setup frontends, with read-only Desktop permission observations.
