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

## Runtime setup and release pins

The setup wizard installs shared software before account login. The component
graph checks governed skill entrypoints and companions, all six bot bundles,
locked npm packages and native payloads. `media-tools` verifies yt-dlp, Node
and the pack-local ffmpeg/ffprobe links. Bundled-browser readiness checks its
executable and core skill. `make verify-skill-runtime` performs these checks
without provider calls. Setup writes inventories under `skillshub/.runtime/`;
graph probes recheck every recorded file and executable permission.

`additional-browser-engines` is a separate optional wizard component. It checks
the actual Cloak Chromium and Lightpanda executables as well as their resolvers;
an installed Python SDK alone cannot pass it. `make setup-browser-engines`
provisions them. Lightpanda follows its official nightly formula; this optional
host engine is separate from the generated CLI artifacts pinned into releases.

Shared npm dependencies install with `npm ci`. GWS 0.16.0, tgcli 2.4.0,
wu-cli 0.1.20 and bot SDK dependencies have explicit package pins. Scoped
bundles resolve modules from their shared symlink targets; auth and workdirs
remain scoped. The bot installer rejects missing bundles/dependencies and
preserves legacy scoped npm installs unless `--prune-legacy` is explicit.

Metabase generation uses `metabase/generator-pin.json`: printing-press v4.0.3,
Go 1.26.1 and a reviewed spec SHA-256. Regeneration records the spec,
post-generation patch, build recipe and resulting checksum in
`metabase/bin/generated.json`. A spec refresh needs a reviewed pin update.
Release packaging rejects stale generated artifacts, includes the Metabase and
document conversion binaries, browser fork and bot bundles, and records their
checksums in `release-tools.json`. It freezes both Python environments.
Packaged setup consumes these artifacts and locked dependencies without
Rust/Go generation. Native artifacts must match the release target; build the
document converter in the release output directory before packaging.

## Higgsfield setup

`make setup-higgsfield-cli` installs the exact `@higgsfield/cli` version in
the shared npm workspace, verifies the package and native executable versions,
and links `higgsfield/bin/higgsfield` directly to the native binary. The npm
package downloads its versioned release and checks its bundled SHA256 before
installation. Generated binaries stay ignored. This target is also part of
`setup-all` and does not require a login. `make verify-higgsfield-cli` checks
the shared installation without downloading or changing credentials.

Authentication is a separate operator step, run from the repo root:

```bash
make setup-higgsfield-cli
./skillshub/higgsfield/bin/higgsfield auth login
make setup-higgsfield-auth SCOPE=<principal>/<workspace>
```

The auth target copies the existing operator login into the selected scope's
isolated HOME. Existing scoped credentials are kept unless `FORCE=1` is set;
token refreshes stay scope-local. The Linux container release manifest must
match the npm version pin when upgrading Higgsfield.

The agent coding-delegate skills `claude`, `codex`, `agy`, and `opencode`
are removed. Magician agents code through `run_coding_task`. Operators
still launch those CLIs from Developer Mode. `websearch-via-claude` and
`deep-research-with-claude` stay.

## Direct OpenAI images

`image-generation-via-openai` `0.1.0` adds governed `generate` and `edit`
actions for GPT Image 2, using the scoped `OPENAI_API_KEY` credential
authority. Each call saves one new PNG/JPEG/WebP artifact in its workspace;
editing accepts an existing image and optional same-size PNG mask. The package
validates paths/options before dispatch, rejects overwrites and redirects, and
makes one provider request without an automatic paid retry.

Its `images[].path` uses the standard artifact collector. Receipts retain
`request_id`, token `usage`, versioned estimated `cost_microusd` and
`pricing_version`; unavailable usage is never substituted with zero. Work
Quality uses its existing metered-tool policy, ledger and sealed file review
(receipt `/request_id`, reported cost `/cost_microusd`, read/write path bindings
and frozen pricing parameters). The package adds no separate review loop.
See [the skill contract](image-generation-via-openai/SKILL.md) for details.

Validate offline with `make -C skillshub test-openai-image`. Install only this
interpreted package with
`make -C skillshub install-scope -o build SCOPE=anonymous/default NAMES=image-generation-via-openai`;
`-o build` skips unrelated native skill builds. The scoped vault secret takes
precedence; the existing private env-file resolver is also supported. For local
development, reuse the existing OpenAI key in the installed skill's private
`config/.env`, including only that key rather than the shared env file. Only
the declared key is read and injected at dispatch. No new global model profile is needed.
Paid live generation/editing is separate from automatic readiness.

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

### Every scope, and scopes that move

A scope's layer is absolute links into this skillshub, and several `bin/`
entries are ignored, generated files (`browser/bin/agent-browser`, OfficeCLI,
the Metabase CLI, document-to-markdown). On 2026-09-30 those files were emptied
from the checkout and every scope's agent-browser link dangled; the runtime
takes pinned CLIs only from the scope, never from `PATH`, so browsing failed in
every workspace. Three rules now keep scopes runnable:

- **Pinned executables are stored, not linked into the checkout.** A `bin/`
  entry that resolves to a Mach-O/ELF executable inside skillshub (outside
  `node_modules/` and `.venv/`) is copied once into
  `<data root>/system/skill-bin/<sha256>/<name>`, and the scope's
  `bin/<name>` is a relative link to it (`scripts/pinned_bin_store.py`). A
  cleaned, re-cloned or moved checkout no longer breaks it, and a copied or
  moved data root keeps it. npm-owned binaries (rg, dugite's git, Higgsfield),
  scripts and host tools (pdftotext, ffmpeg, tesseract) stay plain links.
  `setup-document-to-markdown` copies the Cargo build instead of linking into
  the target dir.
- **Every scope is repaired, not just the one setup named.** `setup-all`, each
  `setup-wizard-*` step and `setup-agent-browser` relink all existing scopes
  (`link_runtime.py --all-scopes`); `make -C skillshub link-scopes` does the
  same on demand, re-points links into a checkout that moved and prunes unused
  stored executables. The service does the offline part itself: at startup and
  the first time a workspace is touched it installs missing skills, re-points
  dangling links at the running skillshub and refreshes the store
  ([skills spec](../docs/components/magician/skills-spec.md#scope-layer-self-repair)).
- **Broken entries are reported with their fix.** `make -C skillshub
  check-scopes` (root: `make skills-check-scopes`) lists each scope's `bin/`
  entry that does not run, with the make target that rebuilds its source. With
  `--data-root`, `verify_runtime.py` adds the scoped files to the `skills`,
  `media` and `browser` inventories by absolute path, so the component graph
  reports a missing scoped CLI instead of a healthy shared install.

`setup-env` reads `$MAGICIAN_ROOT_DIR/operator-config.yaml`, then
`$HOME/MagicianNotes/operator-config.yaml`, then the in-tree fallback. Exact
`$VAR` / `${VAR}` secret references resolve from the setup process environment,
then runtime-root `.env` (production) and `.env.development` (fallback). Setup
copies matching values into each installed skill's `config/.env` from its
`config/.env.example`. This keeps secrets such as `GITHUB_TOKEN` scoped to the
runtime skill directory; example files in git must keep secret values blank.

### Per-scope manifests (rendered, not linked)

Code is shared; a manifest that must differ per workspace is not. A skill whose
`SKILL.md` carries a per-scope value (a self-hosted MCP endpoint and its OAuth
issuer, for example) ships a **valid** template `SKILL.md` using a reserved
placeholder origin (`https://<name>.invalid`, RFC 2606) plus `render.yaml`:

```yaml
schema_version: magician.skill-render.v1
substitutions:
  - placeholder: https://metabase.invalid
    input: METABASE_BASE_URL   # read from <scope>/skills/<skill>/config/.env
    kind: https_origin         # https, host[:port] only; no path/query/userinfo
```

`scripts/skill_render.py` writes the scope's own `SKILL.md` (a regular file)
and `.skill-render.json` (template, input and manifest hashes). `setup-env`
renders right after it fills `config/.env`; `install_skill_layer.py` and
`link_runtime.py` render after linking. Unchanged inputs are left alone, a
changed template or input re-renders, and a hand edit is corrected. Inputs come
only from the scope's own skill config, never the server-wide env, so
workspaces sharing a server never see each other's endpoints. Missing or
invalid input leaves the scope with **no** manifest, so the placeholder never
reaches a catalog. The template and `render.yaml` are never linked into a
scope (the Rust `skills::scope_layer` reconciler follows the same rule), and a
marked manifest stays setup-managed: the skill's other files keep being linked.
A real `SKILL.md` without the marker is still scope-owned and never touched.
Only non-secret values belong in a rendered manifest. `metabase-mcp` is the
first such skill.

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
