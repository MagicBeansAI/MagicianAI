# OfficeCLI Tool Skills

## Decision

Magician exposes OfficeCLI as three tool skills over one shared executable:

| Tool skill | File contract | Primary quality owner |
|---|---|---|
| `office-word` | Local `.docx` creation and editing | `writing-assistant` (Scribe) |
| `office-excel` | Local `.xlsx` creation and editing | `simple-data-analyst` (Tally) |
| `office-powerpoint` | Local `.pptx` creation and editing | `creative-mind` (Muse) |

`executive-assistant` (Vera) carries all three for bounded edits, template
fills, packaging, and cross-format workflows. It delegates substantial writing,
analytical workbook construction, and presentation narrative/design to the
domain owners above. This avoids a generic Office Docs agent whose prompt and
memory would mix three different quality disciplines.

`wealth-manager` also carries `office-excel`, but only for explicitly requested
editable financial workbooks, models, templates, or existing-XLSX edits. Its
persistent store and analytical compute surface remain DuckDB, lightweight CSV
conversion remains csvkit, and dashboard artifacts remain its default visual
output.

Selector routing uses artifact destination rather than the broad word
"spreadsheet": local editable XLSX work routes to `office-excel`; collaborative
owner-account cloud work routes to `sheets`; Presto-identity cloud work routes
to `presto-sheets`; conversion and light CSV transformations route to `csvkit`;
reproducible notebook reports route to `marimo`; and reusable BI assets route
to `metabase`. Multi-action pack descriptions are promoted into bounded
flat-loop search hints, while action descriptions retain operation-specific
detail. Single-action packs keep their description as the sole selector text
so they are not double-weighted.

This follows the existing `gws` pattern: Gmail, Calendar, and Sheets are
separate Magician tools even though they share one CLI and authentication
substrate. Tool selection, least privilege, domain guidance, and evaluation are
cleaner when file types remain separate.

## Upstream capability

[OfficeCLI](https://github.com/iOfficeAI/OfficeCLI) is an Apache-2.0,
cross-platform, headless Office document CLI. It reads, creates, edits, renders,
and validates Word, Excel, and PowerPoint OOXML files without requiring
Microsoft Office. Its important agent-facing properties are:

- structured JSON output and structured error codes;
- semantic views followed by object-model paths and a raw OOXML fallback;
- built-in HTML/PNG rendering for an inspect-edit-render loop;
- atomic batch operations;
- template merge across all three formats;
- embedded format and specialized authoring guides.

Magician pins OfficeCLI `v1.0.135`. The release
asset names and GitHub-published SHA-256 values are committed in
`skillshub/officecli-runtime/release.json`.

## Runtime architecture

```text
office-word ----------+
office-excel ---------+--> skill-local bin/officecli symlink
office-powerpoint ----+                |
                                       v
                  skillshub/officecli-runtime/bin/officecli
                         pinned + checksum verified
```

`make setup-officecli` runs `skillshub/scripts/setup_officecli.py`. The setup
script selects the current OS/architecture asset, downloads the immutable
versioned release URL, verifies SHA-256 before installation, checks
`officecli --version`, and creates the three skill-local symlinks. It installs
nothing outside `skillshub` and does not run upstream's installer.

The tool subprocess environment sets:

- `OFFICECLI_SKIP_UPDATE=1` so an agent execution cannot silently replace the
  reviewed binary;
- `OFFICECLI_NO_AUTO_RESIDENT=1` so document calls do not leave resident
  processes behind.

Multi-operation work should use OfficeCLI `batch` instead of resident mode.

## Exposed surface

Each format tool exposes only the commands needed for document work:

- inspect: `help`, `load_guidance`, `view`, `get`, `query`, read-only `raw`;
- create/edit: `create`, `set`, `add`, `remove`, `move`, `batch`, `merge`;
- verify: `validate`, `view issues`, and rendered `html`/`screenshot` views;
- format-specific additions: Word `refresh`, PowerPoint `swap`.

The tools intentionally do not expose OfficeCLI's `install`,
`config`, `mcp`, `plugins`, `watch`, `open`, `close`, `raw-set`, or `add-part`
commands. Those commands either mutate developer configuration, start
long-lived processes, broaden the raw-write surface, or are unnecessary inside
Magician's compiled tool runtime.

## Artifact workflow

All three skills enforce the same high-level contract:

1. Inspect existing content before mutation.
2. Preserve the original and write a new output unless in-place editing was
   explicit.
3. Load one format-specific embedded OfficeCLI guide.
4. Use semantic views first, object paths second, and raw inspection last.
5. Query built-in help instead of guessing properties.
6. Batch multi-step mutations.
7. Validate the OOXML package, inspect OfficeCLI issues, and visually render
   user-facing layouts before returning the artifact.

PowerPoint additionally requires a narrative/claim review and slide render
loop. Excel additionally requires representative formula/value checks. Word
refreshes fields and cross-references when applicable.

## Setup and scope installation

```bash
make setup-officecli
make -C skillshub install-scope \
  SCOPE=anonymous/default \
  NAMES="office-word,office-excel,office-powerpoint"
make verify-officecli
```

The source skill files are symlinked into the scope, matching the rest of
AgentSkills v1. Agent definitions must list the tool name before it appears in
that agent's effective catalog.

## Known boundaries

- These tools operate on local OOXML files, not Microsoft 365 cloud storage,
  SharePoint, OneDrive, Google Docs, or Google Sheets.
- `.xls`, `.doc`, and `.ppt` legacy binary formats are not part of the
  contracts.
- The pinned setup helper currently supports macOS and glibc/musl Linux on
  arm64 and x86_64; it does not install the upstream Windows release.
- PNG screenshot rendering requires a supported local browser such as Chrome,
  Edge, Chromium, or Firefox. When none is available, use OfficeCLI's HTML view
  and inspect that output with Magician's browser tooling.
- OfficeCLI's renderer is the verification baseline, but high-stakes files
  should still be opened in Microsoft Office or LibreOffice when exact native
  rendering compatibility is material.
- No persistent live-preview server is exposed.
- OfficeCLI is a fast-moving external binary. Version upgrades require updating
  the pinned manifest, reviewing upstream changes, running all three smoke
  fixtures, and then changing the committed checksums.
