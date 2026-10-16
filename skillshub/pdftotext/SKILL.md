---
name: pdftotext
version: 0.2.0
description: Extract exact plain text, page ranges, physical layout, TSV/bounding boxes, raw stream order, or password-protected content from PDF files with Poppler. Use for PDF-specialist extraction; prefer document-to-markdown for ordinary document reading and OCR for scanned/image-only PDFs.
metadata:
  magician:
    content_reader:
      schema_version: 1

      reader:
        id: pdf-text
        display_name: Public PDF text
        class: web_page
        cache_ttl_secs: 900
        max_response_bytes: 33554432
        max_text_chars: 4194304
        min_gist_chars: 80
        min_full_text_chars: 200
        fetch_timeout_secs: 20
        max_redirects: 5
        accepted_media_types:
          - application/pdf
        retrieval:
          action_id: pdf_text.read
          operation: read
          rung: public_static
          priority: 300
          outputs: [gist, full_text]
          authority: public_remote_read
          parallel_safe: false

      capability:
        name: pdftotext
        action: run

      input:
        input_file_argument: input_file
        max_chars_argument: null
        fixed_arguments:
          output_file: "-"
          flags: -nopgbrk
          timeout_secs: 20

      output:
        mode: text
    requires:
      bins:
      - pdftotext
    install_hint:
      docs: 'requires binary on PATH: pdftotext'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      # A PDF does not have to be binary. This one is a hand-built,
      # uncompressed, pure-ASCII single page whose xref offsets are exact, so
      # the probe stays self-contained in the manifest rather than depending on
      # a committed binary or on a document that happens to exist on the host.
      # `output_file: "-"` keeps the call read-only; the default would write
      # `{input_file}.txt` beside the fixture.
      input:
        input_file: "canary-fixtures/pdftotext-input.pdf"
        output_file: "-"
        flags: "-nopgbrk"
      fixtures:
        "canary-fixtures/pdftotext-input.pdf": "%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 106 >>\nstream\nBT /F1 24 Tf 72 700 Td (Tool canary fixture page) Tj 0 -36 Td (Poppler extracted this second line.) Tj ET\nendstream\nendobj\n5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\nxref\n0 6\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \n0000000241 00000 n \n0000000397 00000 n \ntrailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n467\n%%EOF\n"
      expect:
        # The second line proves Poppler walked the content stream rather than
        # echoing a header: an adapter that produced an empty extraction, or one
        # that never reached the page, cannot contain it.
        stdout_contains: "Poppler extracted this second line."
        max_latency_ms: 15000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - pdftotext
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
      schema_version: tool-runtime.typed-action-overrides.v2
      actions:
        run:
          description: Run the pdftotext capability with the arguments selected from this capability guide.
            Inspect stdout/stderr, update the runtime ledger when useful, and call goal_reached only after
            the requested result is present.
          parameters:
            input_file:
              type: string
              description: Path to the PDF file to extract text from
              required: true
              max_length: 4096
            output_file:
              type: string
              description: Path to save extracted text. Defaults to {input_file}.txt. Use '-' for stdout.
              default: '{input_file}.txt'
              max_length: 4096
            flags:
              type: string
              description: Optional pdftotext flags (e.g. '-layout', '-f 1 -l 10', '-nopgbrk'). See guide.
              default: ''
              max_length: 4096
          mappings:
          - type: split_positional
            parameter: flags
            max_items: 64
            max_item_bytes: 4096
          - type: positional
            parameter: input_file
          - type: positional
            parameter: output_file
          timeout_secs: 60
    runtime_catalog:
      categories:
      - data_processing
      - text_extraction
      - pdf
      - document_processing
      composition_category: data_operations
      expose_timeout_control: true
      timeout_default_secs: 60
---

# Pdftotext

Tool name: `pdftotext`
Primary parameter: `input_file`
Requires: pdftotext (poppler-utils) installed on host PATH
Use for: extracting text from PDF files, converting PDFs to plain text,
reading specific page ranges, preserving layout for tabular PDFs.

For ordinary reading or summarization, prefer `document-to-markdown`: it emits
clean structured Markdown and handles office formats as well as text-bearing
PDFs. Keep this skill for Poppler-specific controls such as page ranges,
physical layout, raw ordering, TSV/bounding boxes, or PDF passwords. If either
parser reveals an image-only scan, route the file to `ocr`.

By default output is written to `{input_file}.txt` so later steps can read and reuse it.
Use `output_file: "-"` only when you explicitly want stdout instead of a saved text file.

The `flags` parameter accepts any combination of pdftotext flags:
  -f N        first page to extract (1-based)
  -l N        last page to extract (1-based)
  -layout     maintain original physical layout (good for tables)
  -raw        keep strings in content stream order
  -nopgbrk    don't insert page breaks between pages
  -htmlmeta   output as simple HTML with meta information
  -tsv        output as TSV with bounding box meta information
  -bbox       output bounding box for each word (HTML format)
  -q          quiet mode (suppress messages)
  -opw STR    owner password (for encrypted files)
  -upw STR    user password (for encrypted files)
Combine as needed, e.g. flags="-f 1 -l 5 -layout -nopgbrk"

Examples:
- {"input_file":"report.pdf"} → saves to report.pdf.txt
- {"input_file":"report.pdf","output_file":"report.txt"} → saves to file
- {"input_file":"report.pdf","flags":"-f 1 -l 5"} → first 5 pages only
- {"input_file":"tables.pdf","flags":"-layout -nopgbrk"} → preserve table layout
- {"input_file":"invoice.pdf","output_file":"invoice.txt","flags":"-layout"}
- {"input_file":"encrypted.pdf","flags":"-upw mypassword"} → password-protected PDF
