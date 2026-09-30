#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/magician-native-package.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

mkdir -p "$TMP_ROOT/release" "$TMP_ROOT/packages" "$TMP_ROOT/resources"
for binary in magician magicutor magic-supervisor; do
  cat > "$TMP_ROOT/release/$binary" <<EOF
#!/usr/bin/env bash
echo $binary fixture
EOF
  chmod 755 "$TMP_ROOT/release/$binary"
done

RELEASE_VERSION=fixture \
MAGICIAN_PACKAGE_RELEASE_DIR="$TMP_ROOT/release" \
MAGICIAN_PACKAGE_DIR="$TMP_ROOT/packages" \
MAGICIAN_PACKAGE_PATH_FILE="$TMP_ROOT/package-path" \
  bash "$SCRIPT_DIR/package-release.sh" >/dev/null

archive="$(cat "$TMP_ROOT/package-path")"
test -f "$archive"
test -f "$archive.sha256"
tar -xzf "$archive" -C "$TMP_ROOT"
package_root="${archive%.tar.gz}"
test -f "$package_root/tool-runtime-config.yaml"
test -f "$package_root/share/seed/llm-router.yaml"
test -f "$package_root/share/seed/scopes/anonymous/default/programs/harness_reliability.md"

bash "$package_root/install.sh" --yes --prefix "$TMP_ROOT/installed" >/dev/null
for file in magician.bin magicutor.bin magic-supervisor.bin decision-engine.bin tool-runtime-config.yaml MANIFEST.yaml SHA256SUMS; do
  test -f "$TMP_ROOT/installed/$file"
done

MAGICIAN_PACKAGE_RELEASE_DIR="$TMP_ROOT/release" \
MAGICIAN_PACKAGE_DIR="$TMP_ROOT/stage-packages" \
MAGICIAN_PACKAGE_STAGE_TMP="$TMP_ROOT/stage" \
MAGICIAN_DESKTOP_NATIVE_RESOURCE_DIR="$TMP_ROOT/resources" \
RELEASE_VERSION=fixture \
  bash "$SCRIPT_DIR/stage-desktop-native-backend.sh" >/dev/null

for file in magician.bin magicutor.bin magic-supervisor.bin decision-engine.bin tool-runtime-config.yaml MANIFEST.yaml SHA256SUMS; do
  test -f "$TMP_ROOT/resources/$file"
done
bash "$SCRIPT_DIR/verify-desktop-native-backend.sh" \
  --resource-dir "$TMP_ROOT/resources" \
  --version fixture \
  --signing none >/dev/null

if bash "$SCRIPT_DIR/verify-desktop-native-backend.sh" \
  --resource-dir "$TMP_ROOT/resources" \
  --target definitely-the-wrong-target >/dev/null 2>&1; then
  echo "native package verifier accepted a wrong target" >&2
  exit 1
fi

mkdir -p "$TMP_ROOT/fake.app/Contents/Resources/native-backend"
cp -R "$TMP_ROOT/resources"/. "$TMP_ROOT/fake.app/Contents/Resources/native-backend/"
bash "$SCRIPT_DIR/verify-desktop-native-bundle.sh" \
  --bundle "$TMP_ROOT/fake.app" \
  --platform macos \
  --version fixture \
  --signing none >/dev/null

mkdir -p "$TMP_ROOT/archive-resources"
MAGICIAN_PACKAGE_RELEASE_DIR="$TMP_ROOT/release" \
MAGICIAN_PACKAGE_DIR="$TMP_ROOT/archive-packages" \
MAGICIAN_PACKAGE_STAGE_TMP="$TMP_ROOT/archive-stage" \
MAGICIAN_DESKTOP_NATIVE_RESOURCE_DIR="$TMP_ROOT/archive-resources" \
MAGICIAN_DESKTOP_NATIVE_RESOURCE_FORMAT=archive \
RELEASE_VERSION=fixture \
  bash "$SCRIPT_DIR/stage-desktop-native-backend.sh" >/dev/null
test "$(find "$TMP_ROOT/archive-resources" -name 'magician-*.tar.gz' | wc -l | tr -d ' ')" = 1
test "$(find "$TMP_ROOT/archive-resources" -name 'magician-*.tar.gz.sha256' | wc -l | tr -d ' ')" = 1
bash "$SCRIPT_DIR/verify-desktop-native-backend.sh" \
  --resource-dir "$TMP_ROOT/archive-resources" \
  --version fixture \
  --signing none >/dev/null

# Windows keeps the same package identity and archive contract, but carries PE
# executable names plus PowerShell install/remove entry points. This fixture is
# intentionally not executable on the Unix test host; the verifier is content
# and target driven.
mkdir -p "$TMP_ROOT/windows-release" "$TMP_ROOT/windows-resources"
for binary in magician magicutor magic-supervisor; do
  printf 'PE fixture for %s\n' "$binary" > "$TMP_ROOT/windows-release/$binary.exe"
done
MAGICIAN_PACKAGE_RELEASE_DIR="$TMP_ROOT/windows-release" \
MAGICIAN_PACKAGE_DIR="$TMP_ROOT/windows-packages" \
MAGICIAN_PACKAGE_STAGE_TMP="$TMP_ROOT/windows-stage" \
MAGICIAN_DESKTOP_NATIVE_RESOURCE_DIR="$TMP_ROOT/windows-resources" \
MAGICIAN_PACKAGE_TARGET=x86_64-pc-windows-msvc \
RELEASE_VERSION=fixture \
  bash "$SCRIPT_DIR/stage-desktop-native-backend.sh" >/dev/null
for file in magician.exe magicutor.exe magic-supervisor.exe install.ps1 uninstall.ps1 MANIFEST.yaml SHA256SUMS; do
  test -f "$TMP_ROOT/windows-resources/$file"
done
bash "$SCRIPT_DIR/verify-desktop-native-backend.sh" \
  --resource-dir "$TMP_ROOT/windows-resources" \
  --target x86_64-pc-windows-msvc \
  --version fixture \
  --signing none >/dev/null
mkdir -p "$TMP_ROOT/fake-windows-root/resources/native-backend"
cp -R "$TMP_ROOT/windows-resources"/. "$TMP_ROOT/fake-windows-root/resources/native-backend/"
bash "$SCRIPT_DIR/verify-desktop-native-bundle.sh" \
  --bundle "$TMP_ROOT/fake-windows-root" \
  --platform windows \
  --target x86_64-pc-windows-msvc \
  --version fixture \
  --signing none >/dev/null

printf 'unreviewed\n' > "$TMP_ROOT/resources/scripts/unlisted.sh"
if bash "$SCRIPT_DIR/verify-desktop-native-backend.sh" \
  --resource-dir "$TMP_ROOT/resources" >/dev/null 2>&1; then
  echo "native package verifier accepted an unlisted file" >&2
  exit 1
fi

echo "native package contract: ok"
