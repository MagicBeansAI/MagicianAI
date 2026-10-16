---
name: find-details-by-username
version: 0.2.0
description: 'Toolskill wrapping Maigret as findDetailsByUsername: collect public OSINT

  leads for a username across social networks, developer sites, forums, and

  other public web properties. Use for executive-assistant contact/profile

  enrichment, username reconnaissance, account discovery, and finding likely

  public links for a handle. Returns structured JSON plus Maigret Markdown,

  HTML, and NDJSON report paths. Do not use for harassment, credential attacks,

  stalking, doxxing, or bypassing privacy controls.

  '
homepage: https://github.com/soxoj/maigret
license: MIT
metadata:
  magician:
    skill_type: tool
    user_invocable: true
    requires:
      bins:
      - find-details-by-username
    install_hint:
      docs: 'Requires Python 3.10+. Installed into skillshub/.venv by

        `make -C skillshub setup-python`; then use `make -C skillshub

        install-scope SCOPE=<principal>/<workspace> NAMES=find-details-by-username`.'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          its search action probes hundreds of third-party sites for a real
          person's accounts, which is not an acceptable automated probe.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - find-details-by-username
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin:
          mode: required
          sensitivity: private
        working_directory:
          mode: workspace
        limits:
          timeout_secs: 900
          stdin_bytes: 1048576
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
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        search:
          description: 'findDetailsByUsername: search public web profiles for a username with

            Maigret. Returns JSON with report paths, possible public profile URLs,

            and a compact Markdown report preview. Treat results as leads that need

            verification; false positives are possible.

            '
          fixed_args:
          - search
          parameters:
            username:
              type: string
              description: Public username/handle to search.
              required: true
              max_length: 4096
            top_sites:
              type: integer
              description: Value for top_sites.
              default: 500
            all_sites:
              type: boolean
              description: Value for all_sites.
              default: false
            tags:
              type: string
              description: Value for tags.
              default: ''
              max_length: 4096
            exclude_tags:
              type: string
              description: Value for exclude_tags.
              default: ''
              max_length: 4096
            max_connections:
              type: integer
              description: Value for max_connections.
              default: 50
            recursive:
              type: boolean
              description: Value for recursive.
              default: false
            auto_update:
              type: boolean
              description: Value for auto_update.
              default: true
            output_folder:
              type: string
              description: Optional workspace-relative report directory. Absolute paths, traversal, and symlink escapes are rejected.
              default: ''
              max_length: 4096
            preview_chars:
              type: integer
              description: Value for preview_chars.
              default: 16000
            max_profile_urls:
              type: integer
              description: Value for max_profile_urls.
              default: 50
            proxy:
              type: string
              description: Value for proxy.
              default: ''
              max_length: 4096
            print_errors:
              type: boolean
              description: Value for print_errors.
              default: false
          timeout_secs: 900
        parse_url:
          description: 'Parse a public profile/document URL, extract usernames or related IDs, and

            let Maigret search from those leads. Use when the user provides a profile

            URL rather than only a username.

            '
          fixed_args:
          - parse_url
          parameters:
            url:
              type: string
              description: Public profile/document URL to parse.
              required: true
              max_length: 4096
            top_sites:
              type: integer
              description: Value for top_sites.
              default: 500
            all_sites:
              type: boolean
              description: Value for all_sites.
              default: false
            tags:
              type: string
              description: Value for tags.
              default: ''
              max_length: 4096
            exclude_tags:
              type: string
              description: Value for exclude_tags.
              default: ''
              max_length: 4096
            max_connections:
              type: integer
              description: Value for max_connections.
              default: 50
            recursive:
              type: boolean
              description: Value for recursive.
              default: false
            auto_update:
              type: boolean
              description: Value for auto_update.
              default: true
            output_folder:
              type: string
              description: Value for output_folder.
              default: ''
              max_length: 4096
            preview_chars:
              type: integer
              description: Value for preview_chars.
              default: 16000
            max_profile_urls:
              type: integer
              description: Value for max_profile_urls.
              default: 50
            proxy:
              type: string
              description: Value for proxy.
              default: ''
              max_length: 4096
            print_errors:
              type: boolean
              description: Value for print_errors.
              default: false
          timeout_secs: 900
        version:
          description: Show the installed Maigret version through the bounded provider adapter.
          fixed_args:
          - version
          timeout_secs: 30
        help:
          description: 'Show adapter help or Maigret help. Optional args examples: [] or

            ["maigret"].

            '
          fixed_args:
          - help
          parameters:
            args:
              type: string_array
              description: Value for args.
              max_items: 16
              max_item_bytes: 4096
          timeout_secs: 30
    runtime_catalog:
      categories:
      - osint
      - research
      - search
      - social
      - executive_assistant
      composition_category: research
      expose_timeout_control: true
      timeout_default_secs: 900
---

# findDetailsByUsername

This tool uses a bounded Maigret provider adapter to look up public account/profile leads for a
username. Maigret checks a large public site database, writes machine-readable
and human-readable reports, and extracts public profile fields where available.

Official sources:
- GitHub: https://github.com/soxoj/maigret
- Docs: https://maigret.readthedocs.io/

## Actions

- `search`: run a username lookup and return JSON with report paths, profile
  URL leads, a Markdown preview, stdout/stderr tails, and the exact safe command
  shape used.
- `parse_url`: pass a known public profile/document URL to Maigret's `--parse`
  mode, which extracts usernames/IDs from that page and searches from those
  leads.
- `version`: show the installed Maigret version.
- `help`: show adapter and Maigret CLI help.

## Operating Rules

Use this only for legitimate assistant work: contact enrichment, verifying a
public handle supplied by the user, finding public profile links for someone the
user is already dealing with, or investigating the user's own accounts.

Do not use it for harassment, stalking, credential attacks, password-reset
targeting, doxxing, deanonymizing vulnerable people, or collecting private
details beyond what the user explicitly needs.

Report results as leads, not facts. Maigret can produce false positives because
many people share usernames. Summaries must use cautious language such as
"possible profile", "public lead", and "needs verification" unless a profile
clearly corroborates the identity.

Do not run Maigret's built-in `--ai` mode. It sends the generated report to an
external OpenAI-compatible endpoint. Summarize the local Markdown/JSON report
with the user's assistant instead.

Default to a bounded lookup: top 500 sites, 30 second per-site timeout,
50 concurrent connections, no recursion. Enable `all_sites=true`, tags, or
recursion only when the user's request justifies the longer/louder scan.

## Useful Calls

- `search {"username":"alice"}`
- `search {"username":"alice","tags":"us,developer","top_sites":300}`
- `search {"username":"alice","all_sites":true,"recursive":true,"timeout_secs":45}`
- `parse_url {"url":"https://github.com/someone","top_sites":300}`

## Output

The provider adapter prints one JSON object:

```json
{
  "status": "ok",
  "source": "maigret",
  "tool": "findDetailsByUsername",
  "query": {"username": "alice"},
  "output_dir": "<scope>/workdirs/find-details-by-username/alice-...",
  "reports": {"markdown": "...", "html": "...", "json": "..."},
  "profile_urls": [{"url": "...", "source_hint": "report"}],
  "markdown_preview": "...",
  "stdout_tail": "...",
  "stderr_tail": "..."
}
```

Use `markdown_preview` for the immediate answer. Use the report paths for deeper
follow-up, cross-checking, or user-facing deliverables.

When `output_folder` is not passed, the adapter writes beneath the
runtime-authorized workspace at
`workdirs/find-details-by-username/<username-or-url-slug>-<timestamp>`.
An explicit folder is also workspace-relative; absolute paths, traversal, and
symlink escapes fail before Maigret is launched.

## Failure Modes

- `maigret_not_installed`: run `make -C skillshub setup-python`.
- `invalid_output_folder`: choose a workspace-relative report directory that
  does not traverse through a symlink.
- Empty `profile_urls`: username may not exist on scanned sites, scan may be too
  narrow, or sites may have blocked requests. Try a smaller tag set, `top_sites`
  adjustment, or a later retry.
- Timeouts/403/CAPTCHA: treat as inconclusive. Do not overstate absence.
- Very broad scans can take minutes. Use `timeout_secs` and `all_sites` carefully.
