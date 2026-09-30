# Authoring a magician SKILL.md

This guide is for anyone writing a skill from scratch — internal contributors
building a new bundled skill, or third-party authors publishing a skill that
magician operators can install.

For the formal schema + runtime contract, see
`docs/components/magician/skills-spec.md`. This doc is the practical
walkthrough.

## The minimum viable skill

```
my-skill/
└── SKILL.md
```

`SKILL.md` needs two required frontmatter fields and any markdown body:

```markdown
---
name: my-skill
description: One-sentence description of what this skill does and when to use it.
---

# My Skill

Body markdown that the LLM reads as steering when this skill activates.
```

Constraints (enforced by `skillshub/scripts/validate_skill_md.py`):

- `name` — 1-64 chars, lowercase + hyphens only, must match the parent
  directory name byte-for-byte.
- `description` — 1-1024 chars. Plain text. The LLM reads this when scanning
  the catalog.

```bash
python3 skillshub/scripts/validate_skill_md.py my-skill/
```

## Governed executable packages

Executable skills declare one bounded `metadata.magician.runtime_contract`
(`schema_version: tool-runtime.skill-runtime.v1`) plus typed actions in
`runtime_actions`. The loader parses and compiles that package once, caches
the validated package and action catalog, and fails closed if both governed
and legacy schema sources are present. At dispatch, typed input lowers either
to inert argv for a real CLI or to one bounded canonical JSON object on
stdin. Runtime controls remain separate; model input is never interpolated
into a shell command.

Google Workspace is the complete example (`calendar`, `gmail`, `sheets`, and
the three fixed `presto-*` variants). Those packages execute only through the
governed coordinator: profile readiness and expected identity are checked
through their declared lifecycle, product route admission precedes auth
access, and authorization evidence binds credential materialization,
executable/cwd authority, process cleanup, redaction, result settlement, and
audit. A governed capability does not construct the legacy CLI-template
dispatcher. Compatibility fixtures under
`magician/tests/fixtures/tool_runtime_legacy_contracts/` must not be loaded
by active skills.

Choose the executable boundary before authoring:

- Existing CLI: declare and execute the reviewed binary directly. Do not add
  a skill-owned shell/Python/Node wrapper unless identity injection requires
  it (Kapso's `bin/kapso-governed` is that exception).
- MCP server: declare its transport and local policy; the official Rust SDK
  owns MCP protocol, discovery, session, and OAuth.
- HTTP-only provider: keep only the bounded provider client/normalizer.
  Use `tool-runtime.typed-action-overrides.v2` plus
  `input_delivery: canonical_json_stdin`.
- Browser/native capability: keep specialized controller behavior while
  credentials stay in the browser/OS authority.

For a v2 HTTP provider, declare required private stdin with an explicit byte
ceiling. The runtime validates `runtime_actions` once, removes raw stdin from
the model schema, serializes parameters deterministically, and closes stdin
after delivery.

## When to add a body

The body is operator-authored steering the LLM sees on activation. Use it to
explain when to pick this skill, document typed action shapes, and note
auth/rate-limit gotchas. Avoid marketing prose. Keep it under ~500 lines;
move tutorials to `references/`.

## Two skill kinds

### Procedure skill (default)

No special metadata. Appears in the catalog by name + description. LLM picks
it → body becomes steering → `scripts/` files become `<skill>__<stem>`
ephemeral tools.

### Personality-mode skill

Add `metadata.magician.personality:`. It does **not** appear in the main
catalog; it populates `switch_personality`'s mode list.

```markdown
---
name: focused
description: Sharp, terse persona for deep-focus sessions.
metadata:
  magician:
    personality:
      active_mode: focused
      voice: |
        You are focused, terse, no-nonsense...
      expression_bias: |
        Skip pleasantries. Lead with the answer.
      suppression_rules: |
        - Never use emojis.
      expression_triggers: |
        Activate this mode when the user is in a coding session...
---
```

`switch_personality(mode="focused")` writes these fields into the agent's
`personality_profile` memory tier.

## Adding legacy ephemeral scripts

Do not use this path to author a governed executable package. Governed
packages declare their catalog in `SKILL.md` as above.

```
my-skill/
├── SKILL.md
└── scripts/
    └── run.sh
```

On activation, magician registers each `scripts/*.{sh,bash,py,js,mjs}` file
as an ephemeral tool named `<skill>__<stem>`.

### Script env contract

`magician_v2::skills::runner::run_ephemeral` sets:

| Variable | What it is |
|---|---|
| `MAGICIAN_SKILL_DIR` | Absolute path to the resolved skill folder |
| `MAGICIAN_SCOPE_ID` | `<principal>/<workspace>` |
| `MAGICIAN_WORKSPACE` | Workspace portion |
| `MAGICIAN_VAULT_TOKEN` | Vault HTTP API token |
| `MAGICIAN_LEDGER_TOKEN` | Ledger HTTP API token |
| `PATH` | Active skill `bin/` dirs prepended, then scope venv/node bins |

The invocation argument list is passed as process argv after the script
path. The runner does **not** inject `_TOOL_*` environment variables.

Make scripts executable: `chmod +x scripts/*.sh`. The validator and
installer preserve the executable bit on Unix.

## Declaring dependencies

```markdown
metadata:
  magician:
    requires:
      bins: [jq, curl]
      env: [GITHUB_TOKEN]
      python_packages: [requests]
    install_hint:
      brew: "brew install jq"
      docs: "Set GITHUB_TOKEN in vault before activating"
```

At activation the runner runs `check_required_bins`
(`magician_v2::skills::deps`) — missing binaries fail with the install hint.

## bin/ directory

Optional. The runner prepends each active skill's `bin/` to `PATH`. `bin/`
is gitignored at the workspace root; generated outputs come from `_vendor/`
plus a per-skill `Makefile`. A source-owned governed adapter is the
exception: its reviewed executable is force-tracked in `bin/`.

## config/ directory

Optional. Scripts read `$MAGICIAN_SKILL_DIR/config/`. Magician does not
interpret `config/` — ship a `.example`, gitignore the real file.

## Gating a tool with a spend declaration

Declare spend on the governed package under `runtime_catalog.spend` in
`SKILL.md` (not a `tool_schema.yaml` sidecar — the validator rejects that
legacy file). When `resource_authority.enabled` is on, each call debits the
named commodity against the agent's `budgets:` row.

```yaml
runtime_catalog:
  spend:
    type: counted          # also: committed, metered
    commodity: EMAIL_SENDS
    cost_per_action: '1'
```

Pattern in the tree: the **agentmail** pair splits read from write so the
cap only lands on the costly side. `agentmail-read` and `agentmail-send`
are governed packages (`SKILL.md` + `config/` only — no `scripts/`
wrapper). `agentmail-send` declares `runtime_contract` /
`runtime_actions` for `send` / `reply` / `forward`, binds `AGENT_MAIL_KEY`
as a `secret_ref`, and injects it as `AGENTMAIL_API_KEY`. `inbox_id` is an
optional `--inbox-id` override; the default from-inbox is
`magican@agentmail.to`. Spend is 50 `EMAIL_SENDS`/day on the send skill.
The CLI is a workspace dependency (`agentmail-cli` in
`skillshub/package.json`). Seed keys with `make setup-env`.

The **kapso-whatsapp** pair follows the same split for Presto's *own*
WhatsApp number (distinct from the user's personal WhatsApp `whatsapp`
tool): `kapso-whatsapp-read` (free) and `kapso-whatsapp-send` (`send`,
gated to 50 `WHATSAPP_SENDS`/day). Both shell `kapso` through
`bin/kapso-governed`. Auth is `KAPSO_API_KEY` plus
`KAPSO_PHONE_NUMBER_ID` as `secret_ref`s. The wrapper injects
`--phone-number-id` for send and rejects caller-supplied number selectors.
`@kapso/cli` is exact-pinned in the skillshub workspace. Run
`make -C skillshub setup-kapso-cli` for an idempotent version/readiness
check.

## Declaring a bounded Chat-inline adapter

Most non-compiled packs invoked from Chat are task-backed. A small
runtime-owned transport that must stay inside the active conversational
feature declares the adapter on `runtime_catalog`:

```yaml
runtime_catalog:
  chat_inline_adapter: tutor_screen_draw
  expose_timeout_control: false
```

The value is a closed Rust enum and is accepted only when Magician
implements the corresponding adapter. Invented action suffixes do not
inherit it. The current example is `skillshub/screen-draw/SKILL.md`
(`chat_inline_adapter: tutor_screen_draw`).

## references/ and assets/

Both per AgentSkills v1, both optional, both unused by the magician runtime
today.

- `references/` — long-form context the LLM can request later.
- `assets/` — fixtures/templates copied into the runtime tree.

## Folder summary

```
my-skill/
├── SKILL.md             # required
├── scripts/             # optional — *.sh|py|js|mjs become ephemeral tools
│   └── run.sh
├── bin/                 # optional — added to PATH (gitignored unless force-tracked)
│   └── my-binary
├── _vendor/             # optional — binary source, not installed
├── config/              # optional — tool-owned config
│   ├── README.md
│   └── auth.json.example
├── references/          # optional — not yet loaded
├── assets/              # optional — copied at install
└── Makefile             # optional — compiles into bin/
```

## Iteration loop

1. Write the SKILL.md (and scripts only if this is a procedure skill).
2. Validate: `python3 skillshub/scripts/validate_skill_md.py my-skill/`
3. Install: `magician-skills install path:$(pwd)/my-skill --scope anonymous/default`
4. List: `magician-skills list --scope anonymous/default`
5. Test through Forge `/skills` (browse + Allow-for-agent).
6. Reinstall overwrites atomically.

`scripts/magician-skills` is the CLI entry point.

## Common pitfalls

- **Name collision with a compiled provider** — names like `treasurer`,
  `list_agents`, `read_trace`, `duckdb` resolve to magician's built-in
  Rust providers. The dispatch guard refuses to intercept these. Pick a
  different name.
- **Multi-verb behavior in one script** — prefer `scripts/<verb>.sh` per
  verb (`<skill>__<verb>`) over a single `run.sh` with a verb switch.
- **Body too long** — bodies are read every activation. Move detail to
  `references/`.
- **Description that's just the name restated** — the description is the
  LLM's primary signal.
- **Required env not declared** — list it in `requires.env` so activation
  fails clearly.
- **Legacy sidecars** — `tool_schema.yaml`, `content_source.yaml`,
  `content_reader.yaml`, and `observe_source.yaml` are rejected. Put the
  declarations in `SKILL.md`.

## Reference: validator rules

`skillshub/scripts/validate_skill_md.py` enforces:

- Required: `name`, `description`.
- `name` matches AgentSkills naming (`NAME_RE` in the validator) and the
  parent directory.
- `description` ≤ 1024 chars.
- `metadata.magician.agent` is **rejected**.
- `metadata.magician.personality.active_mode` required when the personality
  block is present.
- Optional `content_source` / `content_reader` must use schema version 1,
  bind the owning skill name, and share one `SKILL.md` with
  `runtime_contract` and `runtime_actions`.
- Optional `observe_source` declares Observe projection profiles without
  granting an executable.
- The four former YAML sidecars listed above are rejected.

Run it before installing. CI should run it across `skillshub/`.
