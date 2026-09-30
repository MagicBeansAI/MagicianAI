#!/usr/bin/env bash
# Prepare a machine to build the magdroid companion.
#
# Three things are missing from a plain checkout, and each fails differently:
#
#   1. A usable JDK. The build targets Java 17 and runs on Gradle 8.13, which
#      does not accept Java 24+. A machine whose only JDK is newer fails with a
#      Gradle "Unsupported class file major version" that reads like a corrupt
#      build rather than a toolchain mismatch.
#   2. The Android SDK. Without it AGP cannot resolve `compileSdk`.
#   3. `gradle/wrapper/gradle-wrapper.jar`. Upstream's `.gitignore` excludes
#      `*.jar` before re-including the wrapper, and the jar never made it into
#      the repository — so `./gradlew` fails with "Could not find or load main
#      class GradleWrapperMain" on a fresh clone of upstream too.
#
# Safe to re-run: every step checks before it installs.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ANDROID_DIR="$ROOT_DIR/magdroid/android"

GRADLE_VERSION="8.13"
JDK_FORMULA="openjdk@21"
ANDROID_PLATFORM="android-34"
ANDROID_BUILD_TOOLS="34.0.0"

say() { printf '\033[1;36m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33m warn\033[0m %s\n' "$1"; }
die() { printf '\033[1;31merror\033[0m %s\n' "$1" >&2; exit 1; }

command -v brew >/dev/null 2>&1 || die "Homebrew is required. See https://brew.sh"

# ── 1. JDK ────────────────────────────────────────────────────────────────────
JAVA_HOME_21="$(brew --prefix)/opt/$JDK_FORMULA/libexec/openjdk.jdk/Contents/Home"
if [ ! -d "$JAVA_HOME_21" ]; then
  say "Installing $JDK_FORMULA (Gradle $GRADLE_VERSION does not support Java 24+)"
  brew install "$JDK_FORMULA"
else
  say "$JDK_FORMULA already present"
fi
[ -d "$JAVA_HOME_21" ] || die "expected a JDK at $JAVA_HOME_21"
export JAVA_HOME="$JAVA_HOME_21"
export PATH="$JAVA_HOME/bin:$PATH"
say "JAVA_HOME=$JAVA_HOME ($(java -version 2>&1 | head -1))"

# ── 2. Android SDK ────────────────────────────────────────────────────────────
# The SDK is ~3GB and the Gradle cache grows past 1GB, so both live on the SSD
# beside the Cargo target dir and coverage output. `~/Library/Android/sdk` is a
# symlink to it rather than an environment variable: Android Studio and other GUI
# tools read that path directly and do not inherit shell exports — the same
# reason the notes store is a symlink rather than `MAGICIAN_NOTES_SPACE`.
MAGDROID_SDK_STORE="${MAGDROID_SDK_STORE:-/Volumes/build/magician/android-sdk}"
DEFAULT_SDK_LINK="$HOME/Library/Android/sdk"
if [ -z "${ANDROID_SDK_ROOT:-}" ] && [ ! -e "$DEFAULT_SDK_LINK" ] && [ -d "$(dirname "$MAGDROID_SDK_STORE")" ]; then
  say "Placing the SDK on the SSD at $MAGDROID_SDK_STORE"
  mkdir -p "$MAGDROID_SDK_STORE" "$(dirname "$DEFAULT_SDK_LINK")"
  ln -s "$MAGDROID_SDK_STORE" "$DEFAULT_SDK_LINK"
fi
ANDROID_SDK_ROOT="${ANDROID_SDK_ROOT:-$DEFAULT_SDK_LINK}"
SDKMANAGER=""
if [ -x "$ANDROID_SDK_ROOT/cmdline-tools/latest/bin/sdkmanager" ]; then
  SDKMANAGER="$ANDROID_SDK_ROOT/cmdline-tools/latest/bin/sdkmanager"
elif command -v sdkmanager >/dev/null 2>&1; then
  SDKMANAGER="$(command -v sdkmanager)"
else
  say "Installing Android command-line tools"
  brew install --cask android-commandlinetools
  SDKMANAGER="$(command -v sdkmanager || true)"
fi
[ -n "$SDKMANAGER" ] || die "sdkmanager not found after install"

mkdir -p "$ANDROID_SDK_ROOT"
export ANDROID_SDK_ROOT ANDROID_HOME="$ANDROID_SDK_ROOT"

say "Accepting SDK licenses (idempotent)"
yes 2>/dev/null | "$SDKMANAGER" --sdk_root="$ANDROID_SDK_ROOT" --licenses >/dev/null || true

say "Installing platform-tools, platforms;$ANDROID_PLATFORM, build-tools;$ANDROID_BUILD_TOOLS"
"$SDKMANAGER" --sdk_root="$ANDROID_SDK_ROOT" \
  "platform-tools" "platforms;$ANDROID_PLATFORM" "build-tools;$ANDROID_BUILD_TOOLS" >/dev/null

# AGP reads this rather than the environment, so a shell that forgot to export
# ANDROID_SDK_ROOT still builds.
printf 'sdk.dir=%s\n' "$ANDROID_SDK_ROOT" > "$ANDROID_DIR/local.properties"
say "Wrote $ANDROID_DIR/local.properties"

# ── 3. Gradle wrapper jar ─────────────────────────────────────────────────────
if [ ! -f "$ANDROID_DIR/gradle/wrapper/gradle-wrapper.jar" ]; then
  say "Generating the missing gradle-wrapper.jar"
  command -v gradle >/dev/null 2>&1 || brew install gradle
  # Generated in a scratch directory on purpose. Running `gradle wrapper` inside
  # the app would make Gradle configure the project first, and Homebrew ships a
  # Gradle far newer than AGP 8.x supports — it fails on a removed internal API
  # before it ever writes the jar. An empty directory has no plugins to apply.
  WRAPPER_TMP="$(mktemp -d)"
  # Gradle 9 refuses to run `wrapper` with no build present, so give it the
  # smallest legal one — an empty settings file declares a build and applies
  # nothing.
  : > "$WRAPPER_TMP/settings.gradle.kts"
  (cd "$WRAPPER_TMP" && gradle wrapper --gradle-version "$GRADLE_VERSION" >/dev/null)
  cp "$WRAPPER_TMP/gradle/wrapper/gradle-wrapper.jar" \
     "$ANDROID_DIR/gradle/wrapper/gradle-wrapper.jar"
  rm -rf "$WRAPPER_TMP"
  chmod +x "$ANDROID_DIR/gradlew"
else
  say "gradle-wrapper.jar already present"
fi
[ -f "$ANDROID_DIR/gradle/wrapper/gradle-wrapper.jar" ] || die "wrapper jar still missing"

# ── 4. A Gradle the plugin actually supports ─────────────────────────────────
# AGP 8.x refuses to run on Gradle 9 (it uses an internal API removed in 9.6),
# and Homebrew's default `gradle` is 9.x. The build prefers this keg.
if [ ! -x "$(brew --prefix)/opt/gradle@8/bin/gradle" ]; then
  say "Installing gradle@8 (AGP 8.x cannot run on Gradle 9)"
  brew install gradle@8
else
  say "gradle@8 already present"
fi

say "Ready. Build with:"
printf '\n    make build-magdroid\n    make test-magdroid\n\n'
say "Both targets pin JAVA_HOME to $JDK_FORMULA, so a newer system JDK will not break them."
