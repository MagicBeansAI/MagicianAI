#!/usr/bin/env bash
# Keep the shared incremental cache from filling the build volume, and clear a
# corrupted crate cache without touching anyone else's.
#
# Two failures this exists for, both seen on this machine:
#
#   * The cache reached 368 GB and every build on the box failed with
#     "No space left on device", which reads like a compile error.
#   * A corrupted cache threw `internal compiler error: incremental
#     compilation error with evaluate_obligation`, and the build *before* that
#     one produced a binary whose runtime behaviour was wrong with no matching
#     source change.
#
# Disabling incremental outright fixes both and costs every debug rebuild on a
# 850k-LOC crate, so this prunes instead: oldest first, never a cache another
# process has open, down to a budget.
#
# Usage:
#   scripts/prune-incremental.sh [--dry-run] [--budget-gb N] [--crate PREFIX]
#   scripts/prune-incremental.sh --workspace-artifacts [--dry-run]
#
#   --crate PREFIX   drop every cache for one crate (e.g. magician_api) and
#                    ignore the budget; this is the corrupt-cache path.
#   --workspace-artifacts
#                    the leak that actually filled the volume: every build of
#                    a workspace crate whose dependency or feature hash changed
#                    leaves its previous artifacts under `deps/` — 32
#                    generations of the 850k-LOC `magician` crate at ~10 GB
#                    each were 339 GB on the day this was written, beside a
#                    36 GB incremental cache. `cargo clean -p <member>` for
#                    every workspace member removes them all, current included
#                    (the next build recompiles the workspace crates, never the
#                    third-party graph). Refused while any rustc or cargo is
#                    running: a clean under a live build breaks that build.
set -uo pipefail

BUDGET_GB="${INCREMENTAL_BUDGET_GB:-40}"
DRY_RUN=0
CRATE=""
WORKSPACE_ARTIFACTS=0
TARGET_DIR="${CARGO_TARGET_DIR:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --budget-gb) BUDGET_GB="$2"; shift 2 ;;
    --crate) CRATE="$2"; shift 2 ;;
    --workspace-artifacts) WORKSPACE_ARTIFACTS=1; shift ;;
    --target-dir) TARGET_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,34p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$TARGET_DIR" ]; then
  echo "CARGO_TARGET_DIR is not set; run through the Makefile or pass --target-dir" >&2
  exit 2
fi

if [ "$WORKSPACE_ARTIFACTS" -eq 1 ]; then
  if pgrep -x rustc >/dev/null 2>&1 || pgrep -x cargo >/dev/null 2>&1; then
    echo "a rustc or cargo is running; a workspace clean under a live build breaks it — retry when it finishes" >&2
    exit 3
  fi
  members=$(cargo metadata --no-deps --format-version 1 2>/dev/null \
    | python3 -c 'import json,sys; print(" ".join(p["name"] for p in json.load(sys.stdin)["packages"]))')
  if [ -z "$members" ]; then
    echo "could not list workspace members" >&2
    exit 2
  fi
  echo "workspace members: $members"
  for profile_dir in "$TARGET_DIR"/*/deps; do
    [ -d "$profile_dir" ] || continue
    for member in $members; do
      lib=$(printf '%s' "$member" | tr '-' '_')
      # shellcheck disable=SC2012
      kb=$(ls -l "$profile_dir"/lib"$lib"-*.rlib "$profile_dir"/lib"$lib"-*.rmeta "$profile_dir"/"$lib"-* 2>/dev/null | awk '{s+=$5} END {printf "%d", s/1024}')
      [ "${kb:-0}" -gt 0 ] && printf '  %-28s %6d MB in %s\n' "$member" "$((kb / 1024))" "$(basename "$(dirname "$profile_dir")")/deps"
    done
  done
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "dry run: would run cargo clean -p for every member above"
    exit 0
  fi
  args=""
  for member in $members; do args="$args -p $member"; done
  # shellcheck disable=SC2086
  CARGO_TARGET_DIR="$TARGET_DIR" cargo clean $args
  exit $?
fi

pruned_total_kb=0
pruned_count=0
skipped_in_use=0

# A cache another process has open is being written by a live rustc; removing
# it is how you turn one agent's build into everybody's mystery.
remove_cache() {
  local dir="$1"
  if lsof +D "$dir" >/dev/null 2>&1; then
    echo "  in use, kept: $(basename "$dir")"
    skipped_in_use=$((skipped_in_use + 1))
    return 1
  fi
  local kb
  kb=$(du -sk "$dir" 2>/dev/null | cut -f1)
  kb=${kb:-0}
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "  would remove: $(basename "$dir") ($((kb / 1024)) MB)"
  else
    rm -rf "$dir" || { echo "  FAILED to remove $(basename "$dir")" >&2; return 1; }
    echo "  removed: $(basename "$dir") ($((kb / 1024)) MB)"
  fi
  pruned_total_kb=$((pruned_total_kb + kb))
  pruned_count=$((pruned_count + 1))
  return 0
}

for profile_dir in "$TARGET_DIR"/*/incremental; do
  [ -d "$profile_dir" ] || continue
  echo "$profile_dir"

  if [ -n "$CRATE" ]; then
    # The corrupt-cache path: every generation of one crate, budget ignored.
    found=0
    for dir in "$profile_dir/$CRATE"-*; do
      [ -d "$dir" ] || continue
      found=1
      remove_cache "$dir"
    done
    [ "$found" -eq 0 ] && echo "  no caches for crate '$CRATE'"
    continue
  fi

  total_kb=$(du -sk "$profile_dir" 2>/dev/null | cut -f1)
  total_kb=${total_kb:-0}
  budget_kb=$((BUDGET_GB * 1024 * 1024))
  echo "  $((total_kb / 1024 / 1024)) GB used, budget ${BUDGET_GB} GB"
  [ "$total_kb" -le "$budget_kb" ] && { echo "  under budget, nothing to do"; continue; }

  # Oldest first: the caches least likely to serve the next build.
  while IFS= read -r dir; do
    [ "$total_kb" -le "$budget_kb" ] && break
    [ -d "$dir" ] || continue
    before=$pruned_total_kb
    remove_cache "$dir" && total_kb=$((total_kb - (pruned_total_kb - before)))
  done < <(ls -dt "$profile_dir"/*-* 2>/dev/null | tail -r)
done

echo
if [ "$DRY_RUN" -eq 1 ]; then
  echo "dry run: $pruned_count cache(s), $((pruned_total_kb / 1024 / 1024)) GB would be freed; $skipped_in_use in use"
else
  echo "pruned $pruned_count cache(s), $((pruned_total_kb / 1024 / 1024)) GB freed; $skipped_in_use in use"
fi
exit 0
