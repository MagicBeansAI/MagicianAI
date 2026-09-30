# CUA desktop setup

Run setup on the computer whose graphical desktop will be controlled. A Linux
backend container uses the desktop relay and does not need its own GUI driver.
The compatibility skill ID remains `macos-ui-automation`; it supports CuaDriver
providers on macOS, Windows and Linux independently of Apple Events.

| Desktop | Install | Install, start and verify |
| --- | --- | --- |
| Linux or macOS | `make setup-cua-driver` | `make setup-cua-driver ARGS=--start` |
| Windows (Python 3) | `py -3 scripts/setup-cua-driver.py` | `py -3 scripts/setup-cua-driver.py --start` |

These commands install exactly one CuaDriver release, currently **0.28.2**
(0.28.3 is skipped: its macOS CuaDriver.app shipped unsigned). They run the
[official installer](https://cua.ai/docs/how-to-guides/driver/install) scripts
fetched from that release's `cua-driver-rs-v0.28.2` GitHub tag, not the moving
`cua.ai/driver/install.sh` (which resolves the latest release). Every script is
SHA-256-checked before anything runs, the helpers sit beside the entry script in
one private temp folder, and `CUA_DRIVER_RS_VERSION` pins the installer's own
download. An installed driver of any other version, older or newer, is replaced,
and setup fails unless `cua-driver --version` reports the pin afterwards;
`ARGS=--upgrade` (Python: `--upgrade`) reinstalls the pin anyway. `--check` fails
on version drift without installing. The upstream installer displays its
telemetry notice.

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

Windows equivalent: `py -3 scripts/setup-cua-driver.py --check`. Checks never
install, upgrade or start a daemon. The local check prints doctor diagnostics and
requires a running daemon and nonempty window observation; doctor exit zero alone is not
proof. Open a target window before checking. It prints counts, not window titles.
The relay check verifies transport/daemon status only. Neither check certifies
click/type or screenshot permissions on every app.

Fresh Desktop setup selects **Control desktop apps** by default, separately from
**Control Mac apps** and iMessage. The user can leave it off and add it later
through **Settings → Manage capabilities**. When selected, Desktop installs the
same pinned release from the `cua-driver` entry of the components setup catalog
compiled into Desktop (never from the connected engine's copy): the tag-pinned
scripts for its own operating system (`install.sh`, `_install-rust.sh`,
`_install-common.sh`; Windows `install.ps1` + `_install-common.psm1`, with
`-LocalDir $PSScriptRoot` rewritten to the verified temp folder), each
SHA-256-checked before it runs, with `CUA_DRIVER_RS_VERSION` set. An already
installed driver is kept only when `cua-driver --version` equals the pin; any
other version is replaced, and the version must match after install. It installs
on the desktop host rather than the connected backend, starts the daemon, and
requires a successful verification before setup can finish. On macOS it runs
the read-only app-owned `check_permissions` probe; when a grant is missing it
runs `cua-driver permissions grant` and opens the first missing Accessibility or
Screen Recording pane only if that fails or leaves a grant missing. Only the
user can toggle that TCC grant. The Settings row shows `direct_capture_status`
and does not report ready when a live capture (`screen_recording_capturable`)
failed. On Windows and Linux it requires a successful, nonempty `list_windows`
observation from the local daemon, matching the platform setup verifier.

`requires.cua` gates the skill on a local desktop provider or relay;
`/host/automation/status.cua_available` is
independent of native macOS automation, while the aggregate `available` field
accepts either route. These are provider availability
checks, not a claim that all GUI actions are permitted. Catalog availability is
probed at backend startup: restart the backend after installing a new provider.

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
