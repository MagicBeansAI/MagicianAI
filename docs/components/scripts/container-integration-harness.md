# Container integration harness: host, browser and Linux skills

`make qualify-container-integration` attaches to an **already-running test
container**. It never builds an image, installs dependencies, starts/stops a
service, changes runtime configuration or calls an LLM. Run it after backend
startup/persistence qualification. Its offline regressions run with:

```sh
make test-container-integration-harness
```

This is also part of `make test-container-tooling`. The regressions use fake
runtime/HTTP/CLI responses, plus real bounded subprocesses and a Node check of
the generated extension module. Passing them is not live integration evidence.

## Checks and evidence

### Mobile pairing protocol lane

`make qualify-mobile-connectivity MOBILE_CONNECTIVITY_ARGS='PHASE --origin http://127.0.0.1:3004 --state-file /private/probe.json'`
runs a separate, serial lane on backend loopback. Run it inside Linux when
qualifying Linux credentials. The state parent must be owned by the invoking
user with mode 0700; the new state file is created exclusively with mode 0600.
It contains test credentials and must never be attached to reports or committed.

For a credentials-gated backend, add `--owner-token-env PROBE_OWNER_TOKEN` and
supply that environment variable privately from an existing owner session/PAT.
The bearer is used only for loopback roster reads, enrollment-ticket creation
and revocation. It is neither forwarded to the public origin nor saved in probe
state. An unset/empty named variable fails before requests are made. Omit the
flag only for an intentionally open isolated fixture.

Use `enroll`, restart only the isolated backend and its Secret Service, then
`verify`, `revoke`, restart again and `verify-revoked`. The script does not own
service lifecycle. Enrollment creates three unique synthetic identities using
the existing iOS/Android QR exchange and ESP loopback bootstrap. It checks
single-use exchanges, ordinary mobile capabilities, server-owned scope despite
forged headers, invalid-token rejection and saved credential authentication.
Revocation affects only identities from that private probe state. If enrollment
partially fails, retain the state for `revoke`; a new enrollment needs a new file.

Add `--public-origin https://your-test-host.example` to exercise the actual
HTTPS/Access ingress. Keep that same flag for every phase: saved credentials
are bound to both origins. Ticket creation and revocation stay on backend
loopback; QR exchange and subsequent device requests use the public origin.
The QR must name that exact origin. Access credentials returned by ordinary
enrollment are kept only in the private state file and reused for ESP bootstrap;
the shared Access credential alone must not authenticate `/devices/me`.
Redirects are refused and TLS certificate verification remains enabled.
`make test-mobile-connectivity` checks credential routing, partial-failure
cleanup state and endpoint binding offline, without devices or provider calls.

This lane deliberately does **not** count as real iPhone/Android/ESP32 acceptance,
Android automation authority, push, chat streaming or audio. Loopback-only runs
also do not qualify HTTPS ingress. A backend given an older roster while keeping
its keyring anchor must refuse the rollback and every revoked credential.

Physical-iPhone lanes are separate, opt-in, and call the configured inference
provider (not part of this provider-free harness). They require an enrolled
device plus `MAGIOS_DEVICE_ID` and `MAGIOS_LIVE_TEST_HOST` (verified in Settings
before sending); see the [iOS test instructions](../../../magios/README.md#tests):

- `make test-ios-live-chat` — native composer, reply persistence after restart.
- `make test-ios-live-today` — read-only cold-launch check of the default app
  widgets and their placement.
- `make test-ios-live-appearance` — Longhand Day/Night captures; restores the
  original appearance.
- `make test-ios-live-playback` — backend audio starts and finishes, distinct
  from local voice fallback; restores reply-voice settings.
- `make test-ios-live-upload` — synthetic PDF shared from Safari through the
  Share Extension and upload/chat path.

### Host, browser and skill probes

| Stage | Executed from inside the container | Pass criterion |
|---|---|---|
| Runtime | Linux identity and effective UID | Linux, non-root execution |
| Host | Gateway endpoint contract, automation availability, a `64x64` screen-region request | Valid contract, `available: true`, PNG with dimensions/chunks/checksums; report keeps only size and hash |
| Browser | Same-path shared-directory marker; Magicutor identity; installed agent-browser core skill | Correct mount and proxy, no fallback to another browser engine |
| Browser | Open a nonce-bearing temporary host-loopback HTTP page; snapshot; detach/reconnect client; download through snapshot ref | Exact page title survives reconnect; downloaded bytes match inside Linux; return marker is visible on host |
| Skills | Installed PDF binary, OCR adapter using shipped scan and Tesseract, Python HTML adapter, Node `@vscode/ripgrep` package/native binary | Expected fixture content, not just exit status/version output |

The browser uses `/devtools/browser/mgi-<random-id>`, which owns a dedicated
Chrome window in Magicutor. Every CLI call uses that ID plus a private HOME,
configuration and socket directory. Cleanup detaches that CLI and requests
deletion of only that thread. Cleanup errors fail the run. The server's cleanup
acknowledgement is recorded; this does not independently prove an extension-side
window close when the extension has disconnected.

Each invocation writes `<report-dir>/mgi-<id>/report.json`, including source
SHA-256, selected stages, per-check status, durations and bounded evidence. Raw
screenshots, browser snapshots, subprocess output and credentials are not
retained. Shared fixture/download files are left in their unique run directory
for inspection; the harness never deletes anything in the runtime data root.
Only worker-owned `/tmp` fixtures are automatically removed.

Exit codes: **0** means all three stages passed; **1** means failure; **2** means
partial coverage (selected stages only, or `--skip-capture`). Missing optional
image dependencies are failures in their selected probes, not successful skips.

## Prepare a running test container

Use a previously built image in the same runtime. The following Apple Container
example uses isolated ports, a fresh runtime root, and a same-path shared mount.
It assumes the container runtime is configured. Managed desktop relay calls do
not require Apple's localhost DNS forwarding. Do not use the live data root.

```sh
INTEGRATION_DIR=$(mktemp -d /Volumes/build/magician/integration.XXXXXX)
mkdir -p "$INTEGRATION_DIR/runtime" "$INTEGRATION_DIR/share"
container run -d --name magician-integration-test \
  --cpus 2 --memory 4g \
  -p 127.0.0.1:13002:3002 -p 127.0.0.1:13003:3003 \
  -v "$INTEGRATION_DIR/runtime:/data" \
  -v "$INTEGRATION_DIR/share:$INTEGRATION_DIR/share" \
  -e MAGICIAN_ROOT_DIR=/data \
  -e MAGICIAN_CONTAINER_HOST=host.container.internal \
  -e MAGICIAN_HOST_GATEWAY_URL=http://127.0.0.1:3017 \
  magician:integration-test
```

Choose an unused container name/ports. Configure Magican to manage that name
and attach it to the test ports. Its private exec relay supplies guest loopback
port 3017. With Docker on macOS, use `docker run` and `host.docker.internal`
for `MAGICIAN_CONTAINER_HOST`; the gateway URL stays guest loopback. These are runtime caps;
image build resource limits are a separate concern. Use a qualified image and
resolve boot failures before the integration checks (see the
Apple Container build runbook).

The harness requires the mounted shared directory to have the **same absolute
path** on the host and inside Linux, and proves both directions with random
markers. For extension-backed downloads, the patched CLI fetches a concrete
link in the authenticated page context and transfers up to 16 MiB through CDP
to the Linux destination. That transfer does not itself require a shared mount.
Page CORS rules apply; scripted download buttons and larger files are not
supported by this extension path. Ordinary CDP browser downloads retain their
existing behavior.

Keep Magican running in managed-container mode on its required host
`127.0.0.1:3017` listener. Host-alias forwarding alone does not satisfy its
loopback peer check; the private relay is required for container-origin calls.
The worker reads its container's `MAGICIAN_HOST_GATEWAY_URL`; override with
`--gateway-url` only when needed. No listener or trust boundary is changed.

## Prepare the test extension

An ordinary extension discovers the **live** service ports from Tauri on
startup and periodically thereafter. Merely changing a cached URL would not
keep a test profile isolated. Generate an unpacked test copy whose discovery
function stays pinned to the test ports:

```sh
python3 scripts/qualify-container-integration.py \
  --prepare-extension-dir "$INTEGRATION_DIR/extension" \
  --extension-magician-port 13002 \
  --extension-magicutor-port 13003
```

This writes only a new directory and exits; existing destinations and the
standard live ports are rejected. Load that directory through **Load unpacked**
in a separate Chrome profile. Its name ends with `[Integration test]`. Complete
any configured test-stack pairing through the normal extension flow. The
production extension source, profile, desktop ports and stored credentials are
not modified. Host-native gateway calls still use the installed Tauri app.

## Run the probes

```sh
make qualify-container-integration CONTAINER_INTEGRATION_ARGS="\
--runtime apple-container \
--container magician-integration-test \
--shared-dir $INTEGRATION_DIR/share \
--report-dir $INTEGRATION_DIR/evidence"
```

For paths containing spaces, invoke the Python script directly with quoted
arguments. For Docker select `--runtime docker`. `--magicutor-url` is the
origin **inside** the container (default `http://127.0.0.1:3003`), not the host's
published `13003`. The test extension uses the host's published ports.

Run just `--stages skills` or `--stages host` while browser setup is pending;
these do not require `--shared-dir`. They return **2** when their selected checks
pass, since they do not establish full integration. `--skip-capture` checks host
connectivity/availability without sampling the screen and also produces partial
coverage. Default skill paths are `/app/skillshub` and the scoped browser binary
under `/data/scopes/anonymous/default/skills/browser/bin/agent-browser`; use the
CLI overrides if your image intentionally uses different locations.

Commands run serially with a default 30-second per-command timeout, bounded
output, and one OCR/native-library thread. The outer exec has a longer finite
deadline. A killed runtime CLI is not proof that its in-container exec process
has stopped; in-container operations have their own bounds and cleanup. On a
failed/interrupted run, inspect any remaining `mgi-…` test window before retrying.
The harness does not terminate unrelated daemons or containers.

## What this does not qualify

- Client disconnect/reconnect is tested. Restarting Magicutor or reconnecting
  the extension itself is a separate lifecycle test; this harness performs no
  service restart.
- The generated extension deliberately bypasses production endpoint discovery.
  It exercises the real CDP/extension actions against isolated ports, not the
  production discovery/pairing installer UX.
- Skill probes execute installed adapters/dependencies directly. Magician's
  governed dispatch, skill installation/materialization for every scope, model
  planning and permission decisions need their own end-to-end tests.
- A small screen capture proves the existing host route. Typed macOS Apps
  attestation and destructive computer actions are outside this lane.
- The local shared mount and bounded browser link transfer are tested. General
  remote filesystem access, desktop pairing and file grants are separate work.
