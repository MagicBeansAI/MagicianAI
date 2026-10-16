---
name: ocr
version: 0.3.0
description: 'Extract text from images and scanned PDFs. Three engines — `--engine` MUST be chosen explicitly
  based on input shape: (1) `--engine agy` (or `codex`) for ANY photo, screenshot, receipt, whiteboard,
  scanned exam/form, page with handwriting to skip, multi-column or table layout, low-resolution image,
  skewed or rotated text, non-Latin script, or anything where reading order / layout fidelity matters.
  (2) `--engine tesseract` (default, free) ONLY for clearly clean machine-printed pages at high DPI with
  simple single-column layout and no photographic noise. When in doubt about which class the input falls
  into, pick agy — the cost is ~$0.001–0.005 per page and accuracy is meaningfully higher. Try
  `document-to-markdown` first for ordinary PDFs and documents; use `pdftotext` when exact PDF layout,
  ranges, bounding boxes, or password flags matter. Use this tool for scans, image-only PDFs, and images.'
metadata:
  magician:
    requires:
      bins:
      - ocr
      # PATH companions for the default engine. The governed child gets a
      # cleared environment whose PATH is built only from the directories
      # that resolve this list, so `tesseract` and `pdftoppm` — which the
      # adapter spawns by bare name — were not on it and every tesseract
      # call died with `[Errno 2] No such file or directory: 'tesseract'`.
      # Declaring them adds their real directory (Homebrew's, here) to the
      # governed PATH.
      #
      # Naming them does NOT make them hard requirements. A companion is
      # never bound as execution authority, so a host that lacks one simply
      # gets a shorter PATH and keeps every other engine
      # (`governed_executable_directories` in the governed runtime). Only the
      # entrypoint below must resolve. `agy` and `codex` stay undeclared for a
      # different reason: they are operator-installed CLIs that the runtime
      # neither ships nor vendors, and the adapter already reports their
      # absence in its own words.
      - tesseract
      - pdftoppm
    install_hint:
      docs: requires `python3`. tesseract engine also needs `tesseract` + `pdftoppm` (brew install tesseract
        poppler). agy engine needs `agy` CLI on PATH. codex engine needs `codex` CLI (npm i -g @openai/codex)
        + `codex login`.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: extract
      # A PNG cannot be authored as a manifest `fixtures:` string, so the
      # package ships the page instead, at `canary-fixtures/canary-scan.png`;
      # `install-scope` links every package file into `<scope>/skills/<name>/`,
      # so the path below names the installed fixture relative to the scope
      # root.
      #
      # The scope root is the governed working directory for a static-secret
      # CLI, but NOT for this one. `auth.kind: cli_profile` with
      # `profile_selection.mode: implicit` and `storage.kind: cli_owned` — the
      # three facts below that let `agy` and `codex` reach their own completed
      # logins — route this skill through the implicit-CLI lane, whose working
      # directory is the *calling process's* current directory rather than the
      # scope.
      #
      # No relative spelling can name both bases, because they are different
      # directories. So this declaration keeps the one spelling that means
      # something — the installed path, relative to the working directory — and
      # the pairing of a fixture with a skill's real working directory is
      # resolved where it belongs: the canary runner reads the same three auth
      # fields the router reads, and materializes this package's fixture under
      # the base this skill will actually be launched in
      # (the runner's `tests/tool_skill_canary.rs` —
      # `governed_working_directory` and `stage_packaged_fixtures`).
      #
      # The tesseract engine is pinned deliberately: it is the only free,
      # offline, deterministic engine, so the probe reports on this package
      # rather than on a VLM provider's mood or quota. Omitting `--output-file`
      # keeps the call read-only.
      input:
        args:
        - "--input-file"
        - "skills/ocr/canary-fixtures/canary-scan.png"
        - "--engine"
        - "tesseract"
      expect:
        # A phrase from the rendered page. Tesseract failing to load, or
        # returning an empty page, cannot produce it — and `/chars` is a
        # positive count the runner rejects at zero.
        stdout_contains: "Tesseract fixture line 4821"
        require_pointers: ["/content", "/pages", "/chars"]
        # The adapter answers a missing binary or an unreadable input with
        # `{"error": ...}` on stderr and a non-zero exit — the right pairing,
        # since the governed runtime parses stderr exactly when the exit code
        # is non-zero. Without this pointer the lane could only report which
        # assertion went missing rather than the account the adapter wrote.
        error_pointer: "/error"
        max_latency_ms: 60000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - ocr
        - tesseract
        - pdftoppm
        # Required whenever more than one binary is named — see
        # `validate_requirements` in tool-runtime-core. Without it the contract
        # is invalid, the pack never loads, and every engine is lost, not just
        # the default one.
        entrypoint: ocr
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
          timeout_secs: 600
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: cli_profile
        requirement: conditional
        provider: coding-cli
        profile_selection:
          mode: implicit
        storage:
          kind: cli_owned
      policy_floor:
        approval: ordinary
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        extract:
          description: "OCR an image or scanned PDF. ALWAYS set --engine explicitly — do\nnot rely on\
            \ the tesseract default. Routing:\n  --engine agy  (recommended VLM) → photo / screenshot\
            \ /\n    receipt / whiteboard / scanned exam / form / mixed printed +\n    handwritten / multi-column\
            \ / table / low-res / rotated /\n    non-Latin script. ~$0.001–0.005/page via the configured\
            \ Google model.\n  --engine codex  (alternate VLM) → same cases as agy; pick\n    when Google\
            \ quota is exhausted or user prefers OpenAI vision.\n  --engine tesseract  (default, free)\
            \ → only for clean 300+ DPI\n    book scans, single column, no photos. Or bulk batches where\n\
            \    cost dominates. Or offline env. Or determinism required.\nBetween agy and codex: default\
            \ to agy, switch to codex only\nwhen there's a specific reason (quota, user preference, second\n\
            opinion after a failed agy pass).\nMulti-page PDFs under VLM engines run pages in parallel\
            \ (default\nconcurrency=4) so a 22-page doc finishes in ~2min.\nFlags after `python3 ocr.py\
            \ extract`:\n  --input-file <path>                  (required)\n  --engine tesseract|agy|codex\
            \         (default tesseract)\n  --language <iso3>                    (tesseract only; default\
            \ eng)\n  --vlm-prompt \"<...>\"                 (override default skip-handwriting prompt;\
            \ applies to agy+codex)\n  --agy-model <name>                   (default gemini-2.5-flash)\n\
            \  --codex-model <name>                 (default empty = codex picks from its config)\n  --concurrency\
            \ N                      (parallel VLM workers for PDFs; default 4)\n  --first-page N --last-page\
            \ N         (PDF page range; all engines)\n  --output-file <path>                 (write extracted\
            \ text to file too)\n"
          fixed_args:
          - extract
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens after `python3 ocr.py extract`. Examples: ["--input-file","scan.pdf","--engine","agy","--first-page","1","--last-page","3"]
                or ["--input-file","clean_book_page.png"] for tesseract default.'
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
        raw:
          description: 'Escape hatch: run exact argv tokens after `python3 ocr.py` for any OCR helper
            command not listed above.'
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens after `python3 ocr.py extract`. Examples: ["--input-file","scan.pdf","--engine","agy","--first-page","1","--last-page","3"]
                or ["--input-file","clean_book_page.png"] for tesseract default.'
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
        help:
          description: 'Show OCR helper help. Examples: [], ["extract","--help"].'
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens after `python3 ocr.py extract`. Examples: ["--input-file","scan.pdf","--engine","agy","--first-page","1","--last-page","3"]
                or ["--input-file","clean_book_page.png"] for tesseract default.'
              max_items: 16
              max_item_bytes: 4096
          mappings:
          - type: passthrough
            parameter: args
    runtime_catalog:
      categories:
      - text_extraction
      - ocr
      - image
      - pdf
      - document_processing
      composition_category: data_operations
      expose_timeout_control: true
      timeout_default_secs: 600
---

# Ocr

Tool name: `ocr`
Inner actions: `extract`, `raw`, and `help`.

## Outer-loop engine selection (decide BEFORE calling this skill)

**The outer-loop planner MUST set `--engine` on every call. Do not rely on the default.** Use the table below to map the user's input to an engine. If you can't classify the input, choose `agy` — the cost overhead is small and the accuracy delta on tesseract failures is enormous.

| If the input is… | Use engine |
|---|---|
| A photo, screenshot, mobile capture, or anything from a camera | **`agy`** (or `codex`) |
| A scanned form, exam paper, receipt, whiteboard, business card, ID card | **`agy`** (or `codex`) |
| A page mixing printed text + handwritten annotations (and the user wants to skip the handwriting) | **`agy`** (or `codex`) |
| Multi-column layout, table-heavy page, infographic, magazine spread | **`agy`** (or `codex`) |
| Low-resolution or compressed PNG/JPEG | **`agy`** (or `codex`) |
| Rotated / skewed / curved text (signs, logos, stickers) | **`agy`** (or `codex`) |
| Non-Latin script: CJK, Arabic, Devanagari, Thai, Cyrillic | **`agy`** (or `codex`) |
| The user said anything like "skip the margin notes" or "ignore the handwriting" | **`agy`** (or `codex`) |
| A clean 300+ DPI book scan, single column, plain text, no photos | **`tesseract`** |
| Bulk batch processing of thousands of identical clean pages where cost dominates | **`tesseract`** |
| Offline / air-gapped environment with no network | **`tesseract`** |
| Output must be bit-identical run-to-run (audit / regression test) | **`tesseract`** |

**Choosing between `agy` and `codex`** when both fit:
- Default to `agy` (Google Antigravity with the configured Google model) — generally cheaper and very strong on layout.
- Switch to `codex` (OpenAI vision model via `codex exec -i FILE`) when (a) the Google quota is exhausted or auth is broken, (b) the user explicitly asks for OpenAI, or (c) a previous agy call on the same input class produced poor output and you want a second opinion from a different provider.

**Parse documents before OCR**: use `document-to-markdown` for ordinary text-bearing PDFs and office documents. An `unsupported` result for a PDF is the explicit signal to call this OCR tool. Use `pdftotext` instead when the request needs page ranges, exact physical layout, TSV/bounding boxes, raw stream order, or password flags. Call OCR directly for known scans, images, screenshots, and photographs.

## Three engines — full reference

| Engine | Pick for | Avoid for |
|---|---|---|
| `tesseract` (default) | Clean machine-printed pages, high-DPI book scans, plain-text PDFs that returned empty from pdftotext, simple receipts in good lighting | Photos, whiteboards, anything with handwriting you want to ignore, multi-column / table layouts |
| `agy` | Screenshots, receipts in photos, whiteboards, mixed printed+handwritten pages where you want only the printed part, multi-column documents, tables, exam papers, contracts with handwritten margins, anything where layout fidelity matters. Best when Google quota is preferable. | Bulk OCR of thousands of clean PDFs (cost), offline environments without network, situations where determinism is required |
| `codex` | Same use cases as agy — comparable accuracy on complex layouts and mixed content via OpenAI's vision-capable model. Pick when you want a second opinion vs agy, when your OpenAI quota is the cheaper path, or when codex's specific model yields cleaner output on your input | Same caveats as agy (per-call cost, network, non-determinism) |

**Default engine is `tesseract`.** Pass `--engine agy` or `--engine codex` for the harder cases above. Both VLM engines cost roughly ~$0.001–0.01 per page depending on model and require their respective CLI on PATH plus completed auth.

The skill uses the governed implicit CLI-owned session lane. That exposes the
user's existing CLI home only to this reviewed adapter: Tesseract remains usable
without a login, while `agy` and `codex` rely on their own previously completed
CLI login and return their native authentication error when the conditional lane
is unavailable.

## Why three engines

- Tesseract is fast (<0.3s init, 0.77s/page) and free, but cannot distinguish handwriting from print — it OCRs both. It also struggles with skewed text, low-res screenshots, tables, and any non-trivial layout.
- Antigravity (`agy`) with a Google vision model and OpenAI's models (via `codex exec -i FILE`) are vision LLM paths. They understand layout, can be prompted to skip handwriting, handle tables and multi-column content natively, and produce clean markdown. The tradeoffs are per-call cost, network dependency, and ~1–5s latency per page.
- Having both agy and codex lets the inner-loop pick the provider with available quota or the model that performs best on a given input shape (handwriting, multilingual, complex tables, etc.).

## Strategy for PDFs

Try `document-to-markdown` first for an ordinary PDF. If it reports the PDF as unsupported/image-only, use this tool. Keep `pdftotext` for PDF-specialist extraction such as bounded page ranges, physical layout, TSV/bounding boxes, or passwords. The default tesseract engine works for clean monochrome scans; switch to `--engine agy` or `--engine codex` when the PDF has photos, mixed content, complex layouts, or handwritten annotations to skip.

For scanned PDFs, the tool converts each page to a 300 DPI PNG via `pdftoppm`, then runs the chosen engine page-by-page. VLM engines (agy, codex) run pages in parallel by default (`--concurrency 4`), so a 22-page PDF finishes in ~2 minutes instead of ~15 minutes sequential. Use `--first-page` and `--last-page` to limit which pages are OCR'd (important for large scanned documents).

## Returns

JSON with: `content` (extracted text), `pages` (number of pages processed), `engine` (`tesseract`, `agy`, or `codex`), `method` (e.g. `direct_ocr:tesseract`, `pdf_to_image_ocr:agy`). If `output_file` is provided, the same text is written there and the JSON includes `output_path`.

## Examples

- `extract {"args":["--input-file","screenshot.png","--engine","agy"]}` → OCR a screenshot with Antigravity
- `extract {"args":["--input-file","receipt.jpg","--engine","codex"]}` → OCR a receipt photo with Codex
- `extract {"args":["--input-file","scanned_book_page.png"]}` → tesseract default for a clean book scan
- `extract {"args":["--input-file","exam.pdf","--engine","agy"]}` → extract a scanned exam, **skipping handwritten student answers**
- `extract {"args":["--input-file","contract.pdf","--engine","codex","--first-page","1","--last-page","3"]}` → first 3 pages of a contract with handwritten margin notes — codex drops the margin notes
- `extract {"args":["--input-file","document.tiff","--language","deu"]}` → tesseract in German (VLM engines auto-detect language; --language flag is tesseract-only)

## CLI flags

- `--input-file` (required): path to image or PDF
- `--engine`: `tesseract` (default), `agy`, or `codex`
- `--language`: tesseract language code (default `eng`); install additional packs via `brew install tesseract-lang`. Ignored for VLM engines (auto-detect).
- `--vlm-prompt`: override the extraction prompt for the VLM engines. For agy, the literal `{target}` token is replaced with `@<file>` before sending; codex uses `-i FILE` natively. Default prompt skips handwriting and marks figures with `[FIGURE: ...]` placeholders.
- `--agy-model`: Antigravity model (default `gemini-2.5-flash`).
- `--codex-model`: Codex model. Empty (default) lets codex pick from its `config.toml`. Override with e.g. `gpt-5` or `gpt-4o` for a specific revision.
- `--concurrency`: parallel page workers for VLM engines on PDFs (default 4).
- `--first-page` / `--last-page`: 1-based page range for PDFs. All engines respect this.
- `--output-file`: write the extracted text to this path as well as returning it in JSON.

## Inner-loop operating notes

- **Pick `--engine agy` or `--engine codex` for any of**: screenshots; receipts/photos in non-ideal lighting; pages mixing print + handwriting where only printed content is wanted; tables; multi-column documents; non-Latin scripts where Tesseract is weak (CJK, scene text); anything where the requester said "ignore the handwritten notes" or "skip the margin scribbles".
- **Pick `--engine tesseract` (default) when**: input is a clean machine-printed page, you're processing many pages in batch and cost matters, the network is unavailable, or determinism / reproducibility is required.
- **Choosing between agy and codex**: usually interchangeable for OCR. Prefer the provider where your account has more quota / cheaper rate. Try both on a sample page if accuracy on a particular layout matters.
- For large scanned PDFs with a VLM engine, start with a bounded page range — each page is a separate API call.
- If OCR output will be reused by later steps, pass `--output-file` to create a durable artifact.
- If a PDF is text-bearing, prefer `document-to-markdown`; use `pdftotext` for PDF-specific extraction controls. OCR is for scans/images.
- VLM engines run with `cwd=/tmp` internally so they don't pick up project-specific CLI config — keeps OCR calls clean of project MCP servers and extensions.
