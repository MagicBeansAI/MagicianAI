---
name: ocr
version: 0.3.2
description: 'Transcribe text from images and scanned/image-only PDFs. Prefer document-to-markdown for
  ordinary document reading and pdftotext for PDF-specific extraction controls. For visual layout,
  clipping, overlap, margins, or legibility review, use image analysis instead. Inspect images already
  attached to a vision-capable model directly; reuse complete extracted text. Choose --engine explicitly:
  tesseract for clean simple print; an installed, authenticated agy or codex for complex transcription,
  honoring user provider/model choices. Missing binaries, authentication, permission, and path errors
  need their cause fixed rather than repeated OCR attempts.'
metadata:
  magician:
    requires:
      bins:
      - ocr
      # Default-engine setup dependencies. Operator-installed VLM CLIs are
      # optional PATH companions in runtime_contract.requires below, not
      # activation requirements for hosts that only use tesseract.
      - tesseract
      - pdftoppm
      - python3
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
      # Every governed route, this implicit CLI-owned one included, runs in the
      # task's work folder, so the runner stages this package's fixture there
      # (`tests/tool_skill_canary.rs` — `governed_working_directory` and
      # `stage_packaged_fixtures`).
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
        - agy
        - codex
        - python3
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
          timeout_secs: 86400
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
          description: |-
            Transcribe text from a scan or image. Prefer document-to-markdown for
            ordinary document reading; use image analysis for layout, clipping,
            overlap, margins, or visual legibility review. Inspect an image already
            attached to the current vision-capable model directly. Reuse complete
            extracted text instead of re-running OCR.
            Set --engine explicitly: tesseract for clean simple print; an installed,
            authenticated agy or codex for complex transcription. Honor explicit
            provider/model choices. Fix path and permission errors before retrying.
            If an engine is unavailable, use a known ready, permitted alternative
            only when the user's provider choice allows it; avoid blind retries.
            Use --first-page/--last-page for only the pages that need transcription.
            --vlm-prompt overrides the default, which skips handwriting; explicitly
            request all handwriting when the user's task requires it.
            Other flags: --input-file (required), --language (tesseract),
            --agy-model, --codex-model, --concurrency, --timeout-secs, --output-file.
          fixed_args:
          - extract
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens after `ocr extract`. Examples: ["--input-file","scan.pdf","--engine","agy","--first-page","1","--last-page","3"]
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
              description: 'Exact argv tokens after `ocr extract`. Examples: ["--input-file","scan.pdf","--engine","agy","--first-page","1","--last-page","3"]
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
              description: 'Exact argv tokens after `ocr extract`. Examples: ["--input-file","scan.pdf","--engine","agy","--first-page","1","--last-page","3"]
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
      timeout_default_secs: 86400
---

# Ocr

Tool name: `ocr`
Inner actions: `extract`, `raw`, and `help`.

## Select by the requested outcome

- For ordinary document text, use `document-to-markdown` first; keep `pdftotext`
  for PDF-specific extraction controls. Reuse complete extracted text.
- For clipping, overlap, margins, spacing, or visual legibility, use a granted
  image-analysis tool on the relevant rendered page. OCR transcribes text; it
  does not establish visual quality.
- If an image is already attached to the current vision-capable model, inspect
  it directly. A file path alone does not mean the model has seen the image.
- Use OCR when transcription is needed from a scan/image or an unsupported or
  empty PDF text extraction. Reported truncation alone is not a scan signal.
- Fix path, permission, or resource errors before retrying; they are not format
  errors. If an engine is missing or unauthenticated, use a known ready, permitted
  alternative only when the user's provider choice allows it; avoid blind retries.

## Engine selection for transcription

Set `--engine` explicitly. Use Tesseract for clean simple print; for complex
transcription choose an installed, authenticated VLM engine, honoring explicit
provider/model choices and known account availability. The supported VLM engines
are currently `agy` and `codex`; other installed CLIs are not implemented here.

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

## Three engines — full reference

| Engine | Pick for | Avoid for |
|---|---|---|
| `tesseract` (default) | Clean machine-printed pages, high-DPI book scans, plain-text PDFs that returned empty from pdftotext, simple receipts in good lighting | Photos, whiteboards, anything with handwriting you want to ignore, multi-column / table layouts |
| `agy` | Screenshots, receipts in photos, whiteboards, mixed printed+handwritten pages where you want only the printed part, multi-column documents, tables, exam papers, contracts with handwritten margins, anything where text reading order matters. Select according to the requested provider/model and available login. | Bulk OCR of thousands of clean PDFs (cost), offline environments without network, situations where determinism is required |
| `codex` | Complex transcription and mixed content via OpenAI vision. Select according to the requested provider/model and available login. | Same caveats as agy (per-call cost, network, non-determinism) |

**Default engine is `tesseract`.** Pass `--engine agy` or `--engine codex` for the harder cases above. Both VLM engines use provider quota and require their respective CLI on PATH plus completed auth.

The skill uses the governed implicit CLI-owned session lane. That exposes the
user's existing CLI home only to this reviewed adapter: Tesseract remains usable
without a login, while `agy` and `codex` rely on their own previously completed
CLI login and return their native authentication error when the conditional lane
is unavailable.

## Why three engines

- Tesseract is fast (<0.3s init, 0.77s/page) and free, but cannot distinguish handwriting from print — it OCRs both. It also struggles with skewed text, low-res screenshots, tables, and any non-trivial layout.
- Antigravity (`agy`) with a Google vision model and OpenAI's models (via `codex exec -i FILE`) are vision LLM paths. They understand layout, can be prompted to skip handwriting, handle tables and multi-column content natively, and produce clean markdown. The tradeoffs are per-call cost, network dependency, and variable latency: a valid page call can take several minutes.
- Select between available VLM engines using the user's provider/model choice and known login or quota state. Do not make duplicate provider calls merely for reassurance.

## Strategy for PDFs

Try `document-to-markdown` first for an ordinary PDF. If it reports the PDF as unsupported/image-only, use this tool. Keep `pdftotext` for PDF-specialist extraction such as bounded page ranges, physical layout, TSV/bounding boxes, or passwords. The default tesseract engine works for clean monochrome scans; switch to `--engine agy` or `--engine codex` when the PDF has photos, mixed content, complex layouts, or handwritten annotations to skip.

For scanned PDFs, the tool converts each page to a 300 DPI PNG via `pdftoppm`, then runs the chosen engine page-by-page. VLM engines run pages in parallel by default (`--concurrency 4`); latency depends on the selected model and account. Use `--first-page` and `--last-page` to limit transcription to the relevant pages.

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
- `--agy-model`: optional Antigravity model; omitted uses its configured default.
- `--codex-model`: optional Codex model; omitted uses its configured default. Unavailable explicit models fail with the CLI diagnostic; no silent fallback.
- `--timeout-secs`: optional positive per-image VLM deadline. Omit to use the owning invocation budget; there is no separate 240-second cutoff. Long batches belong to tracked tasks, with checkpoints and cancellation.
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
- VLM engines run with `cwd=/tmp`; Codex receives its prompt on stdin with non-repository mode and a read-only sandbox. CLI failures retain exit status and stderr diagnostics.
