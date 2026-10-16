#!/bin/sh
# Cargo target runner for MagicRun test binaries.
#
# tempfile follows TMPDIR. The suite points that at the build volume, which is
# group-writable, so the jail refuses every fixture root before a process
# starts. A non-canonical path also makes the working-directory check compare
# two spellings of one directory. This sets TMPDIR to the canonical Darwin
# user temp for the test process only. rustc keeps the TMPDIR it inherited.
set -eu
if [ "$(uname -s)" = Darwin ]; then
    raw=$(/usr/bin/getconf DARWIN_USER_TEMP_DIR) || {
        printf '%s\n' 'magicrun fixture temp: DARWIN_USER_TEMP_DIR unavailable' >&2
        exit 1
    }
    canon=$(python3 -c 'import os, sys; print(os.path.realpath(sys.argv[1]), end="")' "$raw") || exit 1
    if [ ! -d "$canon" ]; then
        printf '%s\n' "magicrun fixture temp: not a directory: $canon" >&2
        exit 1
    fi
    TMPDIR=$canon
    export TMPDIR
    unset TMP TEMP
fi
exec "$@"
