#!/bin/sh
# Remove earlier Task Recipes fixture-eval workspaces from the runtime root.
#
# Only directories named scopes/<principal>/recipes-eval[-*] that carry the
# harness marker file are removed, and ONLY while Magician is not running:
# the live service lazily recreates any scope it has hydrated (scheduler
# state, boot-repair sweeps, open trace journals), and a journal that
# resumes mid-sequence in a recreated directory stops the next boot cold.
# The registry entries go too: on boot the service materializes every
# registered workspace, which would resurrect an empty scope the product
# then refuses to delete (DELETE /workspaces refuses a scope with files).
#
# The harnesses no longer depend on this: each run purges earlier harness
# workspaces through `DELETE /workspaces/{id}?purge=true`, which removes the
# directory at the server's next start and so is safe while it runs. This
# script remains for sweeping by hand while Magician is stopped.
#
# Usage: scripts/purge_task_recipes_eval_workspaces.sh [runtime_root] [principal]
set -eu
root="${1:-${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}}"
principal="${2:-anonymous}"
marker=".task-recipes-fixture-eval"
# Match the executable name, not command lines: another agent's shell
# that merely mentions magician.bin must not look like the live service.
if ps -axo comm= | awk '{ n = split($0, part, "/"); if (part[n] == "magician.bin") found = 1 } END { exit !found }'; then
  echo "purge_task_recipes_eval_workspaces: magician.bin is running; refusing to delete scope directories" >&2
  exit 2
fi
removed=0
for dir in "$root"/scopes/"$principal"/recipes-eval "$root"/scopes/"$principal"/recipes-eval-*; do
  [ -d "$dir" ] || continue
  if [ -f "$dir/$marker" ]; then
    rm -rf "$dir"
    echo "removed $(basename "$dir")"
    removed=$((removed + 1))
  else
    echo "kept $(basename "$dir") (no harness marker)"
  fi
done
registry="$root/scopes/$principal/workspaces.json"
if [ -f "$registry" ]; then
  python3 - "$registry" "$root/scopes/$principal" <<'PYEOF'
import json, os, sys
path, scopes = sys.argv[1], sys.argv[2]
data = json.load(open(path))
before = len(data.get("workspaces", []))
data["workspaces"] = [
    w for w in data.get("workspaces", [])
    # The harness's fixed slug is the bare `recipes-eval`; older runs minted
    # `recipes-eval-<suffix>`. A prefix test on "recipes-eval-" alone never
    # matched the bare one, so it survived every purge.
    if not ((w.get("id", "") == "recipes-eval" or w.get("id", "").startswith("recipes-eval-"))
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
echo "purge_task_recipes_eval_workspaces: removed $removed workspace(s)"
