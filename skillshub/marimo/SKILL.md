---
name: marimo
version: 0.2.0
description: Reactive Python notebook engine — write, execute, and export data analysis notebooks with
  pandas, plotly, DuckDB, and the full Python data science stack
metadata:
  magician:
    requires:
      bins:
      - marimo
    install_hint:
      docs: 'requires binary on PATH: marimo'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: help
      input:
        args: ["--help"]
      expect:
        stdout_contains: "marimo"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - marimo
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
          timeout_secs: 300
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
        check:
          description: Run `marimo check` to validate and optionally format marimo files.
          fixed_args:
          - check
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        config:
          description: Run `marimo config` for marimo configuration commands.
          fixed_args:
          - config
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
        convert:
          description: Run `marimo convert` to convert a notebook, markdown file, or script.
          fixed_args:
          - convert
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        edit:
          description: Run `marimo edit` to start or edit a notebook. Long-running server command; use
            only when explicitly needed.
          fixed_args:
          - edit
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 300
        env:
          description: Run `marimo env` to print environment information.
          fixed_args:
          - env
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
        export_html:
          description: Run `marimo export html` to execute a notebook and export it as an HTML file.
          fixed_args:
          - export
          - html
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        export_html_wasm:
          description: Run `marimo export html-wasm` for a WASM-powered HTML export.
          fixed_args:
          - export
          - html-wasm
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        export_ipynb:
          description: Run `marimo export ipynb` to export a marimo notebook as a Jupyter notebook.
          fixed_args:
          - export
          - ipynb
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        export_md:
          description: Run `marimo export md` to export a notebook as fenced markdown.
          fixed_args:
          - export
          - md
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        export_pdf:
          description: Run `marimo export pdf` to export a notebook as PDF.
          fixed_args:
          - export
          - pdf
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 180
        export_script:
          description: Run `marimo export script` to export a notebook as a flat Python script.
          fixed_args:
          - export
          - script
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        export_session:
          description: Run `marimo export session` to execute a notebook or directory of notebooks.
          fixed_args:
          - export
          - session
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 180
        export_thumbnail:
          description: Run `marimo export thumbnail` to generate OpenGraph thumbnails.
          fixed_args:
          - export
          - thumbnail
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        new:
          description: Run `marimo new` to create an empty or generated notebook.
          fixed_args:
          - new
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        recover:
          description: Run `marimo recover` to recover a notebook from JSON.
          fixed_args:
          - recover
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        run:
          description: Run `marimo run` to serve a notebook as a read-only app. Long-running server command;
            use only when explicitly needed.
          fixed_args:
          - run
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 300
        shell_completion:
          description: Run `marimo shell-completion` to install or print shell completions.
          fixed_args:
          - shell-completion
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
        tutorial:
          description: Run `marimo tutorial` for tutorial commands.
          fixed_args:
          - tutorial
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        raw:
          description: 'Escape hatch: run exact argv tokens after `marimo` for any marimo CLI command
            not listed above.'
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
              required: true
              min_items: 1
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
        help:
          description: 'Run exact marimo help argv. Examples: ["--help"], ["export","html","--help"].'
          parameters:
            args:
              type: string_array
              description: Exact argv tokens after this fixed marimo command prefix. Do not include shell
                quotes or the marimo binary name.
              max_items: 16
              max_item_bytes: 4096
          mappings:
          - type: passthrough
            parameter: args
    runtime_catalog:
      categories:
      - data_analysis
      - notebooks
      - visualization
      composition_category: data_operations
      expose_timeout_control: true
      timeout_default_secs: 120
---

# Marimo

Tool name: `marimo`
Inner actions expose marimo commands directly: `check`, `config`, `convert`,
`edit`, `env`, `export_html`, `export_html_wasm`, `export_ipynb`,
`export_md`, `export_pdf`, `export_script`, `export_session`,
`export_thumbnail`, `new`, `recover`, `run`, `shell_completion`,
`tutorial`, `raw`, and `help`.
Requires: marimo installed via `uv tool install marimo` (see setup-capability-tools)

Marimo is a reactive Python notebook engine. The agent writes a .py notebook
file using the `files` tool, then executes it with this tool.

Use marimo when the work needs Python state, multiple dependent steps,
visualizations, a reusable notebook, or a durable HTML/PDF/markdown report.
For one-off SQL, aggregation, or file inspection, the caller should usually use
a simpler data tool. This skill focuses on marimo notebook execution and export
behavior.

## Workflow

1. Write a notebook file using the caller's file-writing capability.
   Prefer an artifact path such as:
   `magician_data_v3/artifacts/notebooks/<name>.py`

2. Execute with the `export_html` action:
   export_html {"args":["magician_data_v3/artifacts/notebooks/analysis.py","-o","magician_data_v3/artifacts/notebooks/analysis.py.html"]}

   This executes all cells, captures printed output (stdout), and also saves
   an HTML report alongside the notebook at <notebook_path>.html.

3. Read stdout for concise results and verify the exported file exists or that
   the command reports success. If the user needs a visual deliverable, return
   the HTML/PDF/markdown path and a short summary of what it contains.

## When To Use Which Action

- `check`: validate notebook syntax/structure before export; use
  `["notebook.py","--format","json"]` for machine-readable diagnostics.
- `export_html`: execute and produce a self-contained HTML report. This is the
  default automation path.
- `export_md`: execute and produce markdown when the deliverable is mostly
  narrative/tables.
- `export_pdf`: produce PDF when explicitly requested or when a fixed-layout
  report is needed.
- `export_ipynb`: convert to Jupyter when interoperability is required.
- `export_script`: flatten a notebook to Python when the user needs code.
- `run`/`edit`: start servers only when a human explicitly wants an
  interactive app/session.
- `convert`: import from Markdown or Jupyter into marimo.
- `env`: inspect environment if imports fail unexpectedly.

## Notebook File Format

Marimo notebooks are plain Python files with a specific structure:

  import marimo
  app = marimo.App()

  @app.cell
  def load_data():
      import duckdb
      import pandas as pd
      conn = duckdb.connect()
      df = conn.execute("SELECT * FROM 'sales.csv'").df()
      print(f"Loaded {len(df)} rows")
      return df

  @app.cell
  def analyze(df):
      summary = df.groupby('region').agg({'amount': ['sum', 'mean', 'count']})
      print(summary.to_markdown())
      return summary

  @app.cell
  def visualize(df):
      import plotly.express as px
      fig = px.bar(df.groupby('region')['amount'].sum().reset_index(),
                   x='region', y='amount', title='Sales by Region')
      fig.write_html('magician_data_v3/artifacts/reports/sales_chart.html')
      print("Chart saved to artifacts/reports/sales_chart.html")

  if __name__ == "__main__":
      app.run()

## Pre-installed Libraries

The following libraries are available in the marimo environment:

**DataFrames:** pandas, polars
**Visualization:** plotly, matplotlib, seaborn
**Numerics:** numpy, scipy
**SQL:** duckdb (in-notebook, connects to any .duckdb or .csv file)
**Excel:** openpyxl (read), xlsxwriter (write)
**Web scraping:** requests, beautifulsoup4, lxml
**Marimo built-ins:** mo.ui (sliders, dropdowns, tables), mo.md (markdown)

## Key Rules

- Always use `print()` for output the agent needs to see (stdout is captured during execution)
- Save visualizations and derived data files under an artifacts path
- Use `if __name__ == "__main__": app.run()` at the end of the file
- Cells must return values to share data between them
- Each cell is a function decorated with @app.cell
- Cell dependencies are declared as function parameters (e.g., def analyze(df) depends on df)
- Keep printed output concise: row counts, validation checks, final tables, and
  saved artifact paths. Do not print entire large dataframes.
- Include validation cells for row counts, null checks, date ranges, or sample
  comparisons when the notebook supports an analytical conclusion.
- Use deterministic file paths for generated outputs so later steps can read
  them.

## CLI behavior from local help

- `export_html {"args":["notebook.py","-o","report.html"]}` executes the
  notebook and writes a self-contained HTML report. Use `-f` when overwriting
  an existing output file intentionally.
- `export_html` can pass notebook CLI args after `--`, e.g.
  `["analysis.py","-o","analysis.html","--","--region","west"]`.
- `check {"args":["notebook.py","--format","json"]}` validates a marimo file;
  use `--fix` only when formatting changes are intended.
- `run` and `edit` start long-running servers. Use them only when the user
  wants an interactive app/session; for automation, prefer export actions.
- `run` supports `--headless`, `--host`, `--port`, `--session-ttl`, and
  `--no-token`; keep ports explicit when a human will open the app.
- `convert`, `export_ipynb`, `export_md`, and `export_script` are useful
  when the requested deliverable is a notebook, markdown, or flat Python
  artifact instead of an HTML report.

## Inner-loop operating notes

- Prefer command-level actions over `raw`; use `raw` only for a marimo command
  not modeled in this pack yet.
- For analysis tasks, ensure the notebook prints concise machine-readable
  results and writes durable charts/reports under an artifacts path.
- After export, verify the output path exists or rely on the command result
  if it explicitly reports success.
- When export fails, run `check` first, then inspect the traceback. Do not
  repeatedly export the same broken notebook without changing it.

## LLM-call lakehouse (Parquet via DuckDB)

Every LLM call made by the runtime is captured as a flat row and persisted to
date-partitioned Parquet under
`<scope_root>/analytics/llm_calls/dt=YYYY-MM-DD/*.parquet`. Use this for cost,
latency, cache-hit, retry, model-mix, agent breakdowns, and chat-vs-autonomous
analysis. Read via DuckDB's `read_parquet`, then hand the resulting frame to
pandas / plotly for the visual deliverable.

Example notebook cell:

  @app.cell
  def load_llm_calls():
      import duckdb
      conn = duckdb.connect()
      df = conn.execute("""
          SELECT *
          FROM read_parquet(
              'magician_data_v3/scopes/<principal>/<workspace>/analytics/llm_calls/dt=*/*.parquet',
              hive_partitioning = true
          )
          WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)
      """).df()
      print(f"Loaded {len(df)} LLM calls over the last 7 days")
      return df

  @app.cell
  def spend_by_agent(df):
      import plotly.express as px
      spend = (df.dropna(subset=['agent_id'])
                 .groupby('agent_id', as_index=False)['cost_usd']
                 .sum()
                 .sort_values('cost_usd', ascending=False)
                 .head(10))
      fig = px.bar(spend, x='agent_id', y='cost_usd', title='Top 10 spenders (7d)')
      fig.write_html('magician_data_v3/artifacts/reports/llm_spend.html')
      print(spend.to_markdown(index=False))

Full 28-column schema and the equivalent JSON HTTP surface
(`POST /api/magician/v3/analytics/llm_calls/query` with allowlisted SELECT/WITH)
are documented in `docs/components/magician/duckdb-analytics.md`. Prefer this
lakehouse over the singleton `analytics.duckdb` when the question is about LLM
behavior — schemas are isolated and the cold store is the Parquet files
themselves.
