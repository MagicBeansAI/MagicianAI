---
name: dugite
version: 0.2.0
description: Bundled `git` binary for environments without a system git. The governed `dugite.run`
  action executes exact inert argv against the reviewed standalone binary.
metadata:
  magician:
    requires:
      bins:
      - git
    install_hint:
      docs: Run `make setup-skill-bins` once after clone. Populates `skillshub/dugite/bin/git` from the
        npm-installed dugite package.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        args: ["--version"]
      expect:
        stdout_contains: "git version"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - git
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin:
          mode: denied
          sensitivity: public
        working_directory:
          mode: workspace
        limits:
          timeout_secs: 60
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: none
        requirement: none
      policy_floor:
        approval: ordinary
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        run:
          description: 'Run any git subcommand via the bundled dugite git binary. Inspect

            stdout / stderr; many porcelain commands write status to stderr

            (clone progress, push output) — that''s expected, not an error.

            Use `goal_reached` only after the requested git result is

            visible in the action history.

            '
          parameters:
            args:
              type: string_array
              description: 'Argv tokens after `git`. Pass each token as a separate array

                entry (no shell quoting). E.g. `["log", "-n", "10",

                "--oneline"]` not `["log -n 10 --oneline"]`.

                '
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
    runtime_catalog:
      categories:
      - vcs
      - git
      - delegation
      composition_category: vcs_operations
      expose_timeout_control: true
      timeout_default_secs: 60
      expose_working_directory_control: true
      working_directory_parameter: cwd
      working_directory_default: .
---

# Dugite — Bundled Git

## Callable tool (`dugite.run`)

The agent picks `dugite.run` and passes argv tokens after `git`. The universal
runtime resolves the exact declared `git` executable from the active package or
reviewed installation roots, snapshots and revalidates it, and executes the
argv without a shell.

```
dugite.run({ "args": ["status", "--short"] })
dugite.run({ "args": ["log", "-n", "10", "--oneline"] })
dugite.run({ "args": ["worktree", "add", "../wt", "-b", "feature"] })
dugite.run({ "args": ["show", "--stat", "HEAD"], "cwd": "/abs/path/to/repo" })
```

Use this when:
- The host doesn't have a system `git` (containers, restricted
  delegated CLI sandboxes, fresh CI environments).
- You want a pinned, hermetic git version regardless of what the host
  ships.

Prefer the system git when present and known-good — it is faster to spawn and
avoids downloading another copy.

## Implementation notes

`make setup-skill-bins` installs Dugite's prebuilt standalone `git` at
`skillshub/dugite/bin/git`. The binary is reproducible build output and remains
outside git; the one-file `SKILL.md` is the public action/auth/policy contract.
If the package-local binary is absent, normal reviewed executable discovery may
use an installed standalone `git` with the same declared identity.
