#!/usr/bin/env bash
# package-release.sh — the native backend release artifact.
#
# `release-container` publishes an image and `release-desktop` publishes the
# tray app. Neither produces the thing a person needs to run the backend on
# their own machine without a checkout, which is why `MODE=user FLOW=local` has
# always been refused. This makes that artifact.
#
# It packages, and only packages: if the binaries are not built it says so and
# stops rather than building them behind your back, because a release artifact
# built as a side effect of asking for one is how unreviewed code ships.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
RELEASE_DIR="${MAGICIAN_PACKAGE_RELEASE_DIR:-$BUILD_DIR/release}"
OUT_DIR="${MAGICIAN_PACKAGE_DIR:-$BUILD_DIR/packages}"

ok()   { printf '  OK %s\n' "$*"; }
err()  { printf '  ERROR %s\n' "$*" >&2; }
note() { printf '       %s\n' "$*"; }

# --- what this machine builds for -------------------------------------------
# The triple is part of the artifact name because a macOS arm64 binary on an
# Intel machine fails at exec with a message nobody can act on.
case "$(uname -s)" in
  Darwin) OS_PART="apple-darwin" ;;
  Linux)  OS_PART="unknown-linux-gnu" ;;
  MINGW*|MSYS*|CYGWIN*) OS_PART="pc-windows-msvc" ;;
  *) err "unsupported OS for packaging: $(uname -s)"; exit 1 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) ARCH_PART="aarch64" ;;
  x86_64|amd64)  ARCH_PART="x86_64" ;;
  *) err "unsupported architecture: $(uname -m)"; exit 1 ;;
esac
TARGET="${MAGICIAN_PACKAGE_TARGET:-${ARCH_PART}-${OS_PART}}"
WINDOWS_PACKAGE=0
case "$TARGET" in *-windows-*) WINDOWS_PACKAGE=1 ;; esac

VERSION="${RELEASE_VERSION:-}"
if [[ -z "$VERSION" ]]; then
  VERSION="$(git -C "$ROOT_DIR" describe --tags --abbrev=0 2>/dev/null || true)"
fi
: "${VERSION:=0.0.0-dev}"

BINARIES=(magician magicutor magic-supervisor decision-engine)
STAGE="$OUT_DIR/magician-$VERSION-$TARGET"
ARCHIVE="$STAGE.tar.gz"

# --- refuse early, and say what is missing ----------------------------------
missing=()
for binary in "${BINARIES[@]}"; do
  source_name="$binary"
  [[ "$WINDOWS_PACKAGE" -eq 0 ]] || source_name="$binary.exe"
  if [[ "$WINDOWS_PACKAGE" -eq 1 ]]; then
    [[ -f "$RELEASE_DIR/$source_name" ]] || missing+=("$source_name")
  else
    [[ -x "$RELEASE_DIR/$source_name" ]] || missing+=("$source_name")
  fi
done
if [[ ${#missing[@]} -gt 0 ]]; then
  err "these release binaries are not built: ${missing[*]}"
  note "Run 'make build-all-release' first. This script packages what exists;"
  note "it does not build, so that what ships is what you reviewed."
  exit 1
fi

# --- stage -------------------------------------------------------------------
# The layout is not a choice. The supervisor resolves the platform-native root
# names (`.bin` on Unix, `.exe` on Windows) with the prefix as its working
# directory, so the package *is* the runtime layout.
rm -rf "$STAGE"
mkdir -p "$STAGE/scripts" "$STAGE/share/seed"

for binary in "${BINARIES[@]}"; do
  if [[ "$WINDOWS_PACKAGE" -eq 1 ]]; then
    cp "$RELEASE_DIR/$binary.exe" "$STAGE/$binary.exe"
    chmod 755 "$STAGE/$binary.exe"
  else
    cp "$RELEASE_DIR/$binary" "$STAGE/$binary.bin"
    chmod 755 "$STAGE/$binary.bin"
  fi
done

# The supervisor's default Magician invocation names this file relative to the
# install prefix. Without it a verified package installs three binaries that
# cannot boot outside a source checkout.
cp "$ROOT_DIR/tool-runtime-config.yaml" "$STAGE/tool-runtime-config.yaml"

# The seed a fresh runtime root needs. Copied rather than referenced: the whole
# point is that the target machine has no checkout to reference.
cp "$ROOT_DIR/magician-config.yaml"                          "$STAGE/share/seed/magician-config.yaml"
cp "$ROOT_DIR/llm-router.yaml"                               "$STAGE/share/seed/llm-router.yaml"
cp "$ROOT_DIR/decision-engine.yaml"                          "$STAGE/share/seed/decision-engine.yaml"
cp "$ROOT_DIR/magician_data_v3/.env.example"                 "$STAGE/share/seed/.env.example"
cp "$ROOT_DIR/magician_data_v3/operator-config.template.yaml" "$STAGE/share/seed/operator-config.template.yaml"
for agent in harness-sre cto internal-system-analyst; do
  mkdir -p "$STAGE/share/seed/scopes/anonymous/default/agent_runtime/agents/$agent"
  cp "$ROOT_DIR/magician_data_v3/scopes/anonymous/default/agent_runtime/agents/$agent/definition.agent.yaml" \
    "$STAGE/share/seed/scopes/anonymous/default/agent_runtime/agents/$agent/definition.agent.yaml"
done
mkdir -p "$STAGE/share/seed/scopes/anonymous/default/programs"
cp "$ROOT_DIR/magician_data_v3/scopes/anonymous/default/programs/harness_reliability.md" \
  "$STAGE/share/seed/scopes/anonymous/default/programs/harness_reliability.md"

# Every script the runtime entry points reach, transitively. Computed rather
# than listed: a hand-kept list is a list that silently loses a file the day
# someone adds a call, and the failure lands on a user's machine.
# Read into an array the long way: macOS ships bash 3.2, which has no mapfile.
RUNTIME_SCRIPTS=()
while IFS= read -r _script_line; do
  [ -n "$_script_line" ] && RUNTIME_SCRIPTS+=("$_script_line")
done < <(python3 - "$ROOT_DIR" <<'CLOSURE'
import re, sys
from pathlib import Path

root = Path(sys.argv[1])
# The three things an install always invokes: start the stack, seed a fresh
# runtime root, and check the result.
stack = ["run-supervisor.sh", "seed-silverbullet-space.sh", "install-verify.sh"]

# Plus whatever the component graph says its install steps run. Read from the
# graph rather than listed here, so a component added with a `script:` ships
# that script without anyone remembering to come back to this file — and a
# package that cannot perform its own capability steps is a package that
# installs a backend and nothing else.
graph = root / "magician-components" / "src" / "graph.yaml"
if graph.is_file():
    stack.extend(re.findall(r"script:\s*([A-Za-z0-9_.-]+\.(?:sh|py|rb))", graph.read_text()))
seen = set()
# `scripts/foo.sh`, `$SCRIPT_DIR/foo.sh`, `${ROOT_DIR}/scripts/foo.sh`
pattern = re.compile(
    r"(?:scripts/|\$\{?(?:SCRIPT_DIR|script_dir|ROOT_DIR)\}?/(?:scripts/)?)"
    r"([A-Za-z0-9_.-]+\.(?:sh|py|rb))"
)
while stack:
    name = stack.pop()
    if name in seen:
        continue
    seen.add(name)
    path = root / "scripts" / name
    if not path.is_file():
        continue
    stack.extend(pattern.findall(path.read_text(errors="ignore")))
for name in sorted(seen):
    if (root / "scripts" / name).is_file():
        print(name)
CLOSURE
)
if [[ ${#RUNTIME_SCRIPTS[@]} -eq 0 ]]; then
  err "the runtime script closure came back empty — refusing to ship a package that cannot start"
  exit 1
fi
for script in "${RUNTIME_SCRIPTS[@]}"; do
  cp "$ROOT_DIR/scripts/$script" "$STAGE/scripts/$script"
  chmod 755 "$STAGE/scripts/$script"
done
ok "${#RUNTIME_SCRIPTS[@]} scripts (closure of the runtime entry points and every install step the graph declares)"

# The package installs itself. Shipping the platform-native installer inside
# means a Windows package does not quietly acquire a Git Bash prerequisite.
if [[ "$WINDOWS_PACKAGE" -eq 1 ]]; then
  cp "$ROOT_DIR/scripts/package-installer.ps1" "$STAGE/install.ps1"
  cp "$ROOT_DIR/scripts/package-uninstaller.ps1" "$STAGE/uninstall.ps1"
else
  cp "$ROOT_DIR/scripts/package-installer.sh" "$STAGE/install.sh"
  chmod 755 "$STAGE/install.sh"
  cp "$ROOT_DIR/scripts/package-uninstaller.sh" "$STAGE/uninstall.sh"
  chmod 755 "$STAGE/uninstall.sh"
fi

# --- a manifest that says what this is --------------------------------------
# Per-crate versions, because they are versioned per crate and a single release
# number would hide which magicutor is inside.
crate_version() {
  awk -F'"' '/^version = /{print $2; exit}' "$ROOT_DIR/$1/Cargo.toml"
}
{
  printf 'name: magician\n'
  printf 'version: %s\n' "$VERSION"
  printf 'target: %s\n' "$TARGET"
  printf 'built_at: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'commit: %s\n' "$(git -C "$ROOT_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"
  printf 'components:\n'
  printf '  magician: %s\n'        "$(crate_version magician-bin)"
  printf '  magicutor: %s\n'       "$(crate_version magicutor)"
  printf '  magic-supervisor: %s\n' "$(crate_version magic-supervisor)"
} > "$STAGE/MANIFEST.yaml"

# --- sign, before anything is hashed -----------------------------------------
# Signing rewrites the binaries. Hashing first would put the pre-signature
# digests in SHA256SUMS and every install would fail its own check.
if [[ "${MAGICIAN_SIGN:-0}" == "1" || "${MAGICIAN_SIGN_ADHOC:-0}" == "1" ]]; then
  if [[ "$WINDOWS_PACKAGE" -eq 1 ]]; then
    err "Apple signing cannot be applied to a Windows package"
    exit 1
  fi
  if ! bash "$ROOT_DIR/scripts/sign-release.sh" "$STAGE"; then
    err "signing failed — refusing to ship an unsigned package that was asked to be signed"
    exit 1
  fi
else
  printf 'signing: none\n' >> "$STAGE/MANIFEST.yaml"
fi

# Hashes of the payload, inside the payload, so an unpacked tree can be checked
# without the archive it came from.
if command -v shasum >/dev/null 2>&1; then
  HASH_COMMAND=(shasum -a 256)
elif command -v sha256sum >/dev/null 2>&1; then
  HASH_COMMAND=(sha256sum)
else
  err "shasum or sha256sum is required to package a release"
  exit 1
fi
# shellcheck disable=SC2094 # the output file is deliberately excluded by name
( cd "$STAGE" && find . -type f ! -name SHA256SUMS -print0 \
    | sort -z | xargs -0 "${HASH_COMMAND[@]}" > SHA256SUMS )

# --- archive, then hash the archive -----------------------------------------
( cd "$OUT_DIR" && COPYFILE_DISABLE=1 tar -czf "$(basename "$ARCHIVE")" "$(basename "$STAGE")" )
( cd "$OUT_DIR" && "${HASH_COMMAND[@]}" "$(basename "$ARCHIVE")" > "$(basename "$ARCHIVE").sha256" )

# A caller that wants to chain to the installer needs the path without parsing
# prose. Writing it to a named file beats printing it, because the human output
# above is free to change.
if [[ -n "${MAGICIAN_PACKAGE_PATH_FILE:-}" ]]; then
  printf '%s\n' "$ARCHIVE" > "$MAGICIAN_PACKAGE_PATH_FILE"
fi

ok "$ARCHIVE"
note "$(du -h "$ARCHIVE" | cut -f1) · $(cat "$ARCHIVE.sha256" | cut -d' ' -f1 | cut -c1-16)…"
case "$TARGET:$(awk '/^signing:/{print $2; exit}' "$STAGE/MANIFEST.yaml" 2>/dev/null)" in
  *-windows-*:*)
    note "unsigned Windows package: suitable for VM and local acceptance testing;"
    note "public distribution will need Authenticode signing later."
    ;;
  *:developer-id)
    note "signed, NOT notarised. Gatekeeper wants both before a stranger can run it."
    ;;
  *:adhoc)
    note "ad-hoc signed: identity for this machine, not distribution. Still"
    note "quarantined on download."
    ;;
  *)
    note "unsigned: macOS quarantines a downloaded unsigned binary, so this is"
    note "not publishable to end users. MAGICIAN_SIGN=1 signs it if a Developer"
    note "ID certificate is present."
    ;;
esac
