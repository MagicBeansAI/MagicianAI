# Upstream

Vendored from [wheresryan22/anatomy](https://github.com/wheresryan22/anatomy) at commit `454a0ef` (2026-10), under its MIT licence (`LICENSE`).

Local changes:

- **`SKILL.md` frontmatter.** Adds `version` and `metadata.magician.skill_type: procedure`, so Magician installs it as a procedure skill. Nothing else in the frontmatter or body changed.
- **Built example pages left out.** The pages under `examples/` are build output and are not committed. `node examples/<name>/build.mjs [--light]` writes them, and `.gitignore` keeps them out.
- **Whitespace cleanup.** Removes trailing blank-line spaces in `references/webgl.md` and `scripts/turn-fidelity.mjs`, and the extra final blank line in `examples/test-rig/view.ts`, for the repository's diff checks.
- **Upstream GitHub scaffolding left out.** CI workflows and issue/PR templates belong to the upstream repository and are not needed by this procedure skill. README illustrations under `.github/assets/` remain.

Refresh: copy a newer upstream checkout over this folder with the same exclusions (`.git`, `node_modules`, `examples/**/*.html`, `.github/workflows/`, `.github/ISSUE_TEMPLATE/`, `.github/PULL_REQUEST_TEMPLATE.md`), reapply the frontmatter and whitespace cleanup, and update the commit above.

Users in this repo:

- `showcase/anatomy/explorer/record.mjs` uses `scripts/chrome-port.mjs`.
- `scripts/drive.mjs` and `scripts/capture.mjs` drive the showcase pages headless.
- Claude Code finds the skill through `.claude/skills/anatomy`, a symlink to this folder.

The contribution guide CI link points to upstream because this vendored
package excludes its GitHub automation and issue templates.
