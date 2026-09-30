#!/usr/bin/env bash
set -euo pipefail

mode="standard"
live_evals="ask"

set_live_eval_choice() {
    # Case-insensitive so an interactive "Y"/"N"/"YES" answers just like the
    # lowercase form (macOS bash 3.2 lacks ${var,,}, so lowercase via tr).
    case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in
        1|true|yes|on|y) live_evals="true" ;;
        0|false|no|off|n|"") live_evals="false" ;;
        *) return 1 ;;
    esac
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --verbose) mode="verbose" ;;
        --live-evals) live_evals="true" ;;
        --no-live-evals) live_evals="false" ;;
        --live-evals=*)
            live_eval_value="${1#--live-evals=}"
            if ! set_live_eval_choice "$live_eval_value"; then
                echo "Invalid live-eval choice '$live_eval_value'; use true or false." >&2
                exit 2
            fi
            ;;
        -h|--help)
            echo "Usage: $0 [--verbose] [--live-evals|--no-live-evals|--live-evals=true|false]"
            exit 0
            ;;
        *)
            echo "Usage: $0 [--verbose] [--live-evals|--no-live-evals|--live-evals=true|false]" >&2
            exit 2
            ;;
    esac
    shift
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
make_bin="${MAKE_BIN:-make}"
report_root="${TEST_SUMMARY_REPORT_DIR:-$repo_root/coverage}"
rust_report_path="${RUST_TEST_REPORT_DIR:-$repo_root/coverage/rust}/latest.html"
ui_report_path="${UI_TEST_REPORT_DIR:-$repo_root/coverage/frontend}/latest.html"
live_eval_report_path="${LIVE_EVAL_REPORT_DIR:-$repo_root/coverage/evals/live-suite}/latest.html"
live_eval_manifest_path="${LIVE_EVAL_REPORT_DIR:-$repo_root/coverage/evals/live-suite}/latest.json"
attention_historical_eval_report_path="${ATTENTION_HISTORICAL_BOOTSTRAP_EVAL_REPORT:-$report_root/evals/attention-historical-bootstrap/latest.json}"
run_id="$(date '+%Y%m%d-%H%M%S')-$$"
run_dir="$report_root/summary/results/$run_id"
summary_path="$report_root/latest.html"
archive_path="$run_dir/index.html"
manifest_path="$run_dir/manifest.json"
started_at="$(date '+%Y-%m-%d %H:%M:%S %Z')"
started_epoch="$(date '+%s')"

rust_target="${TEST_SUITE_RUST_TARGET:-test-rust}"
ui_target="${TEST_SUITE_UI_TARGET:-test-ui}"
if [[ "$mode" == "verbose" ]]; then
    rust_target="${TEST_SUITE_RUST_TARGET:-test-rust-verbose}"
    ui_target="${TEST_SUITE_UI_TARGET:-test-ui-verbose}"
fi
desktop_target="${TEST_SUITE_DESKTOP_TARGET:-test-desktop-tray}"
android_target="${TEST_SUITE_ANDROID_TARGET:-test-magdroid}"

macos_audio_target="${TEST_SUITE_MACOS_AUDIO_TARGET:-test-macos-audio-engine}"
offline_audio_eval_target="${TEST_SUITE_OFFLINE_AUDIO_EVAL_TARGET:-test-media-offline-audio-eval}"
attention_historical_eval_target="${TEST_SUITE_ATTENTION_HISTORICAL_EVAL_TARGET:-test-attention-historical-bootstrap-eval}"
authorization_eval_target="${TEST_SUITE_AUTHORIZATION_EVAL_TARGET:-test-agent-tool-visibility-eval-harness}"
surface_runtime_eval_target="${TEST_SUITE_SURFACE_RUNTIME_EVAL_TARGET:-test-agent-surface-runtime-eval}"
tool_projection_eval_target="${TEST_SUITE_TOOL_PROJECTION_EVAL_TARGET:-test-tool-result-projection-context-eval-harness}"
provider_replay_eval_target="${TEST_SUITE_PROVIDER_REPLAY_EVAL_TARGET:-test-provider-replay-eval-harness}"
preplan_eval_target="${TEST_SUITE_PREPLAN_EVAL_TARGET:-test-preplan-flow-eval-harness}"
web_researcher_eval_target="${TEST_SUITE_WEB_RESEARCHER_EVAL_TARGET:-test-web-researcher-eval-harness}"
runtime_performance_eval_target="${TEST_SUITE_RUNTIME_PERFORMANCE_EVAL_TARGET:-test-runtime-performance-eval-harness}"
content_retrieval_eval_target="${TEST_SUITE_CONTENT_RETRIEVAL_EVAL_TARGET:-test-content-retrieval-eval-harness}"
test_runner_target="${TEST_SUITE_RUNNER_TARGET:-test-suite-runner}"
ios_target="${TEST_SUITE_IOS_TARGET:-test-ios}"

mkdir -p "$run_dir"

suite_status=0
suite_state="passed"
suite_report=""

run_suite() {
    local name="$1"
    local target="$2"
    local expected_report="$3"
    local skip_hint="$4"
    local marker="$run_dir/${name}.started"

    echo
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "▶ Running $name suite (make $target)"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    touch "$marker"
    suite_status=0
    set +e
    "$make_bin" --no-print-directory "$target"
    suite_status=$?
    set -e
    if [[ $suite_status -ne 0 ]]; then
        suite_state="failed"
    elif [[ "$skip_hint" == "skipped" ]]; then
        suite_state="skipped"
    else
        suite_state="passed"
    fi
    suite_report=""
    if [[ -n "$expected_report" && -f "$expected_report" && "$expected_report" -nt "$marker" ]]; then
        suite_report="$expected_report"
    fi
}

run_suite "rust" "$rust_target" "$rust_report_path" ""
rust_status=$suite_status
rust_state=$suite_state
rust_report=$suite_report

run_suite "frontend" "$ui_target" "$ui_report_path" ""
ui_status=$suite_status
ui_state=$suite_state
ui_report=$suite_report

extension_skip=""
if ! command -v node >/dev/null 2>&1; then
    extension_skip="skipped"
fi
run_suite "magicutor-extension" "test-magicutor-extension" "" "$extension_skip"
extension_status=$suite_status
extension_state=$suite_state

desktop_skip=""
if [[ "$(uname -s)" != "Darwin" ]] || ! command -v npm >/dev/null 2>&1; then
    desktop_skip="skipped"
fi
run_suite "desktop" "$desktop_target" "" "$desktop_skip"
desktop_status=$suite_status
desktop_state=$suite_state

macos_skip=""
if [[ "$(uname -s)" != "Darwin" ]] || ! command -v swift >/dev/null 2>&1; then
    macos_skip="skipped"
fi
macos_status=$suite_status
macos_state=$suite_state

run_suite "macos-audio" "$macos_audio_target" "" "$macos_skip"
macos_audio_status=$suite_status
macos_audio_state=$suite_state

run_suite "offline-audio-eval" "$offline_audio_eval_target" "" ""
offline_audio_eval_status=$suite_status
offline_audio_eval_state=$suite_state

run_suite "attention-historical-bootstrap-eval" "$attention_historical_eval_target" "$attention_historical_eval_report_path" ""
attention_historical_eval_status=$suite_status
attention_historical_eval_state=$suite_state
attention_historical_eval_report=$suite_report

run_suite "authorization-eval-harness" "$authorization_eval_target" "" ""
authorization_eval_status=$suite_status
authorization_eval_state=$suite_state

surface_runtime_eval_report="${AGENT_SURFACE_RUNTIME_EVAL_REPORT_DIR:-$report_root/evals/agent-surface-runtime}/latest.html"
run_suite "surface-runtime-eval" "$surface_runtime_eval_target" "$surface_runtime_eval_report" ""
surface_runtime_eval_status=$suite_status
surface_runtime_eval_state=$suite_state
surface_runtime_eval_report=$suite_report

tool_projection_eval_report="${TOOL_RESULT_PROJECTION_EVAL_REPORT_DIR:-$report_root/evals/tool-result-projection-context/deterministic}/report.html"
run_suite "tool-result-projection-context-eval" "$tool_projection_eval_target" "$tool_projection_eval_report" ""
tool_projection_eval_status=$suite_status
tool_projection_eval_state=$suite_state
tool_projection_eval_report=$suite_report

provider_replay_eval_report="${PROVIDER_REPLAY_EVAL_REPORT_DIR:-$report_root/evals/provider-replay/deterministic}/report.html"
run_suite "provider-replay-eval" "$provider_replay_eval_target" "$provider_replay_eval_report" ""
provider_replay_eval_status=$suite_status
provider_replay_eval_state=$suite_state
provider_replay_eval_report=$suite_report

preplan_eval_report="${PREPLAN_FLOW_EVAL_REPORT_DIR:-$report_root/evals/preplan-flow/deterministic/latest}/report.html"
run_suite "preplan-flow-eval" "$preplan_eval_target" "$preplan_eval_report" ""
preplan_eval_status=$suite_status
preplan_eval_state=$suite_state
preplan_eval_report=$suite_report

web_researcher_eval_report="${WEB_RESEARCHER_EVAL_REPORT_DIR:-$report_root/evals/web-researcher/deterministic/latest}/report.html"
run_suite "web-researcher-eval" "$web_researcher_eval_target" "$web_researcher_eval_report" ""
web_researcher_eval_status=$suite_status
web_researcher_eval_state=$suite_state
web_researcher_eval_report=$suite_report

runtime_performance_eval_report="${RUNTIME_PERFORMANCE_EVAL_REPORT_DIR:-$report_root/evals/runtime-performance/deterministic/latest}/report.html"
run_suite "runtime-performance-eval" "$runtime_performance_eval_target" "$runtime_performance_eval_report" ""
runtime_performance_eval_status=$suite_status
runtime_performance_eval_state=$suite_state
runtime_performance_eval_report=$suite_report

run_suite "content-retrieval-eval" "$content_retrieval_eval_target" "" ""
content_retrieval_eval_status=$suite_status
content_retrieval_eval_state=$suite_state

run_suite "test-runner" "$test_runner_target" "" ""
test_runner_status=$suite_status
test_runner_state=$suite_state

android_skip=""
android_java_home="${MAGDROID_JAVA_HOME:-}"
android_sdk="${MAGDROID_SDK:-${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}}"
if [[ -z "$android_java_home" || ! -d "$android_java_home" || -z "$android_sdk" || ! -d "$android_sdk" ]]; then
    android_skip="skipped"
fi
run_suite "android" "$android_target" "" "$android_skip"
android_status=$suite_status
android_state=$suite_state

ios_skip=""
if ! command -v xcodebuild >/dev/null 2>&1; then
    ios_skip="skipped"
fi
ios_correlation_id="$run_id:ios"
ios_run_manifest="$run_dir/ios-artifact.json"
export IOS_TEST_CORRELATION_ID="$ios_correlation_id"
export IOS_TEST_RUN_MANIFEST="$ios_run_manifest"
run_suite "ios" "$ios_target" "" "$ios_skip"
unset IOS_TEST_CORRELATION_ID IOS_TEST_RUN_MANIFEST
ios_status=$suite_status
ios_state=$suite_state
ios_report=""
if [[ -f "$ios_run_manifest" && "$ios_run_manifest" -nt "$run_dir/ios.started" ]]; then
    ios_report="$(python3 -c 'import json,sys; data=json.load(open(sys.argv[1], encoding="utf-8")); report=data.get("report_path"); manifest_status=data.get("exit_status"); make_status=int(sys.argv[3]); status_matches=isinstance(manifest_status, int) and ((manifest_status == 0) == (make_status == 0)); valid=data.get("schema_version") == 1 and data.get("correlation_id") == sys.argv[2] and status_matches; print(report if valid and report else "")' "$ios_run_manifest" "$ios_correlation_id" "$ios_status" 2>/dev/null || true)"
    if [[ -z "$ios_report" || ! -f "$ios_report" || ! "$ios_report" -nt "$run_dir/ios.started" ]]; then
        ios_report=""
    fi
fi

echo
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✓ All non-live test suites are complete"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [[ "$live_evals" == "ask" ]]; then
    prompt_answer="${TEST_SUITE_LIVE_EVAL_PROMPT_RESPONSE:-}"
    if [[ -n "$prompt_answer" ]]; then
        if ! set_live_eval_choice "$prompt_answer"; then
            echo "Invalid TEST_SUITE_LIVE_EVAL_PROMPT_RESPONSE '$prompt_answer'." >&2
            exit 2
        fi
    elif [[ -t 0 && -t 1 && -r /dev/tty && -w /dev/tty ]]; then
        while true; do
            # Discard any buffered type-ahead accumulated during the long
            # non-live test run BEFORE reading, so a stray keystroke can't
            # silently auto-answer this prompt (the previous bug: live evals
            # ran without a real entry). Integer read timeout keeps this
            # portable to macOS's stock bash 3.2.
            while IFS= read -r -t 1 _drain < /dev/tty 2>/dev/null; do :; done
            printf "Run live provider/model evals now? [y/N] " > /dev/tty
            # Block for a real answer; EOF/interrupt -> empty -> treated as No.
            if ! IFS= read -r prompt_answer < /dev/tty; then
                prompt_answer=""
            fi
            if set_live_eval_choice "$prompt_answer"; then
                break
            fi
            printf "Please answer yes or no.\n" > /dev/tty
        done
    else
        echo "No interactive terminal and no explicit live_evals value; skipping live evals."
        live_evals="false"
    fi
fi

live_eval_target="${TEST_SUITE_LIVE_EVAL_TARGET:-test-live-evals}"
live_eval_manifest_for_run=""
if [[ "$live_evals" == "true" ]]; then
    run_suite "live-evals" "$live_eval_target" "$live_eval_report_path" ""
    live_eval_status=$suite_status
    live_eval_state=$suite_state
    live_eval_report=$suite_report
    live_eval_marker="$run_dir/live-evals.started"
    if [[ -f "$live_eval_manifest_path" && "$live_eval_manifest_path" -nt "$live_eval_marker" ]]; then
        live_eval_manifest_for_run="$live_eval_manifest_path"
    fi
else
    live_eval_status=0
    live_eval_state="skipped"
    live_eval_report=""
fi

ended_epoch="$(date '+%s')"
duration_seconds=$(( ended_epoch - started_epoch ))
report_status=0
summary_args=(
    --output "$summary_path"
    --archive-output "$archive_path"
    --manifest "$manifest_path"
    --mode "$mode"
    --run-id "$run_id"
    --started-at "$started_at"
    --duration-seconds "$duration_seconds"
    --suite "Rust" "$rust_state" "$rust_status" "$rust_target" "Workspace unit, integration, and doctest coverage" "html" "$rust_report"
    --suite "Unified UI" "$ui_state" "$ui_status" "$ui_target" "Vitest unit and mounted Svelte component coverage" "html" "$ui_report"
    --suite "Magicutor extension" "$extension_state" "$extension_status" "test-magicutor-extension" "WebSocket lifecycle and extension helper tests" "status" ""
    --suite "Desktop tray" "$desktop_state" "$desktop_status" "$desktop_target" "Tauri tray and gateway checks" "status" ""
    --suite "macOS audio engine" "$macos_audio_state" "$macos_audio_status" "$macos_audio_target" "FluidAudio protocol, PCM, and lifecycle tests" "status" ""
    --suite "Offline audio evaluator" "$offline_audio_eval_state" "$offline_audio_eval_status" "$offline_audio_eval_target" "Provider-free VAD, STT, TTS, diarization metric and schema tests" "status" ""
    --suite "Attention historical bootstrap evaluator" "$attention_historical_eval_state" "$attention_historical_eval_status" "$attention_historical_eval_target" "Frozen migration, repair, task-specific neighbor, and within-lane replay contract" "json" "$attention_historical_eval_report"
    --suite "Authorization eval harness" "$authorization_eval_state" "$authorization_eval_status" "$authorization_eval_target" "Production-catalog validation, baseline gates, profile routing, and HTML-report self-tests" "status" ""
    --suite "Agent surface runtime eval" "$surface_runtime_eval_state" "$surface_runtime_eval_status" "$surface_runtime_eval_target" "Provider-free production cache, family-loading, authorization-intersection, and revision parity gate" "html" "$surface_runtime_eval_report"
    --suite "Tool-result projection and staged context" "$tool_projection_eval_state" "$tool_projection_eval_status" "$tool_projection_eval_target" "Provider-free projection, continuation, provider-pairing, staged-deadline, privacy, and cross-surface matrix" "html" "$tool_projection_eval_report"
    --suite "Provider replay harness" "$provider_replay_eval_state" "$provider_replay_eval_status" "$provider_replay_eval_target" "Provider-free cross-provider repair matrix, checkpoint, profile-selection, and report contracts" "html" "$provider_replay_eval_report"
    --suite "Pre-plan lifecycle harness" "$preplan_eval_state" "$preplan_eval_status" "$preplan_eval_target" "Provider-free mapped-profile, fixture-answer, Plan-panel and Attention identity, HITL resolution, and report contracts" "html" "$preplan_eval_report"
    --suite "Web researcher harness" "$web_researcher_eval_state" "$web_researcher_eval_status" "$web_researcher_eval_target" "Provider-free task, direct/delegated lineage, citation, latency-gate, tool-lifecycle, telemetry, and report contracts" "html" "$web_researcher_eval_report"
    --suite "Runtime performance harness" "$runtime_performance_eval_state" "$runtime_performance_eval_status" "$runtime_performance_eval_target" "Provider-free RSS sampling, bounded store growth, crash-log slicing, source-report metrics, baseline comparison, and report contracts" "html" "$runtime_performance_eval_report"
    --suite "Content retrieval eval harness" "$content_retrieval_eval_state" "$content_retrieval_eval_status" "$content_retrieval_eval_target" "Provider-free adapter fixtures, live-harness contracts, sanitization, and evaluator self-test" "status" ""
    --suite "Composed test runner" "$test_runner_state" "$test_runner_status" "$test_runner_target" "Live-eval final-stage ordering, interactive prompt, explicit true/false, and non-interactive behavior" "status" ""
    --suite "Android (Magdroid)" "$android_state" "$android_status" "$android_target" "Gradle unit tests for the app and bridge modules" "status" ""
    --suite "iOS (Magios)" "$ios_state" "$ios_status" "$ios_target" "XCTest unit and source coverage; UI smoke tests are on demand" "html" "$ios_report"
    --suite "Live LLM evals" "$live_eval_state" "$live_eval_status" "$live_eval_target" "Opt-in real-provider semantic, routing, token, cost, and latency evals" "html" "$live_eval_report"
)
if [[ -n "$live_eval_manifest_for_run" ]]; then
    summary_args+=(--suite-manifest "Live eval · " "$live_eval_manifest_for_run")
fi
python3 "$repo_root/scripts/test_suite_summary_report.py" "${summary_args[@]}" \
    || report_status=$?

for status in "$rust_status" "$ui_status" "$extension_status" "$desktop_status" "$macos_status" "$macos_audio_status" "$offline_audio_eval_status" "$attention_historical_eval_status" "$authorization_eval_status" "$surface_runtime_eval_status" "$tool_projection_eval_status" "$provider_replay_eval_status" "$preplan_eval_status" "$web_researcher_eval_status" "$runtime_performance_eval_status" "$content_retrieval_eval_status" "$test_runner_status" "$android_status" "$ios_status" "$live_eval_status"; do
    if [[ $status -ne 0 ]]; then
        exit "$status"
    fi
done
exit "$report_status"
