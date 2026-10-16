# Container integration harness: host, browser and Linux skills

`make qualify-container-integration` attaches to an **already-running test
container**. It never builds an image, installs dependencies, starts or stops a
service, changes runtime configuration or calls an LLM. Run it after backend
startup/persistence qualification. Offline regressions:

```sh
make test-container-integration-harness
```

This is also part of `make test-container-tooling`. The regressions use fake
runtime/HTTP/CLI responses plus real bounded subprocesses and a Node check of
the generated extension module; passing them is not live integration evidence.

## Checks and evidence

### Mobile pairing protocol lane

`make qualify-mobile-connectivity MOBILE_CONNECTIVITY_ARGS='PHASE --origin http://127.0.0.1:3004 --state-file /private/probe.json'`
runs a separate serial lane on backend loopback (inside Linux when qualifying
Linux credentials). The state parent must be owned by the invoking user with
mode 0700; the state file is created exclusively with mode 0600. It holds test
credentials: never attach it to reports or commit it.

For a credentials-gated backend, add `--owner-token-env PROBE_OWNER_TOKEN` and
supply that variable privately from an existing owner session/PAT. The bearer
is used only for loopback roster reads, ticket creation and revocation; it is
never forwarded to the public origin or saved. An unset or empty variable fails
before any request. Omit the flag only for an intentionally open fixture.

Phases: `enroll`, restart only the isolated backend and its Secret Service,
`verify`, `revoke`, restart again, `verify-revoked`. The script does not own
service lifecycle. Enrollment creates three synthetic identities through the
iOS/Android QR exchange and ESP loopback bootstrap, and checks single-use
exchanges, ordinary mobile capabilities, server-owned scope despite forged
headers, invalid-token rejection and saved-credential authentication.
Revocation affects only identities in that state file. After a partial
enrollment, keep the state for `revoke`; a new enrollment needs a new file.

`--public-origin https://your-test-host.example` exercises the real
HTTPS/Access ingress; pass it on every phase, since saved credentials bind both
origins. Ticket creation and revocation stay on loopback; QR exchange and device
requests use the public origin, which the QR must name exactly. Access
credentials from enrollment stay in the state file and are reused for ESP
bootstrap; the shared Access credential alone must not authenticate
`/devices/me`. Redirects are refused and TLS verification stays on.
`make test-mobile-connectivity` checks credential routing, partial-failure
cleanup and endpoint binding offline.

This lane is **not** real iPhone/Android/ESP32 acceptance, Android automation
authority, push, chat streaming or audio, and loopback-only runs do not qualify
HTTPS ingress. A backend given an older roster while keeping its keyring anchor
must refuse the rollback and every revoked credential.

Physical-iPhone lanes are separate, opt-in, and call the configured inference
provider. They need an enrolled device plus `MAGIOS_DEVICE_ID` and
`MAGIOS_LIVE_TEST_HOST` (verified in Settings before sending); see the
[iOS test instructions](../../../magios/README.md#tests):

- `make test-ios-live-chat`: native composer, reply persistence after restart.
- `make test-ios-live-today`: read-only cold-launch check of the default app
  widgets and their placement.
- `make test-ios-live-appearance`: Longhand Day/Night captures; restores the
  original appearance.
- `make test-ios-live-playback`: backend audio starts and finishes, distinct
  from local voice fallback; restores reply-voice settings.
- `make test-ios-live-upload`: synthetic PDF shared from Safari through the
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
configuration and socket directory. Cleanup detaches that CLI and deletes only
that thread; cleanup errors fail the run. The server's acknowledgement is
recorded, which does not prove the window closed if the extension had
disconnected.

Each run writes `<report-dir>/mgi-<id>/report.json` with source SHA-256,
selected stages, per-check status, durations and bounded evidence. Raw
screenshots, snapshots, subprocess output and credentials are not kept. Shared
fixture/download files stay in their run directory; the harness never deletes
anything in the runtime data root, only worker-owned `/tmp` fixtures.

Exit codes: **0** all three stages passed; **1** failure; **2** partial
coverage (selected stages only, or `--skip-capture`). Missing optional image
dependencies fail their probes rather than skip.

## Prepare a running test container

Use a previously built image in the same runtime, with isolated ports, a fresh
runtime root and a same-path shared mount. Never use the live data root.
Managed desktop relay calls do not need Apple's localhost DNS forwarding.

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

Choose an unused name and ports, and configure Magican to manage that container
on the test ports; its private exec relay supplies guest loopback port 3017.
With Docker on macOS use `docker run` and `host.docker.internal` for
`MAGICIAN_CONTAINER_HOST` (the gateway URL stays guest loopback). Resolve boot
failures first (see the
Apple Container build runbook).

The shared directory must have the **same absolute path** on the host and in
Linux; the harness proves both directions with random markers.
Extension-backed downloads fetch a concrete link in the authenticated page
context and move up to 16 MiB through CDP to the Linux destination (no shared
mount needed). Page CORS rules apply; scripted download buttons and larger files
are not supported on this path. Ordinary CDP downloads are unchanged.

Keep Magican in managed-container mode on its required host `127.0.0.1:3017`
listener. Host-alias forwarding alone fails its loopback peer check; the private
relay is required for container-origin calls. The worker reads the container's
`MAGICIAN_HOST_GATEWAY_URL`; override with `--gateway-url` only when needed.

## Prepare the test extension

An ordinary extension discovers the **live** service ports from Tauri, so a
test profile needs an unpacked copy pinned to the test ports:

```sh
python3 scripts/qualify-container-integration.py \
  --prepare-extension-dir "$INTEGRATION_DIR/extension" \
  --extension-magician-port 13002 \
  --extension-magicutor-port 13003
```

This writes only a new directory; existing destinations and the standard live
ports are rejected. Load it via **Load unpacked** in a separate Chrome profile
(its name ends with `[Integration test]`) and pair through the normal flow. The
production extension, profile, desktop ports and stored credentials are
untouched; host-native gateway calls still use the installed Tauri app.

## Run the probes

```sh
make qualify-container-integration CONTAINER_INTEGRATION_ARGS="\
--runtime apple-container \
--container magician-integration-test \
--shared-dir $INTEGRATION_DIR/share \
--report-dir $INTEGRATION_DIR/evidence"
```

For paths with spaces, call the Python script directly. For Docker use
`--runtime docker`. `--magicutor-url` is the origin **inside** the container
(default `http://127.0.0.1:3003`), not the host's published `13003`; the test
extension uses the published ports.

`--stages skills` or `--stages host` run without `--shared-dir` and return
**2** on success, since they do not establish full integration. `--skip-capture`
checks host connectivity without sampling the screen (also partial). Default
skill paths are `/app/skillshub` and
`/data/scopes/anonymous/default/skills/browser/bin/agent-browser`; CLI
overrides exist for other layouts.

Commands run serially with a 30-second default per-command timeout, bounded
output, and one OCR/native-library thread; the outer exec has a longer finite
deadline. A killed runtime CLI does not prove its in-container process stopped.
After a failed run, check for a leftover `mgi-…` test window. The harness never
terminates unrelated daemons or containers.

## What this does not qualify

- Client disconnect/reconnect is tested; restarting Magicutor or reconnecting
  the extension is a separate lifecycle test.
- The generated extension bypasses production endpoint discovery, so it tests
  real CDP/extension actions on isolated ports, not the discovery/pairing UX.
- Skill probes run installed adapters directly. Governed dispatch, per-scope
  skill installation, model planning and permission decisions need their own
  end-to-end tests.
- A small screen capture proves the host route. Typed macOS Apps attestation
  and destructive computer actions are out of scope.
- The local shared mount and bounded browser transfer are tested; remote
  filesystem access, desktop pairing and file grants are separate.
