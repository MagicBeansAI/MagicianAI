# Changelog

All notable changes to the Magician Desktop project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

_Current development version: `0.3.30`._

---

## [Unreleased]

- The native backend package carries the pinned ripgrep (`rg.bin`; `rg.exe` is copied on Windows installs), and macOS native setup installs ripgrep as a host prerequisite.

### 2026-10-31 — 0.3.30

- Desktop voice talks in its own Desktop thread (`desktop-general`, created on first use), and each Orb conversation starts a new chat there instead of taking over the chat open in #general; a saved `general` moves over once.

### 2026-10-30 — 0.3.29

- Tauri 2.12 with tauri-plugin-updater 2.14.1 and `@tauri-apps/api`/`cli` 2.12.4; the Windows updater's archive reader moves to zip 9 (RUSTSEC-2026-0367/0368/0369).
- Setup checks a connected Google account's Cloud project roles and offers **Grant access**.
- Setup's pairing panel checks a connected Google account's Cloud project roles and offers **Grant access** (or the commands to run) and **Check again**.

### 2026-10-25 — 0.3.28

- The tray menu has **Restart Magican**, which relaunches the desktop app without touching the backend services.
- The desktop app no longer crashes when its own notch Attention panel takes focus: the contextual-assist watcher skips this app's own windows (it read their accessibility tree from a background thread, which WebKit kills the process for). Browser-context probes and the hotkey path no longer read this app's own windows through Accessibility either.
- Native setup repairs an empty or corrupt backend LaunchAgent instead of failing on it.
- Settings has two tabs in one window. **This Mac** groups the desktop settings into Permissions & privacy, Orb & assist, Engine & connection, General, and Keys & environment, with a sticky category panel, search (`/`, Enter, Esc) and a chip bar in a narrow window; Key Mappings, Env Map and Environment are categories instead of separate tabs. **Magican** shows the shared web Settings in the same window, signed in through the desktop session, instead of opening the browser. "Choose speaking voices" opens Voice & calls there.
- The tray no longer crashes when the notch Attention panel's page failed to load (for example, the dev server was down) and the panel is opened again.
- Desktop Edge tells the server its version when it connects, so web Settings → Devices shows which desktop build each paired Mac runs.
- The host gateway's automation status reports the desktop version, so Magician's `/health` (`versions.desktop`) and the phones' and web Settings' version lists show which desktop build is running.
- The Orb's voice drawer follows the app theme (surfaces, text, a themed thin scrollbar) instead of a fixed black panel; the Orb window now receives the app theme like Settings does.
- magic-supervisor relaunches the desktop app after a crash (the backend LaunchAgent now passes it the app bundle), but not after **Quit Magican**.
- Uninstall stops the native backend before deleting data, refuses a data folder that is the home folder or above it, and re-seeding never creates a shadowing `.env.development`.

### 2026-10-24 — 0.3.27

- Debug builds wait for the Vite dev server before pre-warming the HUD and Contextual Assist. Before this, a tray started alongside Vite could pre-warm empty windows, so "Ask Magican about this page" opened nothing until the tray restarted.

### 2026-10-24 — 0.3.26

- Contextual Assist pre-warms with the HUD, opens on its current target, follows the app theme and no longer jumps to the centre when expanded again.

### 2026-10-23 — 0.3.25

- Docker and Apple container launches publish Magicutor on `127.0.0.1` only and set `MAGICUTOR_TRUST_GATEWAY=1`, so only this machine reaches the browser bridge.

---

Older entries: `docs/archive/changelogs/desktop.md`
