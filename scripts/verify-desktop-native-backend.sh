#!/usr/bin/env bash
# Fail-closed validation for the backend payload embedded in Magican Desktop.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESOURCE_DIR="$ROOT_DIR/desktop/src-tauri/native-backend"
EXPECTED_TARGET=""
EXPECTED_VERSION=""
EXPECTED_SIGNING=""
EXPECTED_COMMIT=""
GATEKEEPER=0

usage() {
  cat <<'USAGE'
Verify a Desktop native-backend resource directory.

  --resource-dir DIR   staged or bundled native-backend directory
  --target TARGET      exact Rust target triple (defaults to this host)
  --version VERSION    exact package version
  --signing MODE       exact manifest signing value: none, adhoc, developer-id
  --commit SHA         exact source commit
  --gatekeeper         require macOS Gatekeeper acceptance of every binary
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --resource-dir) RESOURCE_DIR="${2:?--resource-dir needs a directory}"; shift 2 ;;
    --target) EXPECTED_TARGET="${2:?--target needs a target}"; shift 2 ;;
    --version) EXPECTED_VERSION="${2:?--version needs a version}"; shift 2 ;;
    --signing) EXPECTED_SIGNING="${2:?--signing needs a mode}"; shift 2 ;;
    --commit) EXPECTED_COMMIT="${2:?--commit needs a SHA}"; shift 2 ;;
    --gatekeeper) GATEKEEPER=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) echo "ERROR: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

host_target() {
  case "$(uname -s):$(uname -m)" in
    Darwin:arm64|Darwin:aarch64) printf '%s\n' aarch64-apple-darwin ;;
    Linux:arm64|Linux:aarch64) printf '%s\n' aarch64-unknown-linux-gnu ;;
    Linux:x86_64|Linux:amd64) printf '%s\n' x86_64-unknown-linux-gnu ;;
    MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64) printf '%s\n' x86_64-pc-windows-msvc ;;
    *) echo "ERROR: cannot derive a native package target for $(uname -s)/$(uname -m)" >&2; return 1 ;;
  esac
}

manifest_value() {
  local key="$1"
  awk -v key="$key" '$1 == key ":" { $1=""; sub(/^[[:space:]]+/, ""); print; exit }' "$PACKAGE_ROOT/MANIFEST.yaml"
}

verify_sha_file() {
  local directory="$1"
  local sums_file="$2"
  if command -v shasum >/dev/null 2>&1; then
    (cd "$directory" && shasum -a 256 -c "$sums_file" >/dev/null)
  elif command -v sha256sum >/dev/null 2>&1; then
    (cd "$directory" && sha256sum -c "$sums_file" >/dev/null)
  else
    echo "ERROR: shasum or sha256sum is required to verify the native backend" >&2
    return 1
  fi
}

[[ -d "$RESOURCE_DIR" ]] || { echo "ERROR: native backend resource directory is missing: $RESOURCE_DIR" >&2; exit 1; }
[[ -n "$EXPECTED_TARGET" ]] || EXPECTED_TARGET="$(host_target)"

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/magician-native-verify.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

archives=()
while IFS= read -r archive; do
  [[ -n "$archive" ]] && archives+=("$archive")
done < <(find "$RESOURCE_DIR" -maxdepth 1 -type f -name 'magician-*.tar.gz' -print | sort)

DIRECT=0
if [[ -f "$RESOURCE_DIR/MANIFEST.yaml" ]]; then
  DIRECT=1
  [[ ${#archives[@]} -eq 0 ]] || { echo "ERROR: native backend resources mix a directory package with an archive" >&2; exit 1; }
  PACKAGE_ROOT="$RESOURCE_DIR"
else
  [[ ${#archives[@]} -eq 1 ]] || { echo "ERROR: expected exactly one native backend archive, found ${#archives[@]}" >&2; exit 1; }
  archive="${archives[0]}"
  archive_hash="$(basename "$archive").sha256"
  [[ -f "$RESOURCE_DIR/$archive_hash" ]] || { echo "ERROR: archive checksum is missing: $archive_hash" >&2; exit 1; }
  verify_sha_file "$RESOURCE_DIR" "$archive_hash"
  package_name="$({ python3 - "$archive" "$TMP_ROOT" <<'PY'
import pathlib
import sys
import tarfile

archive, destination = sys.argv[1:]
with tarfile.open(archive, "r:gz") as handle:
    roots = set()
    for member in handle.getmembers():
        path = pathlib.PurePosixPath(member.name)
        if path.is_absolute() or ".." in path.parts or not path.parts:
            raise SystemExit("ERROR: native backend archive contains an unsafe path")
        if not (member.isfile() or member.isdir()):
            raise SystemExit("ERROR: native backend archive contains a link or special file")
        roots.add(path.parts[0])
    if len(roots) != 1:
        raise SystemExit("ERROR: native backend archive must contain exactly one package root")
    handle.extractall(destination)
    print(next(iter(roots)))
PY
  } )"
  PACKAGE_ROOT="$TMP_ROOT/$package_name"
fi

target="$(manifest_value target)"
if [[ "$target" == *-windows-* ]]; then
  installer=install.ps1
  binaries=(magician.exe magicutor.exe magic-supervisor.exe)
else
  installer=install.sh
  binaries=(magician.bin magicutor.bin magic-supervisor.bin)
fi

for required in \
  MANIFEST.yaml SHA256SUMS "$installer" tool-runtime-config.yaml \
  "${binaries[@]}"; do
  [[ -f "$PACKAGE_ROOT/$required" ]] || { echo "ERROR: native backend package is missing $required" >&2; exit 1; }
done
[[ -d "$PACKAGE_ROOT/scripts" && -d "$PACKAGE_ROOT/share/seed" ]] || {
  echo "ERROR: native backend package is missing scripts or runtime seeds" >&2
  exit 1
}

verify_sha_file "$PACKAGE_ROOT" SHA256SUMS

python3 - "$PACKAGE_ROOT" "$DIRECT" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
direct = sys.argv[2] == "1"
listed = set()
for line in (root / "SHA256SUMS").read_text().splitlines():
    parts = line.split(maxsplit=1)
    if len(parts) != 2:
        raise SystemExit("ERROR: malformed native backend SHA256SUMS")
    name = parts[1]
    if name.startswith("*"):
        name = name[1:]
    if name.startswith("./"):
        name = name[2:]
    listed.add(name)
actual = {
    str(path.relative_to(root))
    for path in root.rglob("*")
    if path.is_file() and path.name != "SHA256SUMS"
}
if direct:
    actual.discard("README.md")
if listed != actual:
    missing = sorted(actual - listed)
    stale = sorted(listed - actual)
    raise SystemExit(f"ERROR: native backend checksum inventory differs (unlisted={missing}, missing={stale})")
PY

name="$(manifest_value name)"
version="$(manifest_value version)"
signing="$(manifest_value signing)"
commit="$(manifest_value commit)"

[[ "$name" == magician ]] || { echo "ERROR: native package name is '$name'" >&2; exit 1; }
[[ "$target" == "$EXPECTED_TARGET" ]] || { echo "ERROR: native package target '$target' does not match '$EXPECTED_TARGET'" >&2; exit 1; }
[[ -n "$version" ]] || { echo "ERROR: native package version is empty" >&2; exit 1; }
case "$signing" in none|adhoc|developer-id) ;; *) echo "ERROR: unknown native package signing mode '$signing'" >&2; exit 1 ;; esac
[[ -z "$EXPECTED_VERSION" || "$version" == "$EXPECTED_VERSION" ]] || { echo "ERROR: native package version '$version' does not match '$EXPECTED_VERSION'" >&2; exit 1; }
[[ -z "$EXPECTED_SIGNING" || "$signing" == "$EXPECTED_SIGNING" ]] || { echo "ERROR: native package signing '$signing' does not match '$EXPECTED_SIGNING'" >&2; exit 1; }
[[ -z "$EXPECTED_COMMIT" || "$commit" == "$EXPECTED_COMMIT" ]] || { echo "ERROR: native package commit '$commit' does not match '$EXPECTED_COMMIT'" >&2; exit 1; }

if [[ "$signing" == developer-id || "$signing" == adhoc ]]; then
  [[ "$target" == *-apple-darwin ]] || { echo "ERROR: Apple signing mode '$signing' is invalid for $target" >&2; exit 1; }
  command -v codesign >/dev/null 2>&1 || { echo "ERROR: codesign is required to verify signed macOS backend binaries" >&2; exit 1; }
  for binary in "${binaries[@]}"; do
    codesign --verify --strict "$PACKAGE_ROOT/$binary"
    if [[ "$signing" == developer-id ]]; then
      codesign_detail="$(codesign -dvv "$PACKAGE_ROOT/$binary" 2>&1)"
      [[ "$codesign_detail" == *"Authority=Developer ID Application:"* ]] || {
        echo "ERROR: $binary is not signed by a Developer ID Application identity" >&2
        exit 1
      }
    fi
  done
fi

if [[ "$GATEKEEPER" -eq 1 ]]; then
  [[ "$signing" == developer-id ]] || { echo "ERROR: Gatekeeper verification requires developer-id signing" >&2; exit 1; }
  [[ "$target" == *-apple-darwin ]] || { echo "ERROR: Gatekeeper verification applies only to macOS packages" >&2; exit 1; }
  command -v spctl >/dev/null 2>&1 || { echo "ERROR: spctl is required for Gatekeeper verification" >&2; exit 1; }
  for binary in "${binaries[@]}"; do
    spctl --assess --type execute --verbose=4 "$PACKAGE_ROOT/$binary"
  done
fi

printf 'Desktop native backend verified: version=%s target=%s signing=%s\n' "$version" "$target" "$signing"
