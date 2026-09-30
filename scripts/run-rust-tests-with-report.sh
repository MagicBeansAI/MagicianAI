#!/usr/bin/env bash
set -euo pipefail

# This script exports test-lane values — dummy API keys, a temp
# MAGICIAN_STORAGE_PATH, an explicit execution driver — that are correct for a
# child `cargo` process and actively harmful in an interactive shell. Sourced
# instead of executed, they persist in that shell, and anything launched from
# it later inherits them: a supervisor started that way boots with
# `OPENAI_API_KEY=test-dummy` and a runtime root under $TMPDIR, and fails in
# ways that name neither. Both happened on 2026-09-07.
#
# `dotenvy` will not overwrite a variable already in the environment — correctly,
# so an operator export wins — which means the runtime's own `.env` cannot undo
# this. Refusing to be sourced is the only place it can be stopped.
if [[ "${BASH_SOURCE[0]}" != "${0}" ]]; then
    echo "run-rust-tests-with-report.sh must be executed, not sourced:" >&2
    echo "  its test-lane exports would persist in this shell and be inherited" >&2
    echo "  by anything you launch from it (supervisor, desktop, another build)." >&2
    echo "Run it as: ./scripts/run-rust-tests-with-report.sh" >&2
    return 1
fi

# Stack-safety is part of the canonical Rust test contract. Never let a
# developer shell or supervisor session silently make tests pass by enlarging
# every spawned test/Tokio thread.
unset RUST_MIN_STACK

verbose=0
coverage=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --verbose) verbose=1 ;;
        --coverage) coverage=1 ;;
        *)
            echo "Usage: $0 [--verbose] [--coverage]" >&2
            exit 2
            ;;
    esac
    shift
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
report_root="${RUST_TEST_REPORT_DIR:-$repo_root/coverage/rust}"
cargo_target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
test_threads="${RUST_TEST_THREADS:-4}"
fd_limit="${TEST_FD_LIMIT:-16384}"
run_id="$(date '+%Y%m%d-%H%M%S')-$$"
run_dir="$report_root/results/$run_id"
junit_source="$repo_root/target/nextest/rust-report/junit.xml"
junit_result="$run_dir/nextest-junit.xml"
doctest_log="$run_dir/doctests.log"
coverage_json="$run_dir/coverage-summary.json"
coverage_html="$run_dir/coverage/html/index.html"
marker="$run_dir/.started"
test_data_parent="$(mktemp -d)"

cleanup() {
    rm -rf "$test_data_parent"
}
trap cleanup EXIT

mkdir -p "$run_dir"
touch "$marker"
ulimit -n "$fd_limit" 2>/dev/null || ulimit -n 4096 2>/dev/null || true

export OPENAI_API_KEY="${OPENAI_API_KEY:-test-dummy}"
export ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-test-dummy}"
export OPENROUTER_API_KEY="${OPENROUTER_API_KEY:-test-dummy}"
export GEMINI_API_KEY="${GEMINI_API_KEY:-test-dummy}"
export MAGICIAN_DISABLE_SYSTEM_PROXY=1
# Most unit and integration fixtures exercise execution behavior without
# constructing the durable identities required by the production-default
# stateless driver. Default the canonical suite to the explicit rollback driver,
# while preserving an explicit caller override for stateless qualification.
export MAGICIAN_EXECUTION_DRIVER="${MAGICIAN_EXECUTION_DRIVER:-inprocess}"
export MAGICIAN_STORAGE_PATH="$test_data_parent/magician_data_v3"
export MAGICUTOR_DATA_ROOT="$test_data_parent/magician_data_v3"
export RUST_TEST_THREADS="$test_threads"
# This lane deliberately saturates the machine with several concurrent test
# processes, so a wall-clock budget line here measures the runner rather than
# the adapter. Gate 3 latency lines are recorded but not asserted; enforce them
# in the uncontended lane instead (`make test-storage-budgets`).
export MAGICIAN_GATE3_ENFORCE_LATENCY="${MAGICIAN_GATE3_ENFORCE_LATENCY:-0}"

nextest_args=(
    --workspace
    --config-file "$repo_root/scripts/nextest-report.toml"
    --profile rust-report
    --no-fail-fast
    --test-threads "$test_threads"
)
if [[ $verbose -eq 1 ]]; then
    nextest_args+=(--status-level all --final-status-level all --success-output immediate)
fi

if [[ $coverage -eq 1 ]]; then
    echo "🦀 Running all Rust unit and integration tests with coverage..."
    set +e
    cargo llvm-cov nextest --no-report "${nextest_args[@]}" 2>&1 | tee "$run_dir/nextest.log"
    nextest_status=${PIPESTATUS[0]}
    set -e
else
    echo "🦀 Running all Rust unit and integration tests..."
    set +e
    cargo nextest run "${nextest_args[@]}" 2>&1 | tee "$run_dir/nextest.log"
    nextest_status=${PIPESTATUS[0]}
    set -e
fi

if [[ -f "$junit_source" && "$junit_source" -nt "$marker" ]]; then
    cp "$junit_source" "$junit_result"
fi

echo "📚 Running all Rust doctests..."
doctest_args=(--workspace --doc -- --test-threads "$test_threads")
if [[ $verbose -eq 1 ]]; then
    doctest_args+=(--nocapture)
fi
set +e
cargo test "${doctest_args[@]}" 2>&1 | tee "$doctest_log"
doctest_status=${PIPESTATUS[0]}
set -e

if [[ $coverage -eq 1 ]]; then
    echo "📈 Rendering Rust source coverage..."
    coverage_status=0
    cargo llvm-cov report --html --output-dir "$run_dir/coverage" || coverage_status=$?
    json_status=0
    cargo llvm-cov report --json --summary-only --output-path "$coverage_json" || json_status=$?
    if [[ $coverage_status -eq 0 && $json_status -ne 0 ]]; then
        coverage_status=$json_status
    fi
    
    report_status=0
    python3 "$repo_root/scripts/rust_test_report.py" \
        --junit "$junit_result" \
        --nextest-log "$run_dir/nextest.log" \
        --doctest-log "$doctest_log" \
        --coverage-summary "$coverage_json" \
        --coverage-html "$coverage_html" \
        --output "$report_root/latest.html" \
        --nextest-status "$nextest_status" \
        --doctest-status "$doctest_status" \
        --coverage-status "$coverage_status" || report_status=$?
else
    coverage_status=0
    report_status=0
    python3 "$repo_root/scripts/rust_test_report.py" \
        --junit "$junit_result" \
        --nextest-log "$run_dir/nextest.log" \
        --doctest-log "$doctest_log" \
        --output "$report_root/latest.html" \
        --nextest-status "$nextest_status" \
        --doctest-status "$doctest_status" \
        --coverage-status "$coverage_status" || report_status=$?
fi

if [[ $nextest_status -ne 0 ]]; then
    exit "$nextest_status"
fi
if [[ $doctest_status -ne 0 ]]; then
    exit "$doctest_status"
fi
if [[ $coverage_status -ne 0 ]]; then
    exit "$coverage_status"
fi
exit "$report_status"
