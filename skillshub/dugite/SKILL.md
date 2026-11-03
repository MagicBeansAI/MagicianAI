---
name: dugite
version: 0.2.1
description: Bundled `git` binary for environments without a system git. The governed `dugite.run`
  action executes exact inert argv against the reviewed standalone binary.
metadata:
  magician:
    requires:
      bins:
      - git-adapter
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
        # The tracked `git-adapter` is the snapshot-safe entrypoint: dugite's
        # git is built for prefix `/`, so it derives GIT_EXEC_PATH and
        # GIT_TEMPLATE_DIR from the `git` PATH companion's real location (as
        # dugite's own Node wrapper does) and execs it in place.
        entrypoint: git-adapter
        bins:
        - git-adapter
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
runtime snapshots the tracked `bin/git-adapter` entrypoint and executes the argv
without a shell; the adapter runs the declared dugite `git` in place with the
exec path and template directory dugite's own wrapper would set.

```
dugite.run({ "args": ["status", "--short"] })
dugite.run({ "args": ["log", "-n", "10", "--oneline"] })
dugite.run({ "args": ["worktree", "add", "../wt", "-b", "feature"] })
dugite.run({ "args": ["show", "--stat", "HEAD"], "cwd": "/abs/path/to/repo" })
```

`cwd` must be the absolute path of a VibeDev project folder, the task's working
folder or an owner-granted folder, or a path relative to the task's working
folder (a bare project folder name also resolves in the workspace home). Omit it
to use the task's working folder; anything else is refused.

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
`bin/git-adapter` is tracked source. Dugite's git is built for prefix `/`, so
run alone it reports `//libexec/git-core` and finds no templates; the adapter
derives `GIT_EXEC_PATH` and `GIT_TEMPLATE_DIR` (plus `PREFIX` and the bundled CA
file on Linux) from the companion's real path and refuses a host git such as
`/usr/bin/git`. Commit identity comes from the repository's own config; without
one, commits are signed `Magician <magician@localhost>` from the tracked
`bin/dugite.gitconfig`, loaded as git's system layer (never a hostname guess).
To commit as yourself, set `user.name`/`user.email` in that repository.
