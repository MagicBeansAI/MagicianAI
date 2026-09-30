#!/usr/bin/env bash
# install.sh — one composed installer for the magician stack (local + container, dev + user).
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# --- inputs (flags > env > prompt > default) ---
MODE="${MAGICIAN_INSTALL_MODE:-}"          # dev (build) | user (fetch prebuilt)
FLOW="${MAGICIAN_INSTALL_FLOW:-}"          # local | container
DATA_DIR="${MAGICIAN_ROOT_DIR:-}"          # default $HOME/MagicianNotes
RUNTIME="${MAGICIAN_CONTAINER_RUNTIME:-}"  # docker | apple-container; empty = auto-detect by OS/arch
# RUNTIME is UNSET by default and auto-detected from the host below
# (detect_container_runtime: Apple Silicon macOS >= 26 -> apple-container, else
# docker), so the operator never has to pick. An explicit --runtime / env
# MAGICIAN_CONTAINER_RUNTIME overrides it. The auto-detect resolves to a concrete
# runtime before any phase, so phase_run/phase_build's `case` never falls through
# to its `*)` die.
IMAGE_REF="${MAGICIAN_IMAGE_REF:-ghcr.io/magicbeanbs100x/magician:latest}"
# --- component gates ---
# Only MAGICIAN_ENABLE_FUNNEL exists, and it is opt-in (default 0). Gates for
# Ollama and the desktop tray were added and removed the same day:
# the mechanism is easy, but nothing downstream knows how to behave when a
# component is absent, so a gate produced a silently broken install rather than a
# smaller one. See docs/components/scripts/README.md for what has to exist first.
#
# ONE consistent container name across every container path (build/run/rm/stop).
# The Makefile's own targets default to `magician-dev`; we pick our own default so
# the installer fully owns its container lifecycle (start/replace/stop) without
# depending on the Makefile's name, and never collides with a hand-run dev one.
CONTAINER_NAME="${MAGICIAN_CONTAINER_NAME:-magician}"
RELEASE_URL="${MAGICIAN_RELEASE_URL:-}"
RELEASE_SHA256="${MAGICIAN_RELEASE_SHA256:-}"
RELEASE_SHA256_URL="${MAGICIAN_RELEASE_SHA256_URL:-}"
ASSUME_YES="${MAGICIAN_INSTALL_YES:-0}"
# Local dev builds default to DEBUG (the repo convention — phase_tauri already uses
# the -debug targets). Opt into release explicitly with MAGICIAN_INSTALL_RELEASE=1.
INSTALL_RELEASE="${MAGICIAN_INSTALL_RELEASE:-0}"

# setup-ollama-host.sh resolves model tags directly from the runtime config's
# embedding contract and mapped Ollama profiles. The installer does not carry
# a second model default.
ONLY_VERIFY=0
while [ $# -gt 0 ]; do case "$1" in
  --mode) MODE="$2"; shift 2;;
  --flow) FLOW="$2"; shift 2;;
  --data-dir) DATA_DIR="$2"; shift 2;;
  --runtime) RUNTIME="$2"; shift 2;;
  --yes) ASSUME_YES=1; shift;;
  --verify) ONLY_VERIFY=1; shift;;
  *) echo "unknown arg: $1" >&2; exit 2;;
esac; done

prompt() { # prompt VAR "question" "default"
  local __v="$1" q="$2" d="$3" ans
  if [ -n "${!__v:-}" ]; then return; fi
  if [ "$ASSUME_YES" = 1 ]; then printf -v "$__v" '%s' "$d"; return; fi
  read -r -p "$q [$d]: " ans || true; printf -v "$__v" '%s' "${ans:-$d}"
}

log()  { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33mWARN: %s\033[0m\n' "$*" >&2; }
die()  { printf '\033[1;31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

# Auto-select the container runtime from the host when none was given. Native
# Apple `container` is the ONLY runtime that ships on Apple Silicon macOS 26+;
# everything else (pre-26 / Intel macOS / Linux) uses Docker (via Colima on mac).
# Mirrors scripts/setup-container-runtime.sh + desktop/src-tauri/src/container so
# the choice is identical across installer, bootstrap, and the desktop app.
detect_container_runtime() {
  if [ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ]; then
    local major
    major="$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)"
    if [ -n "$major" ] && [ "$major" -ge 26 ] 2>/dev/null; then
      echo apple-container
      return
    fi
  fi
  echo docker
}

ensure_apple_container_host_services() {
  local domain="host.container.internal" localhost_alias="203.0.113.113"
  local container_cli
  container_cli="$(command -v container)"
  if container system dns list --quiet 2>/dev/null | grep -Fxq "$domain"; then
    log "Apple Container host-service forwarding is configured ($domain)."
    return 0
  fi

  log "Configuring Apple Container access to host Ollama and desktop services"
  warn "Apple Container's localhost forwarding disables iCloud Private Relay while active; the system service restores its forwarding rule after host restart."
  sudo "$container_cli" system dns create "$domain" --localhost "$localhost_alias" \
    || die "failed to configure Apple Container localhost forwarding for $domain"
  container system dns list --quiet 2>/dev/null | grep -Fxq "$domain" \
    || die "Apple Container did not retain the required $domain localhost domain"
}

# DRYRUN: when MAGICIAN_INSTALL_DRYRUN=1, the heavy phases only announce
# themselves ("would run: <phase>") so the orchestration + the non-clobber logic
# can be verified without installing anything. phase_runtime_root deliberately
# runs FOR REAL even in dry-run (it touches only the data dir, never the host)
# so the non-clobber guarantee is exercised.
DRYRUN="${MAGICIAN_INSTALL_DRYRUN:-0}"
run_phase() { # run_phase <phase_fn>
  if [ "$DRYRUN" = 1 ]; then log "would run: $1"; return 0; fi
  "$1"
}

# gate NAME DEFAULT — read MAGICIAN_ENABLE_<NAME>, falling back to DEFAULT.
#
# Only the Cloudflare Tunnel uses this today. It earns its place by validating:
# a value that is neither on nor off is fatal rather than coerced, so
# MAGICIAN_ENABLE_FUNNEL=ture fails loudly instead of silently meaning off.
#
# It is also the seam for real per-component gates once the runtime can answer
# "is this component present" and the surfaces can hide what depends on an absent
# one. Adding a gate before then yields a broken install, not a smaller one.
gate() { # gate NAME DEFAULT -> 0 when enabled, 1 when disabled
  local __name="MAGICIAN_ENABLE_$1" __value
  # `${!name:-}` not `${!name}`: this script runs under `set -u`, where indirect
  # expansion of an unset variable is a fatal error, which would make the common
  # case — no gate set at all — abort the install.
  __value="${!__name:-}"
  [ -n "$__value" ] || __value="$2"
  case "$__value" in
    1|true|yes|on)  return 0;;
    0|false|no|off) return 1;;
    *) die "$__name must be 1 or 0 (got '$__value')";;
  esac
}


# copy_if_absent SRC DEST — the non-clobber primitive. Copies SRC to DEST only
# when DEST does not already exist; an existing DEST is NEVER overwritten.
copy_if_absent() {
  local src="$1" dest="$2"
  mkdir -p "$(dirname "$dest")"
  if [ -e "$dest" ]; then
    log "kept existing $dest"
  elif [ -e "$src" ]; then
    cp "$src" "$dest"
    log "seeded $dest"
  else
    warn "seed template missing, cannot seed $dest (source: $src)"
  fi
}

config_seed_source() {
  printf '%s\n' "$REPO/magician-config.yaml"
}

seed_default_harness_runtime_files() {
  local seed_root="$REPO/magician_data_v3/scopes/anonymous/default"
  local runtime_root="$DATA_DIR/scopes/anonymous/default"

  copy_if_absent "$seed_root/programs/harness_reliability.md" \
                 "$runtime_root/programs/harness_reliability.md"

  local agent
  for agent in harness-sre cto internal-system-analyst; do
    copy_if_absent "$seed_root/agent_runtime/agents/$agent/definition.agent.yaml" \
                   "$runtime_root/agent_runtime/agents/$agent/definition.agent.yaml"
  done
}

# health_wait URL SECS — poll URL until it returns 2xx, up to SECS seconds.
# Returns 0 on first healthy response, 1 if it never came up. Bounded so the
# installer can detach the (foreground-blocking) backend and still return.
health_wait() {
  local url="$1" secs="${2:-30}" i
  for ((i = 0; i < secs; i++)); do
    if curl -fsS "$url" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  return 1
}

# Verify a downloaded user-mode installer before mounting or executing it.
# An explicit digest wins; otherwise releases publish a sibling `.sha256` asset.
verify_release_sha256() {
  local artifact="$1" expected="$RELEASE_SHA256" checksum_url="$RELEASE_SHA256_URL"
  local checksum_file actual

  if [[ -z "$expected" ]]; then
    [[ -n "$checksum_url" ]] || checksum_url="${RELEASE_URL}.sha256"
    checksum_file="$(mktemp -t magician-release-sha256)"
    if ! curl -fSL "$checksum_url" -o "$checksum_file"; then
      rm -f "$checksum_file"
      die "failed to download required release checksum from $checksum_url"
    fi
    expected="$(awk 'NF { print $1; exit }' "$checksum_file")"
    rm -f "$checksum_file"
  fi

  expected="$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')"
  if [[ ! "$expected" =~ ^[0-9a-f]{64}$ ]]; then
    die "release checksum must be exactly 64 hexadecimal SHA-256 characters"
  fi
  actual="$(shasum -a 256 "$artifact" | awk '{ print $1 }')"
  if [[ "$actual" != "$expected" ]]; then
    die "release checksum mismatch for $artifact (expected $expected, got $actual)"
  fi
  log "  SHA-256 checksum verified."
}

# ---------------------------------------------------------------------------
# phase_prereqs — foundational host CLI tools (+ container runtime for FLOW=container).
# Non-fatal: a failing optional prereq step is logged loudly but does not abort
# the whole install (these are best-effort host conveniences).
phase_prereqs() {
  log "Installing host prerequisites"
  # MODE=user doesn't build skills natively (it fetches the container image / the
  # signed .app), so use the slim prereq path — skip the native-skill build deps
  # (node/python3/uv/imagemagick/poppler), keep brew/git/cloudflared + container runtime.
  local _slim=0
  [ "$MODE" = user ] && _slim=1
  # Pass the resolved MODE/FLOW through so install-prerequisites.sh installs the
  # build toolchain (make + rust) ONLY for the MODE=dev + FLOW=local combo (the
  # one that compiles binaries on the host). Every other combo fetches/runs an
  # image and never needs the host toolchain.
  if ! MAGICIAN_PREREQS_SLIM="$_slim" MAGICIAN_INSTALL_MODE="$MODE" MAGICIAN_INSTALL_FLOW="$FLOW" \
       bash "$REPO/scripts/install-prerequisites.sh"; then
    warn "install-prerequisites.sh reported a failure — continuing (non-fatal)"
  fi
  if [ "$FLOW" = container ]; then
    # Idempotent: auto-selects Apple `container` (macOS>=26 + arm64) else
    # Docker/Colima; may prompt for admin once on first run. We pass the CHOSEN
    # $RUNTIME via MAGICIAN_CONTAINER_RUNTIME so the bootstrap honours an explicit
    # docker choice on an Apple-eligible Mac (otherwise its USE_APPLE auto-detect
    # would bootstrap the Apple `container` runtime instead).
    log "Bootstrapping container runtime (host)"
    if ! MAGICIAN_CONTAINER_RUNTIME="$RUNTIME" bash "$REPO/scripts/setup-container-runtime.sh"; then
      warn "setup-container-runtime.sh failed — container build/run will not work until fixed"
    fi
    if [ "$RUNTIME" = apple-container ]; then
      ensure_apple_container_host_services
    fi
  fi
}

# ---------------------------------------------------------------------------
# phase_pi — the Pi coding agent CLI (MIT). REQUIRED, unlike phase_prereqs:
# Pi is a first-class harness engine (chat mouth, run engine, run_coding_task,
# op-harness-pi), and the runtime lists an engine only when its binary is on
# PATH — a host without `pi` silently loses every Pi surface. So a failure here
# aborts the install. setup-pi-coding-agent.sh is idempotent and pins the exact
# version the Rust RPC gate reviewed; it needs Node >= 22.19, which
# phase_prereqs installs for this flow.
#
# FLOW=local only: under FLOW=container the runtime runs inside the image, so a
# host `pi` would never be seen by it.
phase_pi() {
  [ "$FLOW" = local ] || return 0
  log "Installing the Pi coding agent CLI (required harness engine)"
  bash "$REPO/scripts/setup-pi-coding-agent.sh" || die "Pi coding agent install failed — Pi is required; fix the error above and rerun"
}

# ---------------------------------------------------------------------------
# phase_runtime_root — THE NON-CLOBBER GUARANTEE.
#
# Runtime roots own their live config. The git-tracked repo-root
# magician-config.yaml, llm-router.yaml, and decision-engine.yaml are the
# dev/package seeds. We copy
# only when the runtime root is missing the file, so operator edits survive
# installer reruns.
phase_runtime_root() {
  log "Preparing runtime data dir: $DATA_DIR"
  mkdir -p "$DATA_DIR" "$DATA_DIR/secrets" "$DATA_DIR/Inbox"

  local populated=0
  [ -e "$DATA_DIR/magician-config.yaml" ] && populated=1
  local config_seed
  config_seed="$(config_seed_source)"

  # Always seed configs non-clobberingly.
  # An existing file (operator's real config/secrets) survives untouched.
  copy_if_absent "$config_seed"                                           "$DATA_DIR/magician-config.yaml"
  copy_if_absent "$REPO/llm-router.yaml"                                  "$DATA_DIR/llm-router.yaml"
  copy_if_absent "$REPO/decision-engine.yaml"                            "$DATA_DIR/decision-engine.yaml"
  copy_if_absent "$REPO/magician_data_v3/.env.example"                    "$DATA_DIR/.env"
  copy_if_absent "$REPO/magician_data_v3/operator-config.template.yaml"   "$DATA_DIR/operator-config.yaml"
  seed_default_harness_runtime_files

  if [ "$populated" = 1 ]; then
    log "$DATA_DIR already populated — skipping seed-silverbullet-space.sh; skeleton + non-clobber config guards applied above."
  else
    # Fresh root: let the seed script do the full initialization (Space
    # skeleton, optional scopes/ migration, dataless check). The script is also
    # non-clobbering for magician-config.yaml.
    log "Fresh runtime root — running seed-silverbullet-space.sh"
    if ! MAGICIAN_ROOT_DIR="$DATA_DIR" bash "$REPO/scripts/seed-silverbullet-space.sh"; then
      warn "seed-silverbullet-space.sh failed — runtime root may be incomplete"
    fi
  fi

  # Warn (don't fabricate) when operator secrets are absent. The templates above
  # are placeholders; the operator must supply real values before first run.
  [ -f "$DATA_DIR/operator-config.yaml" ] || warn "no operator-config.yaml in $DATA_DIR (operator secrets unset)"
  [ -f "$DATA_DIR/client_secret.json" ]   || warn "no client_secret.json in $DATA_DIR (Google OAuth secrets unset — gws skills will not auth)"
}

# ---------------------------------------------------------------------------
# phase_ollama — host Ollama + the embedding/summary models magician uses.
# Host service even under FLOW=container (the container reaches it over localhost).
phase_ollama() {
  log "Setting up host Ollama"
  if ! bash "$REPO/scripts/setup-ollama-host.sh"; then
    warn "setup-ollama-host.sh failed — embedding/summary ops will be unavailable until Ollama is up"
  fi
}

# ---------------------------------------------------------------------------
# phase_build (MODE=dev) — produce the runtime binaries.
phase_build() {
  if [ "$FLOW" = container ]; then
    # Build the container image per $RUNTIME and ensure the result is tagged as
    # $IMAGE_REF so phase_run/phase_fetch all reference the SAME image.
    case "$RUNTIME" in
      apple-container)
        # Apple `container` builds straight to the requested tag.
        log "Building container image (container build -t $IMAGE_REF)"
        container build -t "$IMAGE_REF" "$REPO" || die "container build failed"
        ;;
      *)
        # docker: `make build-container` produces $(CONTAINER_IMAGE):$(CONTAINER_TAG)
        # (defaults to magician:dev). Pass the tag through so the make target builds
        # $IMAGE_REF directly — no extra `docker tag` round-trip, and build/run agree.
        local _img="${IMAGE_REF%%:*}" _tag="latest"
        case "$IMAGE_REF" in *:*) _tag="${IMAGE_REF##*:}";; esac
        log "Building container image (make build-container -> $IMAGE_REF)"
        make -C "$REPO" build-container CONTAINER_IMAGE="$_img" CONTAINER_TAG="$_tag" \
          || die "build-container failed"
        ;;
    esac
    return 0
  fi
  if [ "$INSTALL_RELEASE" = 1 ]; then
    log "Building all release binaries (make build-all-release; MAGICIAN_INSTALL_RELEASE=1)"
    make -C "$REPO" build-all-release || die "build-all-release failed"
  else
    log "Building all debug binaries (make build-all-debug; set MAGICIAN_INSTALL_RELEASE=1 for release)"
    make -C "$REPO" build-all-debug || die "build-all-debug failed"
  fi
}

# phase_fetch (MODE=user) — obtain prebuilt artifacts instead of building.
phase_fetch() {
  if [ "$FLOW" = container ]; then
    case "$RUNTIME" in
      apple-container)
        log "Pulling image (container pull $IMAGE_REF)"
        container pull "$IMAGE_REF" || die "container pull failed"
        ;;
      *)
        log "Pulling image (docker pull $IMAGE_REF)"
        docker pull "$IMAGE_REF" || die "docker pull failed"
        ;;
    esac
    return 0
  fi
  # local user-mode: there is NO release asset for bare magician / magicutor /
  # magic-supervisor binaries today. Those binaries live ONLY inside the container
  # image; desktop releases ship .dmg/.AppImage (the tray), not the backend
  # CLIs. So we do NOT pretend to fetch a bundle that does not exist — fail loud
  # and point the operator at the two real paths.
  #
  # TODO(CI): when a release job uploads a host-arch backend tarball (magician +
  # magicutor + magic-supervisor) as a release asset, wire it here:
  #   [ -n "$RELEASE_URL" ] || die "..."
  #   curl -fsSL "$RELEASE_URL" | tar -xz -C "<install-prefix>"   # + checksum verify
  # Until then this path has nothing to download. (Validation above already dies
  # on this combo before any heavy phase; this is a belt-and-suspenders guard that
  # reuses the SAME message — UNSUPPORTED_USER_LOCAL_MSG — so it is defined once.)
  die "$UNSUPPORTED_USER_LOCAL_MSG"
}

# phase_run — start the stack.
#
# Container runs must expose the two "cross-into-host" planes correctly PER
# RUNTIME (the host-reach addressing differs):
#   - magicutor :3003 — the HOST Chrome extension dials INTO the container's
#     magicutor (cdp -> the user's profiled, signed-in host Chrome). So :3003
#     must be reachable FROM the host, and any HOST-native magicutor must be
#     stopped first so the extension binds the CONTAINER's :3003 (not a host one).
#   - Tauri host gateway :3017 — the CONTAINER reaches the HOST's gateway for mac
#     automation / voice (magician reads MAGICIAN_HOST_GATEWAY_URL).
#   - Ollama generation :11434 on the host (MAGICIAN_OLLAMA_BASE_URL).
#   - Ollama embeddings :11435 on the host (MAGICIAN_MEMORY_OLLAMA_URL).
#
# Host-reach addressing by runtime (see desktop/src-tauri/src/container/*.rs and
# docs/runbooks/2026-06-22-container-tauri-browser-local-e2e.md):
#   - docker on Linux        : --network host -> the container shares host loopback,
#                              so :3003/:3017 ARE the host's and host services are
#                              reachable at 127.0.0.1 (no env override needed).
#   - docker on macOS        : containers live in a Linux VM, so --network host does
#                              NOT reach the Mac. Publish -p 3003/3017 and reach host
#                              services via the Docker-provided host.docker.internal.
#   - Apple `container` (Mac): published ports forward to the container. The
#                              official host.container.internal localhost domain
#                              carries container-to-host Ollama and gateway traffic.
#
# NOTE (browser-cache wiring): the image bakes warm obscura (~/.cloakbrowser) +
# Chrome-for-Testing (~/.agent-browser) caches into /opt/browser-cache (Dockerfile
# BROWSER_CACHE_HOME). At runtime the browser skill subprocess gets HOME set to the
# scoped workdirs/home on the MOUNTED /data root; the runtime wires the scoped
# HOME's ~/.agent-browser / ~/.cloakbrowser to /opt/browser-cache AUTOMATICALLY,
# per dispatch (the dispatcher creates the cache symlinks when /opt/browser-cache
# exists, and is a no-op natively). No manual step is required.
phase_run() {
  if [ "$FLOW" != container ]; then
    # Start the BACKEND only (magician :3002 + magicutor :3003 via the supervisor)
    # and RETURN, so the later phases (tauri, funnel, verify) still run.
    #
    # Why NOT `make run-all`: run-all is a never-returning foreground process
    # (it ends in `wait $SUPERVISOR_PID` under a `trap … EXIT -> stop-all`). It
    # would block the installer here forever. run-all ALSO launches the desktop
    # tray (:3017) and the Vite UI; we deliberately do NOT use it because
    # phase_tauri owns the tray — letting run-all start it too would double-bind
    # :3017 (B2). Notes are Markdown files Magician serves itself; the public
    # tunnel is phase_tunnel's job.
    #
    # `make run-supervisor` itself blocks (run-supervisor.sh foregrounds
    # `magic-supervisor.bin | tee`), so we detach it with nohup and poll health.
    # The supervisor manages magician+magicutor only — it does NOT launch the
    # tray — so the tray is launched exactly once, by phase_tauri.
    local _runlog="$DATA_DIR/magician-run.log"
    log "Starting the backend detached (nohup make run-supervisor -> $_runlog)"
    nohup make -C "$REPO" run-supervisor >"$_runlog" 2>&1 &
    log "  backend pid: $! (detached; logs at $_runlog)"
    log "Waiting for Magician API health on http://127.0.0.1:3002/health (up to 30s)..."
    if health_wait "http://127.0.0.1:3002/health" 30; then
      log "  Magician API is healthy."
    else
      warn "Magician API did not report healthy within 30s — continuing; check $_runlog. The tray/verify phases will run regardless."
    fi
    return 0
  fi

  # Prepare persistent custody and verify the candidate image before stopping
  # any service. Failure must preserve the existing container and its mounts.
  local _engine _keyring_output _keyring_arg
  case "$RUNTIME" in
    apple-container) _engine="$(command -v container)" ;;
    *) _engine="$(command -v docker)" ;;
  esac
  _keyring_output="$(python3 "$REPO/scripts/prepare-container-keyring.py" \
    --data-dir "$DATA_DIR" --runtime "$RUNTIME" --engine "$_engine" \
    --container "$CONTAINER_NAME" --image "$IMAGE_REF" --format args)" \
    || die "Persistent keyring preflight failed; existing services were not changed"
  local _keyring_args=()
  while IFS= read -r _keyring_arg; do
    _keyring_args+=("$_keyring_arg")
  done <<< "$_keyring_output"

  # Stop any host-native magicutor so the host Chrome extension binds the
  # container's :3003 (cdp -> host Chrome), not a stale host one. Best-effort.
  log "Stopping any host-native magicutor (so the Chrome extension dials the container's :3003)"
  make -C "$REPO" stop-magicutor 2>/dev/null || true

  # Idempotent re-run: stop+remove any existing container of the SAME name so a
  # re-run is a clean REPLACE, not a `name already in use` error. Uses the single
  # $CONTAINER_NAME consistently across docker + apple paths (the Makefile's own
  # stop-container target names a DIFFERENT container, magician-dev, so we stop
  # ours ourselves here rather than delegating to it).
  case "$RUNTIME" in
    apple-container) container rm -f "$CONTAINER_NAME" 2>/dev/null || true ;;
    *)               docker    rm -f "$CONTAINER_NAME" 2>/dev/null || true ;;
  esac

  case "$RUNTIME" in
    docker)
      if [ "$(uname -s)" = Linux ]; then
        # Linux: --network host -> host loopback; the container shares host loopback,
        # so :3003 + :3017 ARE the host's own ports and host Ollama is
        # reachable at 127.0.0.1. We issue `docker run` DIRECTLY (NOT `make
        # run-container`, which runs the magician:dev tag under the magician-dev
        # name) so build/fetch/run all reference the SAME $IMAGE_REF + $CONTAINER_NAME.
        log "Running container (docker, Linux, --network host)"
        local _cmd=(docker run -d --name "$CONTAINER_NAME"
          --network host
          "${_keyring_args[@]}"
          "$IMAGE_REF")
        log "  exec: ${_cmd[*]}"
        "${_cmd[@]}" || die "docker run failed"
      else
        # docker on macOS: containers are in a Linux VM, so --network host is a no-op.
        # Publish only 3002/3003 to the Mac and reach host services via host.docker.internal.
        # We do NOT publish :3017 — the container never BINDS :3017 (the host gateway is
        # a HOST service that phase_tauri's tray binds); publishing it would collide with
        # that host tray. The container REACHES the host gateway via
        # MAGICIAN_HOST_GATEWAY_URL -> host.docker.internal:3017, which resolves to the
        # host regardless of -p (host.docker.internal is an outbound host alias, not a
        # published port).
        log "Running container (docker on macOS, host.docker.internal)"
        local _cmd=(docker run -d --name "$CONTAINER_NAME"
          "${_keyring_args[@]}"
          -p 3002:3002 -p 3003:3003
          -e MAGICIAN_CONTAINER_HOST=host.docker.internal
          -e MAGICIAN_HOST_GATEWAY_URL=http://host.docker.internal:3017
          "$IMAGE_REF")
        log "  exec: ${_cmd[*]}"
        "${_cmd[@]}" || die "docker run failed"
        log "  magicutor :3003 published to the host -> load the Chrome extension and point it at http://localhost:3003"
        log "  NOTE: the meeting/observation summary model reads its URL from config (no env override);"
        log "        point op-meeting-summary-local.api_base_url at host.docker.internal:11434 in the mounted config, or run native."
      fi
      ;;
    apple-container)
      ensure_apple_container_host_services
      log "Running container (Apple container; official localhost forwarding)"
      local _cmd=(container run -d --name "$CONTAINER_NAME"
        "${_keyring_args[@]}"
        -p 3002:3002 -p 3003:3003
        -e MAGICIAN_CONTAINER_HOST=host.container.internal
        -e MAGICIAN_HOST_GATEWAY_URL=http://host.container.internal:3017
        "$IMAGE_REF")
      log "  exec: ${_cmd[*]}"
      "${_cmd[@]}" || die "container run failed"
      export MAGICIAN_VERIFY_MAGICUTOR_URL="http://127.0.0.1:3003/"
      log "  Magicutor is published at $MAGICIAN_VERIFY_MAGICUTOR_URL for the host extension."
      ;;
    *)
      die "unknown container runtime '$RUNTIME' (expected docker | apple-container)"
      ;;
  esac
}

# ---------------------------------------------------------------------------
# phase_tauri — the macOS desktop tray (Tauri host gateway :3017) + native
# orb/voice services. This is pipeline phase 6: it runs AFTER the
# backend is up (phase_run) because the tray's host gateway is what the backend
# (and, under FLOW=container, the containerized backend) reaches for mac
# automation / voice / ambient presence.
#
# macOS-ONLY by design. On Linux there is no native desktop surface and the
# mac skills (macos_automation / imessage_send) are auto-hidden by the
# host_gateway availability gate, so we skip cleanly and return.
#
# Gating note (FLOW=container):
#   - container on Linux  -> skipped here via the Darwin guard below (headless;
#     mac skills hidden by the availability gate). Correct.
#   - container on macOS  -> STILL RUNS. The containerized backend relays mac
#     skills through the HOST gateway :3017 (MAGICIAN_HOST_GATEWAY_URL points at
#     host.docker.internal:3017 / the vmnet gateway), so the host tray must be
#     up. The Darwin check is the ONLY gate — flow is intentionally not consulted.
#
# cua-driver: the host gateway's `/host/ax/*` AX relay is now LIVE — it proxies to
# the host cua-driver daemon, so a containerized backend reaches host AX automation
# over :3017 (along with Screen / AppleScript / voice). This installer does not
# install CuaDriver itself (it needs the signed-in graphical session); it prints
# the pinned path: `make setup-cua-driver ARGS=--start` (or Desktop onboarding),
# then `cua-driver permissions grant` so the grants go to CuaDriver.app.
phase_tauri() {
  if [ "$(uname -s)" != Darwin ]; then
    log "skipping native desktop services (Linux headless; mac skills hidden by the host_gateway availability gate)"
    return 0
  fi

  log "Setting up the macOS desktop tray (host gateway :3017) + native voice/orb"

  if [ "$MODE" = user ]; then
    # user mode: install the prebuilt signed .app rather than building it.
    # A missing $RELEASE_URL must NOT abort the whole install (the backend is
    # already up and the health sweep + summary still need to run). When it IS
    # set (pointing at the host-arch Magician .dmg), do a real install; the Tauri
    # auto-updater then handles later updates, so this path stays inert until a
    # release is published.
    if [ -z "$RELEASE_URL" ]; then
      warn "MAGICIAN_RELEASE_URL is unset — skipping the desktop .app install (no published release to fetch yet; the Tauri auto-updater handles updates once one exists, or use --mode dev to build the tray from source). Install continues."
      return 0
    fi
    # Install the signed and notarized Magician.app from the host-arch .dmg at
    # $RELEASE_URL. This path is fail-closed once a URL is supplied: checksum,
    # disk-image Gatekeeper assessment, app signature, and app Gatekeeper
    # assessment must all pass before anything is copied into /Applications.
    local _dmg _mnt _app _dest _stage _backup
    _dmg="$(mktemp -t magician-dmg).dmg"
    log "Downloading Magician desktop .dmg from $RELEASE_URL"
    if ! curl -fSL "$RELEASE_URL" -o "$_dmg"; then
      rm -f "$_dmg"
      die "failed to download the .dmg from $RELEASE_URL"
    fi
    verify_release_sha256 "$_dmg"
    if ! spctl --assess --type open --context context:primary-signature --verbose=2 "$_dmg"; then
      rm -f "$_dmg"
      die "Gatekeeper rejected the downloaded Magician disk image"
    fi
    log "  disk-image signature and notarization assessment passed."
    # Attach the .dmg and discover its mount point from hdiutil's plist output
    # (the volume name is not fixed, so we parse the actual mount-point).
    log "Attaching the .dmg (hdiutil attach)"
    _mnt="$(hdiutil attach "$_dmg" -nobrowse -readonly -plist 2>/dev/null \
      | grep -Eo '/Volumes/[^<]+' | head -1 || true)"
    if [ -z "$_mnt" ] || [ ! -d "$_mnt" ]; then
      rm -f "$_dmg"
      die "could not attach or locate the downloaded Magician disk image"
    fi
    # Find the .app inside the mounted volume (name may vary; take the first).
    _app="$(find "$_mnt" -maxdepth 1 -name '*.app' -print 2>/dev/null | head -1 || true)"
    if [ -z "$_app" ]; then
      hdiutil detach "$_mnt" >/dev/null 2>&1 || true
      rm -f "$_dmg"
      die "no .app bundle was found inside the downloaded Magician disk image"
    fi
    if ! codesign --verify --deep --strict --verbose=2 "$_app"; then
      hdiutil detach "$_mnt" >/dev/null 2>&1 || true
      rm -f "$_dmg"
      die "the Magician application signature is invalid"
    fi
    if ! spctl --assess --type execute --verbose=2 "$_app"; then
      hdiutil detach "$_mnt" >/dev/null 2>&1 || true
      rm -f "$_dmg"
      die "Gatekeeper rejected the Magician application"
    fi
    log "  application signature and notarization assessment passed."
    _dest="/Applications/$(basename "$_app")"
    _stage="/Applications/.magician-install-${USER:-user}-$$.app"
    _backup=""
    log "Staging $(basename "$_app") for installation"
    if [[ -e "$_stage" ]] && ! rm -rf "$_stage"; then
      die "failed to remove stale installer staging path $_stage"
    fi
    if ! ditto "$_app" "$_stage"; then
      hdiutil detach "$_mnt" >/dev/null 2>&1 || true
      rm -f "$_dmg"
      rm -rf "$_stage"
      die "failed to stage $(basename "$_app") in /Applications"
    fi
    if ! codesign --verify --deep --strict --verbose=2 "$_stage"; then
      hdiutil detach "$_mnt" >/dev/null 2>&1 || true
      rm -f "$_dmg"
      rm -rf "$_stage"
      die "the staged Magician application signature is invalid"
    fi
    if [[ -e "$_dest" ]]; then
      _backup="${_dest}.previous.$$"
      if ! mv "$_dest" "$_backup"; then
        hdiutil detach "$_mnt" >/dev/null 2>&1 || true
        rm -f "$_dmg"
        rm -rf "$_stage"
        die "failed to preserve the existing Magician application"
      fi
    fi
    if ! mv "$_stage" "$_dest"; then
      [[ -n "$_backup" ]] && mv "$_backup" "$_dest" >/dev/null 2>&1 || true
      hdiutil detach "$_mnt" >/dev/null 2>&1 || true
      rm -f "$_dmg"
      rm -rf "$_stage"
      die "failed to install $(basename "$_app") into /Applications"
    fi
    if [[ -n "$_backup" ]] && ! rm -rf "$_backup"; then
      warn "installed the new app but could not remove backup $_backup"
    fi
    log "  installed $_dest"
    # Always detach + clean up, then launch the freshly installed app.
    hdiutil detach "$_mnt" >/dev/null 2>&1 || warn "hdiutil detach $_mnt failed — the volume may still be mounted"
    rm -f "$_dmg"
    log "Launching the desktop tray (open -a Magician)"
    open -a Magician || warn "open -a Magician failed — launch it manually from /Applications"
  else
    # dev mode: build the Vosk wake-word deps, then the tray and native audio
    # engine. The orb is process-owned by the Tauri app; there is no child
    # presence host to build or launch.
    if [ -f "$REPO/scripts/setup-desktop-vosk.sh" ]; then
      log "Fetching desktop Vosk wake-word deps (setup-desktop-vosk.sh)"
      bash "$REPO/scripts/setup-desktop-vosk.sh" || warn "setup-desktop-vosk.sh failed — native wake word may be unavailable"
    else
      warn "scripts/setup-desktop-vosk.sh not found — skipping Vosk wake-word setup"
    fi
    log "Building the desktop tray and FluidAudio sidecar"
    make -C "$REPO" build-desktop-tray-debug build-macos-audio-engine-debug || die "desktop native component build failed"
    # run-desktop-tray-debug ends in `exec ./<tray-bin>` — a FOREGROUND, never-
    # returning process. Launch it DETACHED with nohup so phase_tauri returns and
    # the funnel/verify phases still run (same no-block discipline as phase_run).
    # This is the ONLY tray launch in the local-dev flow — phase_run starts the
    # backend supervisor (no tray) — so the :3017 host gateway is bound exactly once.
    local _traylog="$DATA_DIR/desktop-tray.log"
    log "Launching the desktop tray detached (nohup make run-desktop-tray-debug -> $_traylog)"
    nohup make -C "$REPO" run-desktop-tray-debug >"$_traylog" 2>&1 &
    log "  tray pid: $! (detached; host gateway :3017; logs at $_traylog)"
  fi

  # TCC grants the operator must approve on first launch. The host gateway
  # (:3017) + native voice need these macOS privacy permissions; without them
  # relayed mac skills and voice/orb capture cannot work.
  log "macOS TCC grants to approve (System Settings → Privacy & Security):"
  log "  - Screen Recording  (screen capture / observation)"
  log "  - Accessibility     (UI automation / AX control)"
  log "  - Automation        (AppleScript app control)"
  log "  - Microphone        (voice / wake word)"
  log "  NOTE: the host gateway /host/ax/* AX relay is LIVE — it proxies to the host"
  log "        cua-driver daemon, so a containerized backend reaches host AX over :3017."
  log "Desktop computer use needs the pinned CuaDriver in this graphical session:"
  log "  make -C \"$REPO\" setup-cua-driver ARGS=--start   (or Magican Desktop onboarding)"
  log "  then: cua-driver permissions grant   (grants go to CuaDriver.app, not this terminal)"
}

# ---------------------------------------------------------------------------
# phase_tunnel — public Cloudflare Tunnel (cloudflared) so Kapso WhatsApp
# callbacks reach the backend at a STABLE URL on the operator domain
# (webhook.<zone>). Opt-in only (MAGICIAN_ENABLE_FUNNEL=1); a normal run is
# unaffected because this phase is never invoked otherwise.
#
# Placed AFTER phase_run so the Kapso webhook receiver (the host-native kapso
# bot on :3010) is already listening before we point the tunnel at it. The
# ingress target is the resolved Kapso port — the native :3010 for the local
# flow, or a container-published port via KAPSO_WEBHOOK_PORT. First run is
# interactive (cloudflared login + tunnel create); the script detects + instructs
# (never scripts the browser) and writes the stable URL to $DATA_DIR/funnel-url.
phase_tunnel() {
  log "Ensuring public Cloudflare Tunnel for Kapso webhook ingress"
  MAGICIAN_ROOT_DIR="$DATA_DIR" MAGICIAN_INSTALL_DRYRUN="$DRYRUN" \
    bash "$REPO/scripts/ensure-magician-tunnel.sh" || warn "ensure-magician-tunnel.sh reported a failure — Kapso webhooks may not be reachable"
}

# phase_meet_audio — host devices the meeting bot speaks and listens through.
# Host-side for both flows: BlackHole is a Mac HAL driver, and the Linux
# packages are Pulse tools and Xvfb on the machine where the browser runs.
# pipewire-pulse is installed only when no Pulse daemon package is present.
# Ollama is skipped here because phase_ollama already ran. Non-fatal: a
# declined sudo prompt must not abort the rest of the install.
phase_meet_audio() {
  log "Setting up meeting-bot audio"
  if ! MAGICIAN_MEET_BOT_SKIP_OLLAMA=1 bash "$REPO/scripts/setup-meet-bot.sh"; then
    warn "setup-meet-bot.sh failed — meeting audio is incomplete until this is fixed"
  fi
}

phase_verify() { MAGICIAN_INSTALL_FLOW="$FLOW" bash "$REPO/scripts/install-verify.sh"; }

if [ "$ONLY_VERIFY" = 1 ]; then phase_verify; exit $?; fi

prompt MODE "Mode (dev = build from source | user = install prebuilt)" "dev"
prompt DATA_DIR "Runtime data dir (config + secrets + notes live here)" "$HOME/MagicianNotes"
prompt FLOW "Flow (local | container)" "local"
# Container runtime: explicit --runtime / env wins; otherwise the host decides
# (Apple Silicon macOS >= 26 -> apple-container, else docker). No prompt.
[ -n "$RUNTIME" ] || RUNTIME="$(detect_container_runtime)"
export MAGICIAN_ROOT_DIR="$DATA_DIR"

# --- input validation (hoisted: runs BEFORE any heavy phase) ----------------
# Validate the resolved inputs up front so a bad MODE/FLOW (or the unsupported
# MODE=user + FLOW=local combo) dies immediately — NOT after the expensive
# build/fetch. UNSUPPORTED_USER_LOCAL_MSG is the single source for the user+local
# message so it is not duplicated in phase_fetch.
UNSUPPORTED_USER_LOCAL_MSG="MODE=user FLOW=local has no prebuilt backend-binary release yet (magician, magicutor, and magic-supervisor ship in the container image; desktop releases contain the operator app only). Use FLOW=container (pulls $IMAGE_REF) or MODE=dev (builds locally)."
case "$MODE" in dev|user) ;; *) die "invalid MODE '$MODE' (expected: dev | user)";; esac
case "$FLOW" in local|container) ;; *) die "invalid FLOW '$FLOW' (expected: local | container)";; esac
case "$RUNTIME" in docker|apple-container) ;; *) die "invalid runtime '$RUNTIME' (expected: docker | apple-container)";; esac
[ "$MODE" = user ] && [ "$FLOW" = local ] && die "$UNSUPPORTED_USER_LOCAL_MSG"

log "Magician installer — mode=$MODE flow=$FLOW data_dir=$DATA_DIR runtime=${RUNTIME:-n/a}"

# --- orchestration ---------------------------------------------------------
# phase_ollama is a HOST service and runs even under
# FLOW=container (the container reaches it over the host network).
# Heavy phases honour MAGICIAN_INSTALL_DRYRUN via run_phase; phase_runtime_root
# runs for real (data-dir only) so the non-clobber guarantee is always exercised.
run_phase phase_prereqs
run_phase phase_pi
phase_runtime_root
run_phase phase_ollama
if [ "$MODE" = dev ]; then run_phase phase_build; else run_phase phase_fetch; fi
# After the build so a local macOS dev install finds the helper that
# build-all already staged. Container and user flows still run it: the
# driver is a host device, and the script builds the helper when it is missing.
run_phase phase_meet_audio
run_phase phase_run

# Pipeline phase 6: the macOS desktop tray (host gateway :3017) + native voice/orb.
# Runs after the backend is up. macOS-only (skips cleanly on Linux). Under
# FLOW=container on macOS it STILL runs — the host gateway is needed for the
# relayed mac skills — so the only gate is the Darwin check inside phase_tauri.
run_phase phase_tauri

# Optional public Cloudflare Tunnel for Kapso WhatsApp callbacks (opt-in). Called
# directly (not via run_phase) because the tunnel script honours
# MAGICIAN_INSTALL_DRYRUN itself — so under dry-run it still runs and logs the
# "would run" commands, exercising the orchestration without mutating DNS or the
# service. When MAGICIAN_ENABLE_FUNNEL is unset, NO cloudflared code path is
# touched at all.
if gate FUNNEL 0; then ENABLE_FUNNEL=1; else ENABLE_FUNNEL=0; fi
if [ "$ENABLE_FUNNEL" = 1 ]; then
  phase_tunnel
else
  log "Skipping Cloudflare Tunnel (set MAGICIAN_ENABLE_FUNNEL=1 to expose Kapso webhooks publicly)."
fi

# Honour dry-run: phase_verify fires real curls (and prints red FAILs against a
# stack that was never started). Under dry-run, just announce it; otherwise run it
# for real and tolerate its non-zero exit (a partially-up stack must not abort).
if [ "$DRYRUN" = 1 ]; then
  log "would run: phase_verify"
else
  phase_verify || true
fi
if [ "$ENABLE_FUNNEL" = 1 ]; then
  # The cloudflared tunnel script wrote the STABLE webhook URL to
  # $DATA_DIR/funnel-url (it is config-known on the operator domain, not derived
  # from a live status call). Echo it last so it's the final thing the operator
  # sees — paste it into Kapso once.
  if [ -s "$DATA_DIR/funnel-url" ]; then
    log "Kapso webhook URL (set this in Kapso): $(cat "$DATA_DIR/funnel-url")"
  else
    log "Kapso webhook tunnel requested — see the tunnel output above for the public URL (https://webhook.${MAGICIAN_TUNNEL_ZONE:-<your-zone>}/webhook once set up)."
  fi
fi
if [ "$FLOW" = container ]; then
  log "Browser caches: the image bakes warm obscura/Chrome-for-Testing caches into /opt/browser-cache; the runtime wires the scoped HOME's ~/.agent-browser + ~/.cloakbrowser to them AUTOMATICALLY, per dispatch (gated on /opt/browser-cache; a no-op natively) — no manual step required."
fi
log "Done."
