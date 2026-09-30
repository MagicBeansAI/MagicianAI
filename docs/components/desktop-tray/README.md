# Magician Desktop Tray

Docs for the Tauri tray app that hosts the unified-ui as a menu-bar
application + Jarvis HUD overlay.

Runtime presentation copy is sourced through generated Rust and Svelte
constants from `data/presentation_identity.json`; bundle metadata remains the
compile-time OS identity. `make presentation-identity-check` prevents migrated
tray consumers from returning to handwritten product-name fallbacks.

## Canonical references

- [Desktop Changelog](../../../desktop/CHANGELOG.md)
- [Process-owned Mac Notch Orb](../desktop/mac-notch-orb.md)
- Tauri ↔ Unified-UI Consolidation Plan

## Pages

- [macOS media permissions](macos-media-permissions.md) — Info.plist /
  entitlements / build.rs / patch script plumbing that exposes mic +
  camera APIs to the WKWebView. Required for the HUD's voice-note
  recorder + realtime call buttons.

## Ambient orb ownership

There is no mascot subprocess; the Tauri process
owns the ambient lifecycle, native wake detector, hands-free session claim,
notch-aware NSPanel, WebGL renderer, and controls. The tray mirrors status,
summon/Talk, pause/resume, and disarm/re-arm actions. App and Settings remain
top-level tray destinations instead of being duplicated inside the Orb. A high-
salience wake may arrive at screen center and settle to the notch; the orb does
not follow draw-overlay shapes or roam. Ambient Orb is the only user-facing
desktop voice entry: no Dictation/Live tray item, Voice submenu, or host-wide
PTT trigger. See the canonical orb contract above.

## Voice Output Ownership

Recorded Orb Dictation turns are submitted by the tray, but Magician still owns
STT, chat insertion, and provider TTS. When the response includes
`assistant_speech_segments`, the tray asks Magician's
`/media/tts/synthesize_message` endpoint for WAV segments and plays them
through the native CPAL output path. The request omits a concrete provider, so
Magician resolves the scoped Dictation profile and TTS stage. Web Settings
edits the canonical surface profile and stage options using only the verified
backend `/media/providers` inventory; those choices are not mirrored into the
desktop TOML. Providers still need their configured credentials before Magician
advertises them, and local macOS TTS appears only when the host speech verifier
passes. The
tray suppresses native reply speech when **Mute Assistant Audio** is enabled, realtime output
is active, or an active web/HUD media session advertises browser/provider TTS,
avoiding duplicate playback while transcript and lifecycle feedback remain available.
