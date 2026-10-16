# CUA desktop setup

Run setup on the computer whose graphical desktop will be controlled. A Linux
backend container uses the desktop relay and does not need its own GUI driver.
The compatibility skill ID remains `macos-ui-automation`; it supports CuaDriver
providers on macOS, Windows and Linux independently of Apple Events.

| Desktop | Install | Install, start and verify |
| --- | --- | --- |
| Linux or macOS | `make setup-cua-driver` | `make setup-cua-driver ARGS=--start` |
| Windows (Python 3) | `py -3 scripts/setup-cua-driver.py` | `py -3 scripts/setup-cua-driver.py --start` |

These install exactly one CuaDriver release, currently **0.28.2** (0.28.3 is
skipped: its macOS CuaDriver.app shipped unsigned), using the
[official installer](https://cua.ai/docs/how-to-guides/driver/install) scripts
from that release's `cua-driver-rs-v0.28.2` GitHub tag, not the moving
`cua.ai/driver/install.sh`. Every script is SHA-256-checked before anything
runs, helpers sit beside the entry script in one private temp folder, and
`CUA_DRIVER_RS_VERSION` pins the installer's own download. Any other installed
version is replaced, and setup fails unless `cua-driver --version` reports the
pin afterwards; `ARGS=--upgrade` (Python: `--upgrade`) reinstalls anyway.
`--check` fails on version drift without installing. The upstream installer
shows its telemetry notice.

The pin (version, file URLs, hashes) lives in two places kept equal by
`make test-cua-setup`: `CUA_DRIVER_VERSION`/`INSTALLER_FILES` in
`scripts/setup-cua-driver.py` and the `cua-driver` entry of
`magician-components/src/setup_catalog.yaml`, which Desktop installs from.
Bump both together, only to a release whose macOS app passes codesign.
Installing the binary without a desktop session is allowed (for image preparation);
`--start` requires a desktop. Installation alone never enables a headless provider.
No global execution policy, permissions, or firewall settings are changed by
Magician's helper. On Windows, discovery also checks the official
`%LOCALAPPDATA%\Programs\Cua\cua-driver\bin\cua-driver.exe` location before a
new shell/app has inherited its updated PATH.

- **Windows:** run as the signed-in desktop user. A service or SSH Session 0
  cannot start a driver for the user's desktop. An already-running driver can
  be reached through its local endpoint where the OS permits it.
- **Linux:** start inside the target X11/Wayland session with its display and
  AT-SPI accessibility bus. `cua-driver doctor` diagnoses missing dependencies;
  compositor/toolkit support still determines which actions work.
- **macOS:** startup retains the CuaDriver.app identity. Grant with
  `cua-driver permissions grant`: it launches CuaDriver.app through
  LaunchServices so macOS attributes the grants to the app (not your terminal),
  requests Accessibility, Screen Recording and Tahoe's direct-capture consent,
  then verifies a live capture. `cua-driver permissions status --json` is the
  read-only check. Apple Events is a separate permission.

Read-only verification:

```sh
make check-cua-driver                  # local daemon and window observation
make check-cua-driver ARGS=--relay      # existing desktop relay/daemon only
```

Windows: `py -3 scripts/setup-cua-driver.py --check`. Checks never install,
upgrade or start a daemon. The local check prints doctor diagnostics and needs
a running daemon and a nonempty window observation (open a target window
first); it prints counts, not titles. The relay check covers transport/daemon
status only. Neither certifies click/type or screenshot permission on every
app.

Fresh Desktop setup selects **Control desktop apps** by default, separately
from **Control Mac apps** and iMessage; it can be added later through
**Settings → Manage capabilities**. Desktop installs the same pinned release
from the `cua-driver` entry of the setup catalog compiled into Desktop (never
the connected engine's copy): the tag-pinned scripts for its OS (`install.sh`,
`_install-rust.sh`, `_install-common.sh`; Windows `install.ps1` +
`_install-common.psm1`, with `-LocalDir $PSScriptRoot` rewritten to the
verified temp folder), each SHA-256-checked, with `CUA_DRIVER_RS_VERSION` set.
The same version rule applies. It installs on the desktop host, not the
backend, starts the daemon, and needs a successful verification to finish:

- **macOS:** the read-only app-owned `check_permissions` probe; if a grant is
  missing it runs `cua-driver permissions grant`, and opens the first missing
  Accessibility or Screen Recording pane only if that fails. Only the user can
  toggle a TCC grant. The Settings row shows `direct_capture_status` and is not
  ready when a live capture (`screen_recording_capturable`) failed.
- **Windows and Linux:** a nonempty `list_windows` observation from the local
  daemon, as in the platform setup verifier.

`requires.cua` gates the skill on a local desktop provider or relay.
The `cua_available` field of the gateway's `GET /host/automation/status`
(`desktop/src-tauri/src/host_gateway.rs`) is independent of native macOS
automation; the aggregate `available` field accepts either route. Neither
claims all GUI actions are permitted. Availability is probed once at backend
startup (`magician-bin/src/main.rs` → `config_extras::set_cua_available`), so
restart the backend after installing a new provider.

`MAGICIAN_CUA_DRIVER_BIN` can select a nonstandard absolute executable path on
the desktop. `MAGICIAN_HOST_GATEWAY_URL` selects the relay URL (default loopback
port 3017). A local usable desktop/daemon takes precedence; an installed driver
in a headless Linux container does not take precedence over the relay.

The Windows setup/controller branches are covered by platform fixtures. Magican
Desktop is packaged as a Windows NSIS installer and Linux AppImage; both run the
host gateway in the signed-in desktop session. Live Windows and Linux GUI
click/type acceptance is still performed on those operating systems.

Focused checks, with no full service/image build:

```sh
make test-cua-setup                    # Python platform and relay fixtures
CARGO_BUILD_JOBS=1 make test-cua-platform  # small Rust discovery/setup-graph tests
```
