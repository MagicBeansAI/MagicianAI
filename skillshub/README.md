# skillshub/

Source of every skill magician ships with. Git-of-record;
`$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/` (default
`~/MagicianNotes/scopes/<principal>/<workspace>/skills/`) is regenerable runtime
state populated from this folder. Operator scripts that still need that live
root call `scripts/runtime_root_shim.py`; Magician-launched skills do not
inherit the engine root. The docs-guard gate lints new direct resolution. The checked-in `magician_data_v3/` tree contains
package/configuration seeds and is not an installation destination. The former system-shared install tier is
retired.

Current bundle version: **0.2.1**. See [CHANGELOG.md](CHANGELOG.md).

The agent coding-delegate skills `claude`, `codex`, `agy`, and `opencode`
are removed. Magician agents code through `run_coding_task`. Operators
still launch those CLIs from Developer Mode. `websearch-via-claude` and
`deep-research-with-claude` stay.

## Bot SDK structured chat contract

`@magician/bot-sdk` `0.3.6` consumes the same canonical structured-response
plain-text projection as Web and iOS. In particular, an empty rich-result
summary projects the first non-empty text content block, so validated
server-produced sidecars deliver the same message across channels. Its
Magician transport now requires a workspace-bound bearer and never serializes
principal/workspace selectors into API, realtime, or artifact requests.

## Drop-in workflow

Adding a new tool, procedure, or personality skill starts with one
`SKILL.md`. A tool declares its versioned runtime/auth contract and typed
actions in that frontmatter. It ships a `bin/` executable only when real
provider protocol, normalization, artifact, policy, browser, or native behavior
cannot be expressed by the generic runtime. Capability-backed discovery,
local extraction, and Observe projection are optional typed blocks in that
same frontmatter:

```
skillshub/<name>/
├── SKILL.md            # complete AgentSkills + governed runtime/product contract
└── bin/<name>          # optional executable exposure; never generic plumbing
```

```bash
make install-scope SCOPE=<p>/<w>       # → ~/MagicianNotes/scopes/<p>/<w>/skills/
```

Validation runs at install and registration: the governed runtime contract,
typed mappings, auth/profile/storage combination, executable identity, limits,
and catalog metadata are checked before the package can publish tools.
`document-to-markdown` is the reference compiled-library boundary: its small
standalone Rust executable links AnyDoc, isolates untrusted document parsing
from the backend process, and remains an ordinary typed CLI to the shared runtime.
`metadata.magician.content_source`, `.content_reader`, and `.observe_source`
ownership and version structure are checked against that same active action
contract. Separate product metadata sidecars are rejected. All legacy built-in tool
packages have migrated, and new packages are governed from inception; former
executable trees and duplicate `tool_schema.yaml` files
were deleted at final closure as recorded in the
Phase 7 migration ledger.
Selected schema-only public-contract baselines remain under the Magician test fixture
root, outside every skill installation and discovery path.
**Per-name shadowing** means a workspace's `<name>/`
wins entirely over the system's; no merging within a name. The runtime
auto-discovers via `scope_loader::procedure_tool_infos_for_agent` — the next chat
session sees it, no process restart. Content-source registry construction scans
the same ordered roots and applies the same shadowing rule to embedded product extensions.

**Keyed skills declare their hosts.** A skill that takes an API key
(`runtime_contract.auth.kind: secrets`) should list the exact HTTPS hosts its
code contacts under `metadata.magician.app_egress`. Use `destination: <host>`
for one host, or `destinations: [..]` for up to 16 lowercase DNS names on
port 443 (never both fields). When an app runs the skill, the owner's key
reaches only those hosts. A skill that declares none makes the owner pick a
host for each key at install. To find the hosts, read the adapter and any
CLI or SDK it calls, or run the skill once in an app and read the broker
receipt's `contacted` list. See
[skills-spec](../docs/components/magician/skills-spec.md#app_egress-the-hosts-a-keyed-skill-contacts)
and [app OS-jail egress](../docs/components/magician/app-os-jail-egress.md).

This is Magician's validated drop-in boundary: install or place the native CLI,
drop in its `SKILL.md`, and the runtime owns the repetitive auth, argv, cwd,
process, redaction, settlement, and audit plumbing.

## Layout

```
skillshub/
├── Makefile                 # ONE install target
├── README.md                # this file
├── scripts/                 # validator, installer, and setup support
└── <name>/                  # one folder per skill
    ├── SKILL.md             # required (AgentSkills v1 frontmatter + body)
    ├── scripts/             # optional setup, verifier, or support files
    ├── bin/                 # optional links/wrappers exposing installed executables
    ├── config/              # optional compatibility/setup templates; never model auth
    ├── references/          # optional — on-demand context
    └── assets/              # optional
```

The skill `<name>` is the parent directory name, and must equal the
`name` field in SKILL.md frontmatter.

## How a skill is "typed"

Structural inference from SKILL.md frontmatter:

- `metadata.magician.personality:` block present → personality-mode
  (LLM activates via `switch_personality(mode=...)`; runtime writes the
  block content into the agent's `personality_profile` memory tier).
- `metadata.magician.agent:` block present → agent-definition (loader
  registers as agent at scope-bind).
- Neither → procedure (default — LLM-pickable from the tool catalog).

Skills with both blocks are rejected at validation.

A procedure skill with no `scripts/` and no `bin/` is a *plain* skill —
pure-instruction, outer-loop only. With scripts/bin it's a *toolskill* —
spans outer loop (LLM picks) + inner loop (script execution).

## Installation

```bash
make setup-agent-browser              # includes bootstrap PyYAML provisioning
make install-scope SCOPE=anonymous/default
                                       # → ~/MagicianNotes/scopes/anonymous/default/skills/
make setup-env SCOPE=anonymous/default # → populate per-skill config/.env files
```

`setup-agent-browser` is self-contained on a fresh checkout: before it
classifies or materializes skills, it ensures PyYAML is available. It reuses a
working `skillshub/.venv` or system installation, creates a lightweight local
environment with `uv` when available, and otherwise falls back to
`python3 -m pip install --user PyYAML`. The materializer uses that same
interpreter for its child classifier and preserves the classifier's stderr on
failure. `verify-agent-browser` remains the read-only validation surface.
The pinned fork is `0.38.1-Magician.0` on upstream `v0.38.1`; the patch and
rebuild notes live at `skillshub/browser/_vendor/v0.38.1-Magician.0/`.

The setup and installation targets are idempotent. Source-owned files are
refreshed as symlinks. Each
installed package's `config` entry is a relative link to stable scope state at
`<skills>/.skill-state/<name>/config`; `.env` and provider OAuth/session writes
there never participate in a package swap. Reinstallers serialize per skill and
atomically exchange complete package trees, so readers never observe a missing
destination and concurrent provider writes cannot be lost.

`setup-env` reads `$MAGICIAN_ROOT_DIR/operator-config.yaml`, then
`$HOME/MagicianNotes/operator-config.yaml`, then the in-tree fallback. Exact
`$VAR` / `${VAR}` secret references resolve from the setup process environment,
then runtime-root `.env` (production) and `.env.development` (fallback). Setup
copies matching values into each installed skill's `config/.env` from its
`config/.env.example`. This keeps secrets such as `GITHUB_TOKEN` scoped to the
runtime skill directory; example files in git must keep secret values blank.

## Loader contract (what magician relies on)

1. Loader walks `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/<name>/`.
   The retired system location is read only as an empty back-compat fallback.
2. A tool package has one active execution contract: `SKILL.md` frontmatter. Plain
   steering skills omit a runtime contract.
3. The governed coordinator validates typed input, authorizes the route, resolves the
   exact reviewed executable/cwd, materializes only explicitly declared credentials,
   runs with a clean bounded environment, settles durable results, and audits the call.
4. Installed CLIs are invoked directly. A `bin/` executable is permitted only for
   irreducible provider protocol, continuation, normalization, artifacts, policy,
   browser/native control, or verification/support work.
5. Browser/OS sessions stay with their native owner; official remote MCP transport uses
   the shared Rust SDK client and shared Auth Broker. Tools never receive generic vault
   or ledger bearer tokens.

See `docs/components/magician/skills-spec.md` for the full schema and
loader semantics.
