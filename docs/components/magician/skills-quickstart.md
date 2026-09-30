# Quickstart: install a third-party skill

Five-minute walkthrough from "I have a SKILL.md folder somewhere" to "an
agent is using it." Schema and authoring: `skills-authoring.md`. Runtime
contract: `skills-spec.md`. Magician's HTTP API binds
**http://127.0.0.1:3002** by default.

## Prereqs

- A working magician install. `make setup-all` delegates skill setup to
  `skillshub setup-all` (Node 24 LTS tarball → `skillshub/.node/` from
  `.node-version`, workspace npm, agent-browser, skill bins, Python/marimo
  venvs, bots, seed-scope, then `install-scope` into
  `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/`). Checked-in
  `magician_data_v3/` is seed data, not a live destination. Installed
  skills are symlinks back to `skillshub/<name>/`.
- The setup token from `/vault` (sessionStorage
  `magician:vault:setup-token`, or `X-Magician-Setup-Token` on privileged
  API calls).
- A skill folder with a valid `SKILL.md`.

## 1. Validate the skill before installing

```bash
python3 skillshub/scripts/validate_skill_md.py /path/to/your/skill/
```

The HTTP install endpoint runs the same validator (missing frontmatter,
name/dir mismatch, description over 1024 chars) and refuses otherwise.

## 2. Install via CLI (the fastest path)

```bash
scripts/magician-skills install --scope anonymous/default
```

That installs every bundled skill from `skillshub/` into the default
workspace. Add a single skill from elsewhere:

```bash
curl -X POST \
     -H "Content-Type: application/json" \
     -H "X-Magician-Setup-Token: $TOKEN" \
     -d '{"source": "path:/abs/path/to/your/skill", "target": {"workspaces": ["anonymous/default"]}}' \
     http://localhost:3002/api/magician/v2/skills/install
```

Source schemes: `path:<absolute-path>` (local folder) or
`skillshub:<name>` (one skill from this repo, e.g. `skillshub:awk`).

`target.workspaces` must list at least one `principal/workspace`. Install
lands at `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/<name>/`.
There is no system-shared tier: `target.system` is silently ignored and
the handler errors if `workspaces` is empty.

Equivalent Make: `make skills-install-scope SCOPE=anonymous/default`
(there is no `skills-install` target).

## 3. Install via Forge UI

Open `/skills` in Forge. **+ Install**, paste `path:/abs/…` or
`skillshub:awk`, paste `principal/workspace` (comma-separate multiple),
paste the setup token from `/vault`, **Install**. The page still offers a
**system** checkbox; the API rejects a request whose `workspaces` list is
empty. Per-row **Promote** / **Demote** buttons POST routes that are not
registered — install into the destination workspace instead.

## 4. Verify the skill is visible

```bash
scripts/magician-skills list                        # system layer only
scripts/magician-skills list --scope anonymous/default  # workspace + system, with shadowing
scripts/magician-skills list --json
```

`GET /api/magician/v2/skills` is unauthenticated. Workspace-layer entries
come from `X-Principal` / `X-Workspace` (or local anonymous/default);
extra registry roots and compiled built-ins (`kind: compiled`,
`layer: built-in`) are appended. Skill folders win on name collision.

## 5. Allow the skill for an agent

An installed skill is in the catalog but unused until allowlisted.

On `/skills`, **Allow for agent ▾** toggles each agent definition. That
line-edits `definition.agent.yaml` `tools:` (comments preserved).

```bash
curl -X POST \
     -H "Content-Type: application/json" \
     -H "X-Magician-Setup-Token: $TOKEN" \
     -d '{"agent_id": "personal-assistant", "action": "add"}' \
     http://localhost:3002/api/magician/v2/skills/your-skill-name/allow-for-agent
```

`action` is `"add"` or `"remove"`. Optional `scope: "principal/workspace"`
edits `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/agent_runtime/agents/<agent_id>/definition.agent.yaml`.
Without scope, the system agent template is edited.

## 6. Use the skill

On the next session the LLM sees `name`, `description`, and for
personality-mode skills the `switch_personality(mode="...")` list.
Procedure bodies become steering when picked; a body-only skill stays
plain context and the agent uses base tools to act.

Flat-loop leaves (`<pack>__<action>`) dispatch through
`dispatch_flat_action`. Primitive CLI-template skills run through
`invoke_cli_template_capability` in
`primitive_dispatch/capability_invoker.rs` (pack YAML
`implementation.command` + `native_action_schemas` + `arg_mappings`,
scope env, stdout/stderr). Browser still has its primitive dispatcher;
provider-backed packs (`files`, `http`, `duckdb`, `read_trace`) stay on
the compiled-provider path.

`POST /skills/{name}/run` is a **debug one-shot** (schema-driven argv,
default 60 s). It bypasses the LLM loop.

## Updating / removing

Re-install serializes per skill and atomically exchanges the tree.
Runtime-owned config at `<skills>/.skill-state/<name>/config` (relative
`config` link in the package) survives. Re-run the install curl or
`scripts/magician-skills install --scope anonymous/default`. Disk is
re-read on the next tool call.

There is no uninstall HTTP endpoint. Stop an agent using a skill with
allow-for-agent `action: "remove"`. To delete the folder:

```bash
python3 skillshub/scripts/uninstall_skill_layer.py \
  --dest "$HOME/MagicianNotes/scopes/anonymous/default/skills" \
  --only-skill your-skill-name
# or: make -C skillshub uninstall-scope SCOPE=anonymous/default
```

## Common workflows

```bash
# Friend's repo → validate → install → allowlist
git clone https://github.com/them/cool-magician-skill.git /tmp/cool-skill
python3 skillshub/scripts/validate_skill_md.py /tmp/cool-skill
curl -X POST -H "X-Magician-Setup-Token: $TOKEN" \
     -d '{"source": "path:/tmp/cool-skill", "target": {"workspaces": ["anonymous/default"]}}' \
     http://localhost:3002/api/magician/v2/skills/install
curl -X POST -H "X-Magician-Setup-Token: $TOKEN" \
     -d '{"agent_id": "personal-assistant", "action": "add"}' \
     http://localhost:3002/api/magician/v2/skills/cool-skill/allow-for-agent
```

To ship into skillshub: move to `skillshub/<name>/`, `make skills-validate`,
commit, then `make skills-install-scope SCOPE=anonymous/default` (or
`make setup-all`, which runs `skillshub setup-all` including
`install-scope`).

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `name 'X' does not match parent dir 'Y'` | folder ≠ `name:` | rename folder or field |
| Install 401 | missing/expired setup token | copy a fresh token from `/vault` |
| Install 400 `target.workspaces` | empty workspaces / only `target.system` | pass at least one `principal/workspace` |
| Installed but missing in `/skills` | UI cache | Reload / hard-refresh |
| Allowlisted but never picked | description too generic | add concrete "when to use" wording |
| Non-zero exit | missing env or `requires.bins` | `[SKILLS-DISPATCH]` warn lines |
| `skill discovery failed` | malformed SKILL.md in the runtime tree | validate `$MAGICIAN_ROOT_DIR/scopes/*/*/skills/` |
| Name collides with a built-in (`treasurer`, …) | structural guard | rename the skill |
| Promote / demote 404 | routes not registered | install into the destination workspace |

## Reference: API endpoints

All under `/api/magician/v2/skills`:

| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/skills` | none (`X-Principal` / `X-Workspace`) | list installed skills + compiled built-ins |
| POST | `/skills/install` | X-Magician-Setup-Token | install `path:` / `skillshub:` into `target.workspaces` |
| GET | `/skills/{name}/schema` | none | parsed `tool_schema.yaml` (workspace wins) |
| POST | `/skills/{name}/run` | none | debug one-shot CLI (`action`, `args`, `session_params`, `timeout_secs`) |
| POST | `/skills/{name}/allow-for-agent` | X-Magician-Setup-Token | add/remove skill in agent's `tools:` list |

CLI: `scripts/magician-skills` — install, list, validate. Allow-for-agent
is HTTP+UI only. Schema/run power the `/debug` Direct Actions panel.
