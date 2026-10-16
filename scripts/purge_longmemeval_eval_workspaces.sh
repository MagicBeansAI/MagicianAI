#!/bin/sh
# Remove LongMemEval harness workspaces from the runtime root.
#
# Only directories named scopes/<principal>/lmeval-* are considered, and ONLY
# while Magician is not running: the live service lazily recreates any scope
# it has hydrated (scheduler state, boot-repair sweeps, open trace journals),
# and a journal that resumes mid-sequence in a recreated directory stops the
# next boot cold. Same contract as
# scripts/purge_task_recipes_eval_workspaces.sh.
#
# A directory is removed when it carries the harness marker
# (.longmemeval-eval, written by scripts/eval-longmemeval-live.py) or when the
# workspace registry no longer knows it (rows die at
# `DELETE /workspaces/{id}?purge=true`; the directory dies at the next server
# start — this sweep does that part offline). A registered workspace is never
# touched: purge those through the API instead.
#
# Usage: scripts/purge_longmemeval_eval_workspaces.sh [runtime_root] [principal]
set -eu
root="${1:-${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}}"
principal="${2:-anonymous}"
marker=".longmemeval-eval"
registry="$root/scopes/$principal/workspaces.json"

# Match the executable name, not command lines: another agent's shell
# that merely mentions magician.bin must not look like the live service.
if ps -axo comm= | awk '{ n = split($0, part, "/"); if (part[n] == "magician.bin") found = 1 } END { exit !found }'; then
  echo "purge_longmemeval_eval_workspaces: magician.bin is running; refusing to delete scope directories" >&2
  echo "(the API purge during each run already removed the registry rows; the" >&2
  echo " directories will be removed automatically at the next server start)" >&2
  exit 2
fi

registered() {
  [ -f "$registry" ] || return 1
  python3 - "$registry" "$1" <<'PYEOF'
import json, sys
path, slug = sys.argv[1], sys.argv[2]
try:
    data = json.load(open(path))
except (OSError, json.JSONDecodeError):
    sys.exit(1)
sys.exit(0 if any(w.get("id") == slug for w in data.get("workspaces", [])) else 1)
PYEOF
}

removed=0
for dir in "$root"/scopes/"$principal"/lmeval-*; do
  [ -d "$dir" ] || continue
  name=$(basename "$dir")
  if registered "$name"; then
    echo "kept $name (still registered — purge it via the API, not on disk)"
    continue
  fi
  marked=no
  [ -f "$dir/$marker" ] && marked=yes
  rm -rf "$dir"
  if [ "$marked" = yes ]; then
    echo "removed $name (marked)"
  else
    echo "removed $name (registry orphan)"
  fi
  removed=$(( removed + 1 ))
done

if [ -f "$registry" ]; then
  python3 - "$registry" "$root/scopes/$principal" <<'PYEOF'
import json, os, sys
path, scopes = sys.argv[1], sys.argv[2]
data = json.load(open(path))
before = len(data.get("workspaces", []))
data["workspaces"] = [
    w for w in data.get("workspaces", [])
    if not (str(w.get("id", "")).startswith("lmeval-")
            and not os.path.isdir(os.path.join(scopes, w["id"])))
]
if len(data["workspaces"]) != before:
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(data, fh, indent=2)
    os.replace(tmp, path)
    print(f"forgot {before - len(data['workspaces'])} registry entr(y/ies)")
PYEOF
fi
echo "purge_longmemeval_eval_workspaces: removed $removed workspace(s)"
