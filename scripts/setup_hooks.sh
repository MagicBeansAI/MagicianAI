#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$repo_root" ]; then
  echo "Not inside a git repository." >&2
  exit 1
fi

cd "$repo_root"

if [ ! -d ".githooks" ]; then
  echo "Missing .githooks directory in repository root." >&2
  exit 1
fi

current="$(git config --local --get core.hooksPath || true)"
if [ "$current" = ".githooks" ]; then
  echo "core.hooksPath already set to .githooks"
  exit 0
fi

git config --local core.hooksPath .githooks
echo "Set core.hooksPath=.githooks"
