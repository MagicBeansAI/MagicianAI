# magician-components

Which stack components are present, what that enables, and what it costs to
decline one. Read by the API, the setup wizard, and Desktop onboarding.

## Why it exists

Per-component install gates are only safe if everything downstream knows how to
behave when a component is absent; otherwise declining one produces a silently
broken install (navigation that leads nowhere, memory retrieval failing at call
time) rather than a smaller one. This crate answers:

> Given what is actually running and what the operator asked for, which features
> work, and for the ones that do not, what exactly is missing?

## Two kinds of truth

`Declared` is persistent intent (wants a local model, declined the public
tunnel). `Observed` is what a probe just found.

| Declared | Observed | State | Meaning |
| --- | --- | --- | --- |
| any | present | `Present` | works now |
| declined | absent | `Declined` | as asked, not a fault |
| wanted | absent | `Missing` | asked for and not got — worth attention |
| unset | absent | `NotInstalled` | never set up either way |
| any | unknown | `Unknown` | a person has to confirm |

Observation decides whether a feature works; declaration decides whether absence
is a problem or a choice. So an install with no declarations reports what is
running, not a wall of "not installed".

## The graph

`src/graph.yaml`, compiled in with `include_str!` (a missing file at startup
would look like an empty stack). Data, so adding a capability is a reviewable
entry. Probe paths come only from existing sources (`scripts/install-verify.sh`
port inventory, `scripts/install.sh` phases, `.env.example` identity keys); a
component that cannot be probed says so with a `manual` probe. Paths carry
`{data_root}` and `{home}` placeholders expanded once on load.

Three node kinds:

- **Component** — installable or configurable (an API key is one). A `core`
  component is not declinable (the macOS tray, which binds the host gateway that
  relays mac skills). A `required` one is not declinable but must still be
  installed (the local embedding model: memory is indexed locally in both
  processing modes, yet the model is a multi-gigabyte pull).
- **Requirement** — a named join point several components satisfy ("a model",
  "a browser driver"), named once so features cannot drift about what counts.
- **Feature** — an outcome a person recognises.

A feature needs all of its needs; a requirement is met by any one provider
(AND/OR), and each alternative states its trade-off, shown at the moment of
choice. A component whose dependency is unusable is `BlockedBy` it regardless of
its own probe.

Shipped-graph tests make `load` safe to panic: the graph parses, refers only to
existing ids, expands every placeholder, gives every requirement a provider,
states a trade-off on every alternative, keeps core components out of choices,
and lands every feature on a surface. Behaviour tests pin that declining local
models leaves chat working through a cloud key, and that the phone works on the
local network with no tunnel while WhatsApp needs one.

`computer-use` depends on the platform-neutral `cua-driver` component,
separately from `mac-skills` and `imessage`. Its `cua_driver` setup binding makes
Desktop install and verify the driver on the local graphical host even when the
engine is remote or containerized; installation alone cannot certify native
permissions.

### Working is not the same as staying local

- **A remote model runs the work; only the on-device one keeps it here.** A
  `local-processing` requirement only Ollama satisfies is separate from the
  `model` requirement either provider satisfies, with a "Keep processing on this
  machine" feature over it — mirroring `privacy.processing.mode`, the one
  locality switch. Embeddings stay local in both modes, so memory always needs
  the local embedding daemon.
- **A bundled browser can research the web; only the extension sees your tabs.**
  "Work with your open tabs" needs the extension component itself; "Research on
  the web" takes either.

Both are pinned by tests.

### Pricing

`pricing` is `free`, `free_tier` or `paid` (default free). Deliberately coarse:
real prices change and are the provider's to state; the only question before
choosing is whether this needs a card. One field so every surface (wizard status
list, graph chip, [the keys guide](../../setup/api-keys.md)) says it the same way.
Free components are not labelled.

### Host requirements

A component may declare a `host` block — minimum memory, architectures,
operating systems, and the `because` shown on refusal. Local generation requires
16 GB on Apple Silicon (embedder plus the smallest model still need unified
memory and a GPU; below that it swaps). Model choice above that floor is not this
gate (36 GB+ Qwen 27B, 24–35 GB Gemma 4 12B, 16–23 GB Woof 4B).

This is deliberately **not** part of `resolve`: "can this machine have it" is
asked before install; folding it in would make a running component report itself
unavailable. `Component::unsupported_on(host)` returns the reason or `None`.

Local generation model identities are not in the graph. Desktop reads them from
the settings API backed by `data/magician_v2/local_generation_catalog.yaml`
(which also supplies the RAM recommendation and warnings; a differing explicit
choice is allowed) and writes `runtime.ollama.local_generation.selected` before
the graph installer runs; every Ollama profile reads it through a YAML anchor.
`make setup-local-generation` exposes the same catalog.

### Install actions

Every component declares an `InstallAction`:

- `make` — carries the target *and* the script it runs, so a package install
  without a Makefile can still run the step; where make exists it stays the way
  in. A target with no script needs a checkout and says so before asking
  (`setup-agent-browser` delegates to skillshub's Makefile).
- `manual` — an instruction with an optional place to open, expanded with
  `{data_root}`, plus `secrets`: the variables a wizard can collect and write,
  each with a human label. Named explicitly, never parsed from prose. Empty for
  steps needing a file or permission (`client_secret.json`). For any-of probes
  every provider is listed and one value ends the asking.
- `with_runtime` — core components that cannot be installed alone.

Automatic server-side actions name existing `make` targets. A typed desktop-local
setup driver may own a bounded packaged-app action when the program must run on
the graphical host (`cua_driver`).

## Setup catalog

`setup_catalog.yaml` defines the reusable interactive drivers — `managed_bot`,
`governed_oauth`, `configuration_file`, `model_runtime`, `browser_extension`,
desktop-local `cua_driver` — with fields, bot start actions, allowed login URL
hosts, model-locality choices and validators. Components bind to them in
`graph.yaml`; Skillshub manifests bind through `metadata.magician.setup`. Desktop
and the API interpret the typed drivers, so a new integration needs data, not a
provider-name branch.

- **`cua_driver`** is an exact release pin: `version` (`x.y.z`) plus
  `unix_installer` and `windows_installer` lists of `{name, url, sha256}`, entry
  script first. URLs must be
  `https://github.com/trycua/cua/releases/download/cua-driver-rs-v<version>/<name>`
  or `https://raw.githubusercontent.com/trycua/cua/cua-driver-rs-v<version>/…/<name>`,
  hashes lowercase 64-hex. `setup::cua_driver_release()` returns the compiled pin.
  The variant also reads (never writes) the older unpinned shape
  (`unix_installer_url` / `windows_installer_url`), because Desktop reads an
  engine's whole catalog at once; the built-in catalog must not use it.
- **Host-bootstrap profiles** are ordered prerequisites, each naming the binaries
  that prove readiness and one bounded installer (`homebrew` or a reviewed
  Homebrew formula). macOS managed-container: Homebrew plus host Python (private
  credential custody); native-services adds Node.js/npm and `uv`. Linux container
  declares host Python as a manual prerequisite (hard stop with distro-neutral
  guidance). The crate only parses and validates; Desktop detects and installs
  after showing the plan for consent.
- **External harnesses** are declared separately from components: display order
  and name, binary, coding and plane engine ids, config gate, refresh endpoint,
  vendor guide, surfaces. No installers: Magican detects, explains what remains,
  and selects a server-reported Ready engine.

## No I/O

`resolve` is a pure function: callers supply declared and observed maps and get a
`Report`. Probing and persistence belong to the caller. Declared state lives in
`operator-config.yaml` in the data root, written through
`runtime_settings.rs` (the existing locked writer for operator config).

`selection::plan_selection` is the shared dependency-closure and provider-choice
algorithm for terminal and Desktop setup: graph + observations + selected
features + host → ordered install steps, unsupported components, unresolved
requirements, capabilities left off. `selection::projected_report` shows the
expected result. Both are pure; the API runs probes beside the engine first, so
remote Desktop setup never mistakes desktop-local state for the server's.

## Probes

Probes are **data** (`ProbeSpec`), executed by the runtime, the wizard, or a
test stub.

| Spec | Present when | Why it exists |
| --- | --- | --- |
| `HttpOk` | 2xx | the ordinary service check |
| `HttpAnyResponse` | any HTTP response | the Kapso webhook is POST-only, so a GET answering 4xx still proves the tunnel routed through to a live upstream |
| `HttpBodyContains` | 2xx and the body contains a needle | the embedding daemon answers happily with no model resident, which is indistinguishable from working until something asks it to embed |
| `ConfiguredOllamaModelPresent` | the model selected by a YAML key path appears in Ollama `/api/tags` | local setup verifies the exact chosen generation model rather than accepting any running Ollama daemon |
| `FileExists` | the path exists | state files such as the funnel URL |
| `FileReadable` | the path can be **opened** | permissions. `~/Library/Messages/chat.db` is on every Mac that has ever sent a text, granted or not, so `FileExists` on it answers "have you used Messages" while appearing to answer "is Full Disk Access on". Denied is `Absent` with the reason the graph wrote; missing is `Unknown`, because a file that is not there says nothing about the permission |
| `EnvKeyPresent` | **any** of the listed keys is set in the environment, or has a non-empty assignment in one of the given env files | API keys, where one component can be configured several ways — a remote model provider is present if a key for any supported provider is. Absence is reported singular for a one-key probe and plural only when several are genuinely listed |
| `Manual` | never — reports `Unknown` | macOS gives no way to read microphone or screen-recording permission from a process that does not hold it |

- **A permission probe answers about the permission.** A read attempt is the
  reliable probe for Full Disk Access (EACCES means missing), as in
  `desktop/src-tauri/src/permissions.rs`. macOS gates reading and controlling
  separately, so iMessage needs Full Disk Access (read `chat.db`) and Automation
  for Messages (AppleScript); an unreadable grant like Automation takes a
  `Manual` probe rather than being inferred.
- `probe::detect_host` observes OS, architecture and physical memory.
  Architecture is normalised (`arm64`/`aarch64` are one spelling); unreadable
  memory is 0, which never qualifies.
- `probe::observe` runs probes behind the **`probe` feature** (adds `reqwest`,
  already compiled by the API); without it the crate depends on serde alone. Keep
  everything network-related, imports included, behind
  `#[cfg(feature = "probe")]`; `cargo clippy -p magician-components --all-targets`
  must be silent with and without the feature.
- **Configuration, not validity:** a key is present when set, never validated by
  calling the provider (their outage would read as the operator's fault).
  **No proxy:** probes ask about this machine.
- Every probe is fail-soft: unreachable is `Absent`; unanswerable is `Unknown`,
  which never counts as usable.

## Surfaces

`Report::available_on(surface)` is what a surface renders; `blocked_on(surface)`
is what it could offer to install. Every feature declares its surfaces, so a
surface renders only what it can deliver.

## Operating it


```bash
cargo run -p magician-components --features probe --example status
```

Prints every component's live state and, per surface, which features work and
what the rest wait on. Disagreement with `scripts/install-verify.sh` is a bug in
one of them.

## Where it sits in the workspace

- Classified as exempt in `magician/tests/architecture_boundaries.rs` (neither
  product lane nor substrate, no edge to either); that test fails any
  unclassified new workspace member.
- `GET /health` (`magician-api/src/service_health_api.rs`) stays thin: it probes
  two things with a 2 s timeout and is polled continuously by web, iOS and
  Android, so the registry (more probes, needs caching) has its own endpoints.
- `NotesProviderStatus` (`configured` / `available` / `writable` / `message`,
  read-and-mutate pair) is the per-component shape the registry generalises;
  `media.engines.*.enabled` in `magician-config.yaml` is the shape a declared
  component entry follows.

Design: `docs/plans/2026-09-02-setup-wizard-rust.md`.
