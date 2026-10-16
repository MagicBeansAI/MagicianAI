# Building the phone apps

There is no App Store or Play Store build yet — the macOS app is in Apple's
notarisation queue and the phone apps are in review. Until they land, running
Magican on a phone means building it, and that means the platform toolchains.

Android is scripted. iOS is not, and cannot be: Xcode comes from the App Store
and Apple will not let a script accept its licence for you.

---

## Android

### 1. Install the toolchain

```bash
make setup-magdroid-build
```

One command, one time. It fetches **JDK 21**, **Android SDK 34** and a pinned
Gradle wrapper.

Two version pins are load-bearing, and both exist because the obvious thing
fails:

- **JDK 21, not newer.** Gradle 8.13 rejects Java 24+, and a modern Homebrew
  install has something newer as the default `java`. The build targets pin
  `JAVA_HOME` rather than inheriting whatever `java` resolves to, so a working
  build does not depend on what else you have installed.
- **The pinned Gradle, not Homebrew's.** AGP 8.x uses a Gradle internal API that
  was removed in 9.6, so `brew install gradle` gives you a Gradle that cannot
  build this project.

If you already have an Android SDK, point at it with `ANDROID_SDK_ROOT` or
`ANDROID_HOME`; the default is `~/Library/Android/sdk`.

### 2. Build

```bash
make build-magdroid
```

If it prints *"Skipping magdroid build"*, step 1 has not completed — it is
checking for the JDK and SDK directories, not guessing.

### 3. Put it on a device

```bash
make install-magdroid
```

`adb` must be on `PATH` or reachable through `ANDROID_SDK_ROOT`. Enable
**Developer options → USB debugging** on the phone and accept the pairing
prompt; the first `adb` command after plugging in is usually the one that
triggers it.

`make logs-magdroid` tails the device log, which is the fastest way to see why
a launch failed.

### 4. Enable Android observation

Open **Settings → Connected devices → Android observation** in Magician. Choose
the trust method that matches how Magdroid was installed:

- **Private / self-hosted build** is for an APK you built and distributed. It
  needs no pins in the config: the phone's hardware attestation proves the
  app's signer and version, the desktop approval shows that exact build, and
  approving it pins the device to it for every reconnect. Switching the phone
  to another server is one pairing and one Allow. `mobile_access.
  android_apps_signing_sha256` / `android_apps_version_codes` still work as
  explicit pins when you want the config to name the build. Attestation roots
  are the config's, or Google's currently published hardware-attestation roots
  (`https://android.googleapis.com/attestation/root`, also in the
  seed/template) when the config lists none; a non-Google device needs its
  manufacturer's reviewed root in the config.
- **Google Play release** requires the same common pins plus the Play project,
  Play release-version and device-verdict lists, and
  `MAGICIAN_PLAY_INTEGRITY_SERVICE_ACCOUNT_PATH`. The common and Play version
  lists must name the same reviewed releases.

The page shows every missing prerequisite beside the disabled choice. Once a
choice is ready, open the compact desktop approval, create the QR, and scan it
in Magdroid. Private mode does not disable verification: Android must still
prove a TEE/StrongBox key, locked verified boot, the exact app signer and
version, and possession of the key on every reconnect.

---

## iOS

### 1. Install Xcode

From the Mac App Store. Not the Command Line Tools — `make setup-prerequisites`
already installs those, and they are not enough to build an app.

Then, once:

```bash
sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
xcodebuild -runFirstLaunch
```

### 2. Add an Apple ID

**Xcode → Settings → Accounts → +**, and sign in. A free Apple ID is enough to
run on your own device; it produces a build that expires after seven days and
has to be reinstalled. A paid Developer Program membership removes that.

Automatic provisioning will not work until an account is present here, and the
failure reads as a signing error rather than a missing account.

### 3. Set your team

The signing team lives in one untracked file:

- copy `magios/Signing.local.xcconfig.example` to `magios/Signing.local.xcconfig`
  (gitignored) and set `DEVELOPMENT_TEAM` to your Team ID

The tracked `magios/Signing.xcconfig` overlay includes it for every target, so it
survives `xcodegen generate`. Do not set the team in `project.yml` or Xcode's
Signing tab: the pre-commit `signing_guard.sh` refuses a `project.pbxproj` that
adds one.

Your Team ID is in **Xcode → Settings → Accounts → Manage Certificates**, or at
<https://developer.apple.com/account> under Membership.

### 4. Build and deploy

```bash
make ios-debug-build      # compile
make ios-debug-deploy     # to a connected device
make ios-debug-run        # build, deploy, launch
```

The device must be unlocked and trusted. The first install of a build signed
with a free Apple ID also needs **Settings → General → VPN & Device Management
→ Developer App → Trust** on the phone.

---

## What the phone needs from the backend

The apps talk to your machine, so it has to be reachable from the phone. Choose
**Same Wi-Fi · this computer** when both devices are on the same trusted network;
Magician advertises the computer's private LAN address. Choose **Remote · works
anywhere** on a different network; it uses your stable `connect.<zone>` host
(`MAGICIAN_CONNECT_HOST`, defaulting to `connect.$MAGICIAN_TUNNEL_ZONE`). Select the same
route in the phone app before scanning the corresponding QR from Magician
Desktop. A mismatched QR is refused with instructions to correct either side.
For Remote, Magican Desktop Settings chooses whether the stable public endpoint
reaches Magician on the host, its local container, or a verified remote HTTPS
server; changing that backend does not require another mobile enrollment.
Do not enter `localhost` on a physical phone: that address means the phone itself.
`make ios-full-setup-debug` wires the Access gate, the QR bootstrap route and
tunnel routing in one pass.

Android's ordinary **Connect Android** QR pairs chat, tasks, notes, and voice;
scan it from **Android Settings → Connection**. App Pilot and its Magician MCP
bridge use a different QR created by **Magican Desktop Settings → Android App
Observation** and scanned inside **Android App Pilot**. Each surface rejects the
other credential type. Until the owner-reviewed automation enrollment is
complete, Android reports **Magician MCP · Needs setup** rather than pretending
that mobile pairing started the automation bridge.

Neither app is useful on its own — it is a window onto the runtime, not a copy
of it.
