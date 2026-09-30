---
name: csvkit
version: 0.2.0
description: Swiss-army knife for tabular data. Convert Excel/JSON/fixed-width to CSV, run SQL queries
  on CSV files, compute statistics, filter rows, select columns, sort, join, and format tabular data.
  Handles XLSX, XLS, CSV, TSV, JSON, NDJSON, and fixed-width formats.
metadata:
  magician:
    requires:
      bins:
      - in2csv
      - csvstat
      - csvsql
      - sql2csv
      - csvgrep
      - csvcut
      - csvsort
      - csvjoin
      - csvstack
      - csvlook
      - csvformat
      - csvjson
      - csvclean
    install_hint:
      docs: requires the csvkit command suite on PATH
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: csvjson
      input:
        args: ["--help"]
      expect:
        stdout_contains: "usage"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - in2csv
        - csvstat
        - csvsql
        - sql2csv
        - csvgrep
        - csvcut
        - csvsort
        - csvjoin
        - csvstack
        - csvlook
        - csvformat
        - csvjson
        - csvclean
        entrypoint: in2csv
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
        in2csv:
          description: Convert an existing local XLSX, XLS, JSON, NDJSON, DBF, fixed-width, or GeoJSON
            source to CSV, or list sheets in an Excel input.
          parameters:
            args:
              type: string_array
              description: Exact argv tokens for the reviewed in2csv executable. Do not include shell
                quotes, pipes, redirection, or command substitutions.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          executable: in2csv
        csvstat:
          description: Compute lightweight CSV column statistics including types, counts, minimums, maximums,
            means, uniqueness, and frequencies.
          parameters:
            args:
              type: string_array
              description: Exact argv tokens for the reviewed csvstat executable. Do not include shell
                quotes, pipes, redirection, or command substitutions.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          executable: csvstat
        csvsql:
          description: Run lightweight SQL queries over one or more local CSV files using csvkit's in-memory
            database.
          parameters:
            args:
              type: string_array
              description: Exact argv tokens for the reviewed csvsql executable. Do not include shell
                quotes, pipes, redirection, or command substitutions.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          executable: csvsql
        sql2csv:
          description: Execute exact argv tokens after `sql2csv` to run SQL against a SQLAlchemy database
            connection and emit CSV.
          parameters:
            args:
              type: string_array
              description: Exact argv tokens for the reviewed sql2csv executable. Do not include shell
                quotes, pipes, redirection, or command substitutions.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          executable: sql2csv
        csvgrep:
          description: 'Filter rows of a CSV by column-matched pattern. Typed params

            enforce required `columns` and `file`. Use `pattern` for literal

            match or `regex` for a Python regex; passing neither leaves

            csvgrep with nothing to filter on (it will error).

            '
          parameters:
            columns:
              type: string
              description: Comma-separated column names or 1-based indices to search. Maps to `-c`.
              required: true
              max_length: 4096
            pattern:
              type: string
              description: Literal substring to match in the selected columns. Maps to `-m`.
              max_length: 4096
            regex:
              type: string
              description: Python regex pattern (alternative to `pattern`). Maps to `-r`.
              max_length: 4096
            invert:
              type: boolean
              description: Emit non-matching rows instead. Bool flag — maps to `-i`.
            file:
              type: string
              description: Path to the input CSV file. Passed as the trailing positional argument.
              required: true
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: -c
            parameter: columns
          - type: flag
            flag: -m
            parameter: pattern
          - type: flag
            flag: -r
            parameter: regex
          - type: bool_flag
            flag: -i
            parameter: invert
          - type: passthrough
            parameter: extra_args
          - type: positional
            parameter: file
          executable: csvgrep
        csvcut:
          description: 'Project / reorder columns of a CSV. Typed params enforce `file`;

            `columns` is optional (omit it to list all columns by emitting

            none — use `list_columns: true` to invoke `-n` instead).

            '
          parameters:
            columns:
              type: string
              description: Comma-separated column names / 1-based indices to keep. Maps to `-c`.
              max_length: 4096
            exclude_columns:
              type: string
              description: Comma-separated columns to drop (everything else is kept). Maps to `-C`.
              max_length: 4096
            list_columns:
              type: boolean
              description: Print just the list of column names and exit. Bool flag — maps to `-n`.
            file:
              type: string
              description: Path to the input CSV file. Passed as the trailing positional argument.
              required: true
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: -c
            parameter: columns
          - type: flag
            flag: -C
            parameter: exclude_columns
          - type: bool_flag
            flag: -n
            parameter: list_columns
          - type: passthrough
            parameter: extra_args
          - type: positional
            parameter: file
          executable: csvcut
        csvsort:
          description: Sort rows in a local CSV file by selected columns.
          parameters:
            args:
              type: string_array
              description: Exact argv tokens for the reviewed csvsort executable. Do not include shell
                quotes, pipes, redirection, or command substitutions.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          executable: csvsort
        csvjoin:
          description: 'Inner / left / outer join two CSVs on shared columns. Typed

            params enforce required `columns`, `left_file`, `right_file`.

            Default join is inner; set one of `left`, `right`, `outer` to

            change semantics.

            '
          parameters:
            columns:
              type: string
              description: Comma-separated column names / indices to join on. Maps to `-c`.
              required: true
              max_length: 4096
            left:
              type: boolean
              description: Left outer join (keep all left rows). Bool flag — maps to `--left`.
            right:
              type: boolean
              description: Right outer join. Bool flag — maps to `--right`.
            outer:
              type: boolean
              description: Full outer join. Bool flag — maps to `--outer`.
            left_file:
              type: string
              description: Path to the left CSV (first positional after the flags).
              required: true
              max_length: 4096
            right_file:
              type: string
              description: Path to the right CSV (second positional).
              required: true
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: -c
            parameter: columns
          - type: bool_flag
            flag: --left
            parameter: left
          - type: bool_flag
            flag: --right
            parameter: right
          - type: bool_flag
            flag: --outer
            parameter: outer
          - type: passthrough
            parameter: extra_args
          - type: positional
            parameter: left_file
          - type: positional
            parameter: right_file
          executable: csvjoin
        csvstack:
          description: Stack compatible local CSV files into one CSV table.
          parameters:
            args:
              type: string_array
              description: Exact argv tokens for the reviewed csvstack executable. Do not include shell
                quotes, pipes, redirection, or command substitutions.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          executable: csvstack
        csvlook:
          description: Render a compact human-readable preview of a local CSV file.
          parameters:
            args:
              type: string_array
              description: Exact argv tokens for the reviewed csvlook executable. Do not include shell
                quotes, pipes, redirection, or command substitutions.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          executable: csvlook
        csvformat:
          description: Convert a local CSV file between delimiter, quoting, line-ending, and other CSV
            dialect formats.
          parameters:
            args:
              type: string_array
              description: Exact argv tokens for the reviewed csvformat executable. Do not include shell
                quotes, pipes, redirection, or command substitutions.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          executable: csvformat
        csvjson:
          description: Convert local CSV records into JSON output.
          parameters:
            args:
              type: string_array
              description: Exact argv tokens for the reviewed csvjson executable. Do not include shell
                quotes, pipes, redirection, or command substitutions.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          executable: csvjson
        csvclean:
          description: Diagnose and separate malformed rows from a local CSV file.
          parameters:
            args:
              type: string_array
              description: Exact argv tokens for the reviewed csvclean executable. Do not include shell
                quotes, pipes, redirection, or command substitutions.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          executable: csvclean
        help_in2csv:
          description: Inspect the installed in2csv command options.
          executable: in2csv
          fixed_args:
          - --help
        help_csvstat:
          description: Inspect the installed csvstat command options.
          executable: csvstat
          fixed_args:
          - --help
        help_csvsql:
          description: Inspect the installed csvsql command options.
          executable: csvsql
          fixed_args:
          - --help
        help_sql2csv:
          description: Inspect the installed sql2csv command options.
          executable: sql2csv
          fixed_args:
          - --help
        help_csvgrep:
          description: Inspect the installed csvgrep command options.
          executable: csvgrep
          fixed_args:
          - --help
        help_csvcut:
          description: Inspect the installed csvcut command options.
          executable: csvcut
          fixed_args:
          - --help
        help_csvsort:
          description: Inspect the installed csvsort command options.
          executable: csvsort
          fixed_args:
          - --help
        help_csvjoin:
          description: Inspect the installed csvjoin command options.
          executable: csvjoin
          fixed_args:
          - --help
        help_csvstack:
          description: Inspect the installed csvstack command options.
          executable: csvstack
          fixed_args:
          - --help
        help_csvlook:
          description: Inspect the installed csvlook command options.
          executable: csvlook
          fixed_args:
          - --help
        help_csvformat:
          description: Inspect the installed csvformat command options.
          executable: csvformat
          fixed_args:
          - --help
        help_csvjson:
          description: Inspect the installed csvjson command options.
          executable: csvjson
          fixed_args:
          - --help
        help_csvclean:
          description: Inspect the installed csvclean command options.
          executable: csvclean
          fixed_args:
          - --help
    runtime_catalog:
      categories:
      - data
      - csv
      - excel-input
      - spreadsheet-input
      - tabular
      - conversion
      - light-tabular-transform
      - lightweight-analysis
      - sql
      composition_category: data_operations
      expose_timeout_control: true
      timeout_default_secs: 60
---

# Csvkit

Tool name: `csvkit`
Inner-loop actions: `in2csv`, `csvstat`, `csvsql`, `csvgrep`, `csvcut`,
`sql2csv`, `csvsort`, `csvjoin`, `csvstack`, `csvlook`, `csvformat`, `csvjson`,
`csvclean`, `raw`, and `help`.
Requires: csvkit (pip install csvkit)

Use csvkit for file-format conversion, quick CSV inspection, small-to-medium
row filtering, column selection, simple joins/stacks, and CSV-shaped outputs.
It is a sharp command-line utility, not a full analyst planner. The caller
decides whether csvkit is the right tool; this skill focuses on csvkit command
choices and safe argv shapes.

## Discovering options

If you are unsure about the available flags for a subcommand, run the help
action first. This prints the full --help output for that subcommand.
- help {"args":["csvgrep","--help"]} → shows all csvgrep options
- help {"args":["csvsql","--help"]} → shows all csvsql options
Use this before constructing complex argv.

## Command Selection

- Use `in2csv` when the input is Excel, JSON, NDJSON, DBF, fixed-width, or
  GeoJSON and a CSV representation is needed.
- Use `csvstat -n`, `csvstat --count`, and `csvstat --json` for cheap
  inspection before expensive transforms.
- Use `csvcut` for column selection and column-name discovery.
- Use `csvgrep` for simple row filtering by exact match or regex.
- Use `csvsort`, `csvjoin`, and `csvstack` for simple local transforms.
- Use `csvsql` for SQL over one or more CSV files.
- Use `sql2csv` when the input is an external SQLAlchemy database connection
  and the desired output is CSV.
- Use `csvjson` and `csvformat` for output conversion.
- Use `csvclean` when CSV parsing fails or row lengths look inconsistent.
- Use `raw` only for a csvkit executable not modeled above.

## Operations

### in2csv - Convert to CSV
Convert XLSX, XLS, JSON, NDJSON, fixed-width, or DBF files to CSV.
- List sheets: in2csv {"args":["-n","data.xlsx"]}
- Convert Excel: in2csv {"args":["data.xlsx"]}
- Specific sheet: in2csv {"args":["--sheet","Sheet2","data.xlsx"]}
- JSON to CSV: in2csv {"args":["-f","json","data.json"]}

### csvstat - Summary Statistics
Print column types, min/max, mean, unique counts, frequency for each column.
- Full stats: csvstat {"args":["data.csv"]}
- JSON output: csvstat {"args":["--json","data.csv"]}
- Specific cols: csvstat {"args":["-c","name,age","data.csv"]}
- Count only: csvstat {"args":["--count","data.csv"]}
- Column names: csvstat {"args":["-n","data.csv"]}

### csvsql - Run SQL on CSV
Execute SQL queries against CSV files (uses in-memory SQLite).
- Query: csvsql {"args":["--query","SELECT product, SUM(revenue) FROM sales GROUP BY product","sales.csv"]}
- Multi-file: csvsql {"args":["--query","SELECT * FROM sales WHERE revenue > 1000","sales.csv"]}
The table name in SQL matches the filename without extension, or "stdin" for piped input.

Use `--tables name1,name2` when filenames are awkward or when joining multiple
files. Use `--snifflimit 0` when dialect sniffing misdetects a file.

### sql2csv - SQLAlchemy database to CSV
Execute SQL against a SQLAlchemy database connection and emit CSV.
- Query string: sql2csv {"args":["--db","sqlite:///data.db","--query","SELECT * FROM orders LIMIT 20"]}
- Query file: sql2csv {"args":["--db","postgresql:///warehouse","query.sql"]}

### csvgrep - Filter Rows
Filter rows by exact match, regex, or inverse match on a column.
- Exact match: csvgrep {"args":["-c","status","-m","active","data.csv"]}
- Regex: csvgrep {"args":["-c","email","-r","@gmail\\.com$","data.csv"]}
- Inverse: csvgrep {"args":["-i","-c","status","-m","deleted","data.csv"]}

### csvcut - Select Columns
Pick specific columns by name or index.
- By name: csvcut {"args":["-c","name,email,phone","data.csv"]}
- By index: csvcut {"args":["-c","1,3,5","data.csv"]}
- List columns: csvcut {"args":["-n","data.csv"]}

### csvsort - Sort Rows
Sort by one or more columns.
- Ascending: csvsort {"args":["-c","revenue","data.csv"]}
- Descending: csvsort {"args":["-r","-c","revenue","data.csv"]}

### csvjoin - Join Files
SQL-style join between two CSV files.
- Inner join: csvjoin {"args":["-c","customer_id","orders.csv","customers.csv"]}
- Left join: csvjoin {"args":["--left","-c","customer_id","orders.csv","customers.csv"]}

### csvstack - Stack Files Vertically
Concatenate CSV files with the same columns (like UNION ALL).
- Stack: csvstack {"args":["jan.csv","feb.csv"]}
- With label: csvstack {"args":["-g","jan,feb","-n","month","jan.csv","feb.csv"]}

### csvlook - Pretty Print
Display CSV as a readable markdown-style table.
- View: csvlook {"args":["data.csv"]}
- Max cols: csvlook {"args":["--max-columns","5","data.csv"]}

### csvformat - Change Delimiters
Convert between CSV, TSV, pipe-delimited, etc.
- To TSV: csvformat {"args":["-T","data.csv"]}
- Custom delim: csvformat {"args":["-D","|","data.csv"]}

### csvjson - CSV to JSON
Convert CSV to JSON or GeoJSON.
- To JSON: csvjson {"args":["-i","2","data.csv"]}
- Keyed: csvjson {"args":["-k","id","-i","2","data.csv"]}

### csvclean - Validate/Fix CSV
Find and fix common CSV errors (length mismatches, encoding).
- Validate: csvclean {"args":["-a","messy.csv"]}

## Common patterns
- Excel analysis: in2csv → csvstat (convert then summarize)
- Excel querying: in2csv → csvsql (convert then SQL)
- Filter + select: csvgrep → csvcut (filter rows then pick columns)
- CSV to JSON: csvcut/csvgrep/csvsort → csvjson
- Database query to CSV: sql2csv → csvstat/csvlook

## CLI behavior from local help
- `in2csv -n file.xlsx` lists Excel sheet names; `--sheet <name>` selects
  one sheet; `-f json|ndjson|xls|xlsx|fixed|dbf|geojson` overrides format
  inference when extension detection is wrong.
- `csvstat -n file.csv` lists columns; `csvstat --count file.csv` is a cheap
  row count; `csvstat --json file.csv` gives structured statistics.
- `csvsql --query <SQL> file.csv` executes SQL and outputs the last query as
  CSV. Table names default to filenames without extensions, or use
  `--tables name1,name2` to control them.
- `sql2csv --db <SQLAlchemy-connection> --query <SQL>` emits CSV from a
  database query; it also accepts a query file as the positional argument.
- Use `--snifflimit 0` when dialect sniffing causes surprises, `-I` to
  disable type inference, `-H` for files without headers, `-K N` to skip
  preamble lines, and `-e <encoding>` when decoding fails.
- There is no shell pipeline inside `args`. Save intermediate outputs through
  files when chaining commands.

## Inner-loop operating notes
- Prefer command-level actions (`in2csv`, `csvstat`, `csvcut`, `csvsql`, ...)
  over `raw`; use `raw` only for a csvkit executable not listed here.
- Start with cheap inspection (`in2csv -n`, `csvstat -n`, `csvstat --count`,
  or a small `csvcut`) before broad transforms.
- For large or important transforms, return concise stdout plus the command
  needed to reproduce the output. If the caller needs a durable file, it should
  run the command through a file-producing workflow rather than relying only on
  stdout.
- Avoid guessing delimiters, encodings, headers, or sheet names. Inspect first
  with `in2csv -n`, `csvcut -n`, `csvstat -n`, `csvstat --count`, or `csvclean`.

## Notes
- For Excel files, always use in2csv first (other operations expect CSV input).
- csvsql table name = filename without extension. For piped input, table = "stdin".
- csvstat can be slow on large files; use --count or -n for quick checks first.
- in2csv -n lists sheet names in an Excel file (essential before converting).
