#!/usr/bin/env bash
set -euo pipefail

CONFIG_PATH="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}/magician-config.yaml"

usage() {
  cat <<'EOF'
Usage: scripts/app-platform-processing-profile.sh [status|local|remote]

status   Print the effective processing profile settings from live config.
local    Select local processing (`privacy.processing.mode=local`).
remote   Select cloud processing (`privacy.processing.mode=cloud`).
EOF
}

run_command_status() {
  python3 - "$CONFIG_PATH" <<'PY'
import sys
from pathlib import Path
import yaml

path = Path(sys.argv[1])
root = yaml.load(path.read_text(), Loader=yaml.BaseLoader)
mode = root.get('privacy', {}).get('processing', {}).get('mode')
if mode not in {'local', 'cloud'}:
    raise SystemExit("privacy.processing.mode is missing or invalid")
processing = root.get('app_platform', {}).get('processing', {})
print(f'processing-locality settings in {path}')
print(f'  privacy.processing.mode: {mode}')
print(f'  derived app_platform.processing.remote_processing_enabled: {str(mode == "cloud").lower()}')
for key in ['local_profile', 'remote_profile']:
    route = processing.get(key, '')
    present = 'present' if route in processing.get('profiles', {}) else 'missing'
    print(f'  {key}: {route}')
    print(f'  processing binding {route}: {present}')
PY
}

set_processing_mode() {
python3 - "$1" "$2" <<'PY'
import os
import re
import sys
import tempfile
from pathlib import Path
import yaml

config_path = Path(sys.argv[1])
mode = sys.argv[2]
text = config_path.read_text()
root = yaml.load(text, Loader=yaml.BaseLoader)
processing = root.get('app_platform', {}).get('processing', {})
for key in ['local_profile', 'remote_profile']:
    if not processing.get(key) or processing[key] not in processing.get('profiles', {}):
        raise SystemExit(f'{key} must name an app_platform.processing binding')

pattern = re.compile(
    r"(?m)^(privacy:\n  processing:\n    mode:\s*)(local|cloud)(\s*(?:#.*)?)$"
)
text, replacements = pattern.subn(rf"\g<1>{mode}\g<3>", text, count=1)
if replacements != 1:
    raise SystemExit("privacy.processing.mode is missing or invalid")

# Avoid a second control silently returning through an old helper or manual
# edit. The runtime derives this field from privacy.processing.mode.
if re.search(r"(?m)^\s*remote_processing_enabled:\s*", text):
    raise SystemExit(
        "retired remote_processing_enabled setting is present; remove it and retry"
    )

fd, temporary_name = tempfile.mkstemp(
    prefix=f".{config_path.name}.", suffix=".tmp", dir=config_path.parent
)
try:
    with os.fdopen(fd, "w") as temporary:
        temporary.write(text)
        temporary.flush()
        os.fsync(temporary.fileno())
    os.chmod(temporary_name, config_path.stat().st_mode & 0o777)
    os.replace(temporary_name, config_path)
finally:
    if os.path.exists(temporary_name):
        os.unlink(temporary_name)
PY
}

main() {
  if [[ ! -f "$CONFIG_PATH" ]]; then
    echo "FAIL: missing live config at $CONFIG_PATH"
    exit 1
  fi

  local mode="${1:-status}"

  case "$mode" in
    status)
      run_command_status
      ;;
    local)
      set_processing_mode "$CONFIG_PATH" local
      echo "Updated live config for local processing. Reload config or restart the server."
      run_command_status
      ;;
    remote)
      set_processing_mode "$CONFIG_PATH" cloud
      echo "Updated live config for cloud processing. Reload config or restart the server."
      run_command_status
      ;;
    *)
      usage
      exit 1
      ;;
  esac
}

main "${1:-status}"
