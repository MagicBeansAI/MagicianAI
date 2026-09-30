#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: scripts/worktree_add.sh <path> <branch> [start-point]" >&2
  echo "Example: scripts/worktree_add.sh ../magician-feature feature/docs origin/main" >&2
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 0
fi

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  usage
  exit 1
fi

worktree_path="$1"
branch="$2"
start_point="${3:-}"

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$repo_root" ]; then
  echo "Not inside a git repository." >&2
  exit 1
fi

cd "$repo_root"

if [ -e "$worktree_path" ]; then
  echo "Target path already exists: $worktree_path" >&2
  exit 1
fi

if git show-ref --verify --quiet "refs/heads/$branch"; then
  git worktree add "$worktree_path" "$branch"
else
  if [ -n "$start_point" ]; then
    git worktree add "$worktree_path" -b "$branch" "$start_point"
  else
    git worktree add "$worktree_path" -b "$branch"
  fi
fi

# Ensure hooks are enabled both in the main worktree and the new worktree.
"$repo_root/scripts/setup_hooks.sh"
git -C "$worktree_path" config --local core.hooksPath .githooks

echo "Created worktree at $worktree_path with branch $branch"
echo "Configured core.hooksPath=.githooks in $worktree_path"
