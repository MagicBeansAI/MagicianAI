# Marimo Capability

## Purpose

The `marimo` skill gives analysis agents a notebook-style Python execution
surface for multi-step analysis, visualization, and HTML report generation.

## Current Shape

The shipped pack is a governed batch CLI skill (`skillshub/marimo/SKILL.md`),
not an interpreted YAML composite and not a shell step chain. `runtime_contract`
declares `protocol: cli`, `interaction: batch`, `stdin: denied`, workspace
working directory, and no auth. Pack limits are 300 s timeout, 10 MiB stdout,
and 2 MiB stderr.

Typed `runtime_actions` pass inert argv after a fixed `marimo` prefix:
`check`, `config`, `convert`, `edit`, `env`, `export_html`, `export_html_wasm`,
`export_ipynb`, `export_md`, `export_pdf`, `export_script`, `export_session`,
`export_thumbnail`, `new`, `recover`, `run`, `shell_completion`, `tutorial`,
plus `raw` and `help`. Each action maps `args` as a passthrough string array
(max 16 tokens). `run` and `edit` start long-running servers; automation prefers
export.

Typical analysis path: the agent writes a notebook `.py` with the `files` tool,
`export_html` runs `marimo export html` (120 s action timeout), stdout is
captured, and an HTML artifact is written beside the notebook (or to `-o`).

## Environment

The local setup flow and the container tool-install script both provision
`marimo` plus the supporting data-science stack used by the pack, including:

- pandas and polars
- plotly, matplotlib, and seaborn
- numpy and scipy
- duckdb
- Excel and scraping helpers

## Intended Use

Use `marimo` when the task needs:

- multi-step Python analysis beyond straightforward SQL
- visualization or chart output
- notebook-style exploratory workflows
- self-contained HTML analysis artifacts

For simple SQL inspection and aggregation, the `duckdb` capability remains the
faster default.

## LLM-Call Lakehouse

Marimo notebooks can query the runtime's per-call LLM telemetry directly from
date-partitioned Parquet under
`<scope_root>/analytics/llm_calls/dt=YYYY-MM-DD/*.parquet`. Use DuckDB's
`read_parquet(... , hive_partitioning = true)` to load any time slice, then
hand the resulting frame to pandas / plotly for the visual deliverable. See
`docs/components/magician/duckdb-analytics.md` for the full schema and the
equivalent `POST /api/magician/v3/analytics/llm_calls/query` HTTP surface used
by dashboard `dataSource` bindings.

## Agent Integration

The data analyst agent includes `marimo` in its tool set and persona guidance.
This positions notebooks as a first-class analysis path rather than an ad hoc
shell escape hatch.
