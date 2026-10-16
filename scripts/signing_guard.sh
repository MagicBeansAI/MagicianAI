#!/usr/bin/env bash
# Refuse a commit that writes an Apple Team ID into a tracked Xcode project.
#
# The team belongs in the gitignored magios/Signing.local.xcconfig, which the
# tracked Signing.xcconfig includes for every target. Picking a team in
# Xcode's Signing & Capabilities tab writes `DEVELOPMENT_TEAM = <id>` into
# project.pbxproj instead, where it would ship one operator's team to every
# clone. An empty value (`DEVELOPMENT_TEAM = "";`) is allowed.
#
# Bypass for a deliberate change: SIGNING_GUARD_DISABLE=1 git commit ...
set -euo pipefail

[ "${SIGNING_GUARD_DISABLE:-0}" = "1" ] && exit 0

offending=$(git diff --cached --no-color -U0 -- '*.pbxproj' \
    | awk '
        /^\+\+\+ b\// { file = substr($0, 7); next }
        /^\+[[:space:]]*DEVELOPMENT_TEAM[[:space:]]*=/ {
            value = $0
            sub(/^[^=]*=[[:space:]]*/, "", value)
            sub(/;.*$/, "", value)
            gsub(/["[:space:]]/, "", value)
            if (value != "") print file ": DEVELOPMENT_TEAM = " value
        }
    ' || true)

if [ -n "$offending" ]; then
    echo "signing_guard: a staged Xcode project pins an Apple team:" >&2
    printf '%s\n' "$offending" | sed 's/^/  /' >&2
    cat >&2 <<'EOF'

Put the team in magios/Signing.local.xcconfig (gitignored) instead:
    DEVELOPMENT_TEAM = <your team id>
then unstage the project hunk (git restore --staged -p <file>) or remove
the DEVELOPMENT_TEAM line from project.pbxproj. In Xcode, leave the
target's Team as "None"; the xcconfig supplies it.
Deliberate? SIGNING_GUARD_DISABLE=1 git commit ...
EOF
    exit 1
fi
