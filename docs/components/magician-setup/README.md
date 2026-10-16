# magician-setup

Version 0.1.2 completes reviewed macOS source installation, native service binding and readiness verification while preserving owner configuration.

The guided installer. Asks what you want the system to do, works out what that
needs, and shows what it would install before installing anything.

The shared graph includes required skill software and media dependency steps.
Cloudflare and Clarity are optional capability choices shared with Desktop.
Cloudflare collects typed per-machine settings and a masked token before
provisioning Tunnel/Access and checking authenticated health. Clarity clearly
opts into app session recording through a private runtime project setting;
neither integration is required for local use. See [Cloudflare](../../setup/cloudflare.md)
and [Clarity](../../setup/clarity.md).
Channel/account setup depends on verified bot software; credentials cannot make
a missing CLI or bundle ready. Installers stream output, link shared software
into the scope and recheck file inventories. The terminal engine passes its
selected data root to both Make and packaged script actions.

Every skill step links ALL existing scopes, not only the selected one, then
verifies what the runtime executes: with the data root, the `skills`, `media`
and `browser` inventories include each scope's `bin/` entries by absolute path.
A scoped CLI that dangles (a cleaned checkout, a data root copied elsewhere)
turns its component Absent, so the wizard offers the step again instead of
reporting the shared install healthy. Pinned native executables live in
`<data root>/system/skill-bin/` and scopes link to them relatively; see
[skillshub](../../../skillshub/README.md#every-scope-and-scopes-that-move).

Native packages include pinned generated tools and bot bundles with locked
dependency inputs. Packaged setup uses them without Rust/Go regeneration.
Bundled-browser setup also has a packaged entrypoint. See
[runtime setup](../../quickstart.md#skill-and-bot-software-readiness).

```bash
make setup-wizard                       # choose capabilities, review the plan
make setup-wizard ARGS=--status          # what is here now, and what it enables
make setup-wizard ARGS=--dry-run         # rehearse with real probes, without writes
```

## Running it

- `make setup-wizard`, or `bash scripts/setup-wizard.sh`. The script builds a debug
  binary the first time and is silent afterwards unless a source under
  `magician-setup/` or `magician-components/`, a Cargo manifest or the lockfile is
  newer than the binary. It reuses
  the Makefile's `CARGO_TARGET_DIR`, so it never forks a second target tree.
- `make setup-wizard ARGS=--status` prints what is here and exits.
- `make setup-wizard ARGS=--dry-run` rehearses the install and changes nothing.
- `make setup-wizard ARGS=--simulate [file.yaml]` plays the wizard against
  described machines and checks invariants.

## It asks about capabilities, not components

The list is outcomes a person recognises; components are derived. The graph and
resolver live in [`magician-components`](../magician-components/README.md), so
the wizard and runtime answer from one declaration. Dependency closure and
provider preference are `magician_components::selection`, shared with the API and
Desktop; the TUI keeps only cursor, remembered-answer, rendering and execution
state. Selection starts from what already works, so re-runs never ask anyone to
re-choose what they have.

## Three ways in, one model

| Mode | When | What it does |
| --- | --- | --- |
| Rich | a TTY on both ends | draws the full-screen UI |
| Plain | no TTY, or `MAGICIAN_WIZARD_NONINTERACTIVE=1` | prints status and exits |
| Failed start | the screen will not initialise | prints status, exits non-zero |

All three read the same `Selection`. Falling back rather than prompting is
deliberate: a wizard that blocks on input in CI is a hung build.

## Screens

- **Welcome.** The argument on the left (the manifesto's closing line,
  `theme::THESIS`, shared with the landing page) and what the wizard already
  found on the right ("10 of 19 capabilities already work" — evidence it probed,
  not decoration). The wordmark is a readable lookup table (`theme::wordmark`,
  no figlet dependency) in its own colour, because green, red and yellow mean
  present, missing and unknown and nothing else uses them. Below the width it
  needs, the wordmark returns nothing and the name falls back to plain accent (a
  wrapped banner looks broken); tested at the boundary.
- **Chrome.** Full screen: a title bar, two bordered panes and a key bar, shared
  by every screen. One line per choice; the side pane explains the row under the
  cursor, and unavailable rows carry a short inline marker. It refuses to draw
  below 72x16 and prints the status instead. (Full screen rather than inline,
  because the plan runs after the screen exits and a consequence must be shown
  beside the choice that causes it.)
- The cursor moves over **every** row, including unselectable ones; skipping
  them would make every keypress do nothing when only one row is choosable.

## How before what

The first choice is how to install, since it changes duration and feasibility.
Three paths, always all three:

| | |
| --- | --- |
| Prebuilt | download what is already built, nothing compiles |
| From source | install what is missing to build, build, then install that |
| Container | the whole runtime in one image |

- A path this machine cannot take is greyed with its reason, not hidden (hidden
  teaches "never", greyed teaches "not yet"). The cursor steps over greyed rows
  here, because a first key press that does nothing reads as a broken screen.
  From source is unavailable on Linux (the prerequisite installer is macOS only);
  a missing toolchain never disqualifies it, missing source does.
- Package-only and container terminal setup are currently disabled with reasons.
  A local `MAGICIAN_PACKAGE` is displayed, but package-only startup belongs to
  Desktop until the standalone terminal package lifecycle is implemented.
- Every route ends at the same install prefix: from source packages what it built
  exactly as a release is packaged, then installs it (one layout to support, one
  uninstaller). Source packages use Desktop's debug-signing identity, with
  ad-hoc fallback when no usable certificate is available.
- The installed version is read from the manifest the installer left, not by
  probing binaries (a stopped install is still installed). A route that would
  replace it says so by name, and `--status` reports it.
- Source setup always runs the incremental debug build; Cargo reuses current artifacts.
  The rules are a pure function over a described machine in `install_mode.rs`.

## What the plan owes

- Dependencies come before dependants. A requirement with several providers picks
  the first declared and the screen says which. Present components are never
  planned again; shared requirements install once.
- Everything left off is listed with its reason, so declining produces a smaller
  install, not a broken one.
- A `required` component (the local embedding model) is planned whatever was
  picked, before anything a capability drags in. `required` is not `core`: core
  arrives with the runtime; required is a real install that is not a choice.
- A component's `host` block is checked (`Component::unsupported_on`) before a
  step is offered, not after it fails; `resolve` is untouched. The engine also
  refuses unsupported components itself, as a backstop for step lists built
  outside `plan()`.
- A step runs `make <target>` where a Makefile exists and the shipped script
  otherwise. The engine asks `in_source_checkout()` before asking the person
  anything. A target with no script (`setup-agent-browser`) is refused outside a
  checkout with that reason.
- The list marks `[paid]` or `[free tier]` from the graph's `pricing` field;
  free carries no label.

## Running the plan

On macOS 14+ Apple Silicon, acceptance covers the reviewed software prefix,
runtime data root, per-user LaunchAgent and capability plan. The shared source
installer prepares prerequisites, seeds absent configuration and both private
env files, sets up software and the memory embedder, builds with
`make build-source-debug`, and checks/installs the native package. It starts Desktop
and the backend, then waits for core readiness before capability execution.
The MLX-enabled source path uses Rust 1.95.0 for the entire build, including
software helpers and Desktop, so sequential steps share compatible artifacts.
Terminal setup restarts Desktop to activate the new source build; a foreign
gateway listener is refused before startup. Desktop's own installer keeps its
current window running.
After configuration, the service picks up changed runtime inputs and a final
sweep checks required services even when no optional components were selected.
Unavailable selected capabilities or unfinished steps return nonzero; the
wizard only prints verified completion after fresh observations. Re-running
preserves existing owner data and uses the same software/service layout as Desktop.
Permission observations are refreshed through the signed Desktop's local status
route, so a manual confirmation cannot replace a native grant. Credential writes
preserve existing values in either env file and escape literal dotenv characters.

The engine takes the plan in order: probe, ask, act, probe again.

- **A step is done when its probe says so, never when a command exited zero.** A
  target that fails is still probed afterwards (it may have done the part that
  matters).
- **One failure does not end the run.** Downstream cost falls out of the graph:
  the next step's dependency check sees the unusable component and reports
  blocked, naming what broke.
- **Manual steps wait.** The wizard shows instructions, opens the Settings pane
  or extensions page, and re-probes on an interval. Desktop attests privacy
  grants through its local status route. An unavailable attestation stays
  unknown and cannot satisfy final readiness through confirmation alone.
- **One ask per variable per run**, however many components declare it (e.g.
  `MINIMAX_API_KEY` serves two components).
- Every effect goes through an injected `Effects` trait, so the engine is a state
  machine over data, tested with no subprocess, terminal or waiting.

### Pasting a key

`InstallAction::Manual { secrets }` names the variable each step wants (never
parsed from prose). The prompt reads with echo off (not even asterisks) and
returns **whether** a value landed, never the value, so secrets never travel back
up toward a log. Raw mode is left before anything prints. Empty input skips; for
any-of components one provider's key satisfies the probe and ends the asking.

The writer (`engine::env_file`) preserves comments and existing values in both
`.env` and `.env.development`. New keys append; empty seed assignments are filled
in place so dotenv's first-assignment precedence cannot hide a credential. It
treats a commented `# EXA_API_KEY=...` as a suggestion and uses mode 0600. A component that wants a *file*
(`client_secret.json`) is never prompted; the wizard opens the right page of
[the setup guides](../../setup/) and takes the result.

### What it remembers

`$MAGICIAN_ROOT_DIR/.setup-answers` records which **capabilities** were chosen,
so a re-run starts from the last answer (declining the tunnel leaves no other
trace). An unrecorded capability (new since the last run) falls back to whether
it already works. The probe stays the authority on what is installed; this file
only remembers a choice.

## Dry run

`--dry-run` walks the whole install with **real** probes (a plan for invented
state would describe nobody's machine); only a step's effect is simulated, and
the probe after a would-run answers for the pretence rather than claiming "ran,
still not answering". `confirm` answers its default. It works without a terminal
(it exists to be read, including in a pipe). Core preparation, startup and
verification print the same shared stage invocations used by a real install.
It never saves `.setup-answers`, prompts for credentials, writes runtime files
or starts services. The launcher may compile the wizard itself if needed;
installation effects are still disabled. `DryRunEffects` is the third
`Effects` implementation beside the real one and the test fake, so the rehearsal
is the real flow.

## Simulation

`--simulate` plays real selection, real plan and real engine with scripted
effects against a built-in sweep of described machines (8 GB Mac refused a local
model, re-run over a half-install, the person who says no to everything, package
install with no Makefile, runtime not up so the blocking cascade is pinned). It
reads no probe, runs no command and writes no file, and says nothing about the
machine it ran on. A scenario is a world — host, probe results, choices, keys
the person will paste, acceptance, checkout, runtime up — with every field
defaulting to the ordinary cooperative run.

`simulate::violations` checks invariants over **any** run, not golden
transcripts:

- never plan something this host cannot have
- never plan something that already works, so a re-run is quiet
- dependencies before dependants
- a required component is planned, present, or refused with a reason
- every planned component produces a step (`Engine::run` skips unresolvable ids,
  so a plan naming something absent from the graph would vanish silently)
- no step runs for a component outside the plan
- nothing is asked for that no component declares, and nothing is asked twice
- a chosen capability either works afterwards or the plan says why not
- saying no changes nothing

The suite fails on a single violation and prints the whole sweep. A described
set runs with `--simulate <file.yaml>` and exits non-zero on violation:

```yaml
- name: "16GB M1, package install, notes only"
  host: { os: macos, arch: arm64, memory_gb: 16 }
  present: [magician-runtime, cloudflare-tunnel]
  wanted: [notes]
  will_paste: [KAPSO_API_KEY]
  in_checkout: false

- name: "a plain Mac, nothing chosen"
```

Only `name` is required.

## Screen tests

`ui/tests.rs` draws the real widgets into ratatui's `TestBackend` and asserts on
the result (name and tagline, key hints, every choice with its reason, cursor
reaches every row, enter on an unavailable row explains).
`make cargo ARGS="test -p magician-setup ui::tests::the_screen_as_drawn -- --nocapture"`
prints the screen instead of asserting.

Design and planned work: `docs/plans/2026-09-02-setup-wizard-rust.md`.
