---
name: document-to-markdown
version: 0.1.0
description: Convert Word, PowerPoint, Excel, OpenDocument, RTF, EPUB, CSV, and text-bearing PDF files into clean GitHub-Flavored Markdown with the local AnyDoc parser. Use for reading or normalizing documents before reasoning, indexing, summarizing, or transforming them. Use OCR instead for images and scanned/image-only PDFs; use pdftotext for PDF-specific page ranges, bounding boxes, passwords, or exact physical-layout extraction.
metadata:
  magician:
    content_reader:
      schema_version: 1

      reader:
        id: document-markdown
        display_name: Public document Markdown
        class: web_page
        cache_ttl_secs: 900
        max_response_bytes: 33554432
        max_text_chars: 4194304
        min_gist_chars: 80
        min_full_text_chars: 200
        fetch_timeout_secs: 30
        max_redirects: 5
        accepted_media_types:
          - application/pdf
          - application/msword
          - application/vnd.ms-word.document.macroenabled.12
          - application/vnd.openxmlformats-officedocument.wordprocessingml.document
          - application/vnd.ms-powerpoint
          - application/vnd.ms-powerpoint.presentation.macroenabled.12
          - application/vnd.ms-powerpoint.slideshow.macroenabled.12
          - application/vnd.openxmlformats-officedocument.presentationml.presentation
          - application/vnd.openxmlformats-officedocument.presentationml.slideshow
          - application/vnd.ms-excel
          - application/vnd.ms-excel.sheet.binary.macroenabled.12
          - application/vnd.ms-excel.sheet.macroenabled.12
          - application/vnd.openxmlformats-officedocument.spreadsheetml.sheet
          - application/vnd.oasis.opendocument.text
          - application/vnd.oasis.opendocument.spreadsheet
          - application/vnd.oasis.opendocument.presentation
          - application/rtf
          - text/rtf
          - application/epub+zip
        retrieval:
          action_id: document_markdown.read
          operation: read
          rung: public_static
          priority: 200
          outputs: [gist, full_text]
          authority: public_remote_read
          parallel_safe: false

      capability:
        name: document-to-markdown
        action: convert

      input:
        input_file_argument: input_file
        max_chars_argument: max_chars
        fixed_arguments:
          format: auto
          pretty: false

      output:
        mode: json
        text_pointer: /content
        method_pointer: /method
        error_pointer: /error/message
    requires:
      bins:
      - document-to-markdown
    install_hint:
      docs: Run `make -C skillshub build`, then install this skill into the active scope. Building requires Rust 1.88 or newer.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: convert
      # `convert` is the read-only action: it returns Markdown and writes
      # nothing. `convert_to_workspace` creates files and must never be canaried.
      #
      # `input_file` is a brokered workspace_path, so the governed runtime
      # resolves it beneath `<scope>/workdirs/` rather than the scope root. The
      # fixture is therefore materialized at `workdirs/canary-fixtures/...`
      # while the parameter names the same file relative to that root.
      #
      # CSV is the one supported format that is authorable as manifest text; it
      # is signature-less, so `format: csv` is required rather than optional.
      input:
        input_file: "canary-fixtures/document-to-markdown-input.csv"
        format: "csv"
        pretty: false
      fixtures:
        "workdirs/canary-fixtures/document-to-markdown-input.csv": |
          Region,Units,Revenue
          North,120,4800
          South,95,3800
      expect:
        # A rendered GFM row proves AnyDoc parsed and re-emitted the table.
        # `/chars` is a positive count, and the runner reads zero as absent —
        # which is exactly the "converted nothing" outcome worth failing on.
        stdout_contains: "| North | 120 | 4800 |"
        require_pointers: ["/content", "/chars", "/format"]
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - document-to-markdown
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
          timeout_secs: 120
          memory_bytes: 4294967296
          stdout_bytes: 33554432
          stderr_bytes: 1048576
      auth:
        kind: none
        requirement: none
      policy_floor:
        approval: ordinary
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      actions:
        convert:
          description: Read one supported workspace document and return GitHub-Flavored Markdown without writing files. The parser detects formats from content, falling back to the filename extension for signature-less CSV. Scanned/image-only PDFs return an explicit unsupported error and must be sent to OCR.
          fixed_args:
          - convert
          parameters:
            input_file:
              type: workspace_path
              access: read_file
              description: Local DOC, DOCX, DOCM, PPT/PPS/POT, PPTX/PPTM/PPSX/PPSM, XLS/XLSX/XLSM/XLSB, ODT/ODS/ODP, RTF, EPUB, CSV, or text-bearing PDF file. Maximum 128 MiB.
              required: true
              max_length: 4096
            format:
              type: string
              description: Optional format override. Keep auto unless an extensionless upload or incorrect filename requires an explicit parser.
              default: auto
              enum_values: [auto, doc, docx, docm, odt, pdf, ppt, pps, pot, pptx, pptm, ppsx, ppsm, rtf, epub, xls, xlsx, xlsm, xlsb, ods, odp, csv]
              max_length: 16
            max_chars:
              type: integer
              description: Optional character limit for returned Markdown. A separate 4 MiB encoded ceiling protects JSON/stdout.
              minimum: 1
              maximum: 16777216
            pretty:
              type: boolean
              description: Pretty-print response JSON for human inspection.
              default: false
          mappings:
          - {type: flag, flag: --input-file, parameter: input_file}
          - {type: flag, flag: --format, parameter: format}
          - {type: flag, flag: --max-chars, parameter: max_chars, omit_if_empty: true}
          - {type: bool_flag, flag: --pretty, parameter: pretty}
          timeout_secs: 120
        convert_to_workspace:
          description: Convert one supported workspace document and create a new Markdown file, optionally with a new embedded-assets directory. Existing destinations are never replaced.
          fixed_args: [convert]
          parameters:
            input_file:
              type: workspace_path
              access: read_file
              description: Supported source document beneath the active workspace.
              required: true
              max_length: 4096
            output_file:
              type: workspace_path
              access: create_file
              description: New Markdown file beneath an existing workspace directory. The destination must not already exist.
              required: true
              max_length: 4096
            format:
              type: string
              description: Optional format override. Keep auto unless an extensionless upload or incorrect filename requires an explicit parser.
              default: auto
              enum_values: [auto, doc, docx, docm, odt, pdf, ppt, pps, pot, pptx, pptm, ppsx, ppsm, rtf, epub, xls, xlsx, xlsm, xlsb, ods, odp, csv]
              max_length: 16
            assets_dir:
              type: workspace_path
              access: create_directory
              description: Optional new directory for embedded assets from non-PDF documents. The destination must not already exist.
              max_length: 4096
            include_content:
              type: boolean
              description: Also include bounded Markdown content in response JSON.
              default: false
            max_chars:
              type: integer
              description: Optional character limit for inline Markdown. This does not truncate the created Markdown file.
              minimum: 1
              maximum: 16777216
            pretty:
              type: boolean
              description: Pretty-print response JSON for human inspection.
              default: false
          mappings:
          - {type: flag, flag: --input-file, parameter: input_file}
          - {type: flag, flag: --output-file, parameter: output_file}
          - {type: flag, flag: --format, parameter: format}
          - {type: flag, flag: --assets-dir, parameter: assets_dir, omit_if_empty: true}
          - {type: bool_flag, flag: --include-content, parameter: include_content}
          - {type: flag, flag: --max-chars, parameter: max_chars, omit_if_empty: true}
          - {type: bool_flag, flag: --pretty, parameter: pretty}
          timeout_secs: 120
          policy:
            additional_approvals: [delegated_workspace_write]
    runtime_catalog:
      categories:
      - document_processing
      - text_extraction
      - markdown
      - pdf
      - office
      - data_processing
      composition_category: data_operations
      expose_timeout_control: true
      timeout_default_secs: 120
---

# Document to Markdown

Convert supported local documents into clean Markdown through the isolated,
compiled AnyDoc binary. The conversion is local, deterministic, and requires no
credentials or external service.

## Route by input, not by filename alone

1. Use this skill first for ordinary Word, PowerPoint, spreadsheet,
   OpenDocument, RTF, EPUB, CSV, and text-bearing PDF reading.
2. Use `ocr` directly for images, screenshots, photographs, and documents known
   to be scans.
3. When a PDF conversion returns `unsupported`, treat it as an OCR-routing
   signal and call `ocr`; do not retry AnyDoc with a forced PDF format.
4. Use `pdftotext` instead when the task needs PDF page ranges, physical layout,
   raw content-stream order, TSV/bounding boxes, or password flags.

AnyDoc detects signed/container formats from bytes. CSV has no signature, so an
extensionless CSV upload needs `format: "csv"`.

## Output choices

- Use `convert` for normal reads. The JSON response contains `content`,
  `format`, character and byte counts, and a concise preview.
- Use `convert_to_workspace` when later steps need a durable Markdown artifact.
  Its `output_file` and optional `assets_dir` are normalized relative paths
  beneath the active scope's dedicated `workdirs/` tree. Absolute paths,
  traversal, symlink traversal, and existing destinations are rejected by the
  runtime before the process starts.
- Set `include_content: true` only when both the durable file and inline content
  are genuinely needed.
- Set `max_chars` to bound only the inline JSON content. The response reports
  `content_truncated: true`; `convert_to_workspace` always keeps complete
  Markdown in its created output file.
- Set `assets_dir` to export embedded assets from non-PDF document models.
  AnyDoc's Markdown uses available textual alternatives; exporting assets does
  not perform visual analysis of them.

The wrapper accepts regular files up to 128 MiB. AnyDoc's own decompression,
nesting, node-count, expansion, and retained-asset limits remain active. The
standalone process boundary ensures a malformed parser input cannot unwind the
governed host runtime. A 4 GiB per-process address-space limit and shared 8 GiB
reservation budget cap concurrent parser pressure. Markdown and asset outputs
are create-only; an asset set becomes visible as one complete directory rather
than as a partially copied tree. Failures return a bounded stable code/message
envelope so callers can distinguish `unsupported` from generic execution
failures and route scanned PDFs to OCR.

## Examples

- `convert {"input_file":"brief.docx"}`
- `convert {"input_file":"upload","format":"csv"}`
- `convert_to_workspace {"input_file":"deck.pptx","output_file":"deck.md","assets_dir":"deck-assets"}`
- `convert {"input_file":"report.pdf"}`; if it returns `unsupported`, route
  the same document to `ocr`.
