# AI CLI and official skill

**[中文](zh/cli.md)** · [Docs](index.md)

## Run without a window

`ducklocal query` executes SQL without initializing the GUI, history, registered files, language settings, or the GUI's global connection. Each invocation owns a new connection. With no command (or file, directory, and glob arguments), the existing GUI still opens. To open a GUI path named `query`, `profile`, `export`, `check` or `dash`, use `./query`, `./profile`, `./export`, `./check`, `./dash`.

`ducklocal export` is the one subcommand that starts the window platform, because an analysis app renders and rendering needs a window: it opens one hidden window, draws one frame, and exits when the app has stopped asking the database. Nothing appears on screen.

The supported release target remains **macOS 12+, Apple silicon**. There is no separate DuckDB CLI dependency. A build containing this CLI can be invoked directly inside its app bundle:

```bash
/Applications/DuckLocal.app/Contents/MacOS/ducklocal --help
/Applications/DuckLocal.app/Contents/MacOS/ducklocal --version
```

Alternatively, build this checkout with `cargo build --locked` and use `./target/debug/ducklocal`. The examples below assume that binary is available as `ducklocal` on your shell's PATH. Older releases may not have `query`; check help first. JSON and Parquet support are bundled, not downloaded at query time.

## Query options

```bash
ducklocal --help
ducklocal --version
ducklocal query --help
ducklocal query --sql "SELECT 1 AS n"
ducklocal query --sql "SELECT 1 AS n" --format md
ducklocal query --sql-file analysis.sql --limit 100
ducklocal query --sql-file - < analysis.sql
ducklocal query --database warehouse.duckdb --sql "SHOW TABLES"
```

- Exactly one of `--sql SQL` and `--sql-file FILE` is required. Files must be UTF-8; `-` reads stdin. SQL must contain exactly one statement. DuckDB's parser validates this before opening the target database or executing anything; comments, quoted semicolons, and trailing semicolons work. Scripts are rejected, not partially executed.
- `--database PATH` opens an **existing file, read-only by default**. Omit it for a fresh in-memory database. `--read-write` requires `--database` and explicitly permits writes and creating a database (not parent directories). Database errors never fall back to memory.
- `--limit N` is a positive integer, defaults to 1000, and limits returned rows. A 2,000,000-cell budget also applies; an extra row is read to determine `truncated`. Neither limit constrains query computation or DuckDB's result buffering. Large individual cells can still be large.
- `--format json` (the default) is the contract below. `--format md` renders the same result as a Markdown table, for a document or an agent reading it. Nothing else changes: the same SQL, the same limits, the same errors.
- Repeated/unknown flags, missing values, empty SQL, and invalid limits are errors. Options use separate values, not `--flag=value`. Relative paths resolve against the process working directory, not the SQL file directory. Quote shell paths; inside SQL, double apostrophes (`'O''Brien.csv'`). SQL identifiers use double quotes.

### Markdown output

`--format md` is a rendering, not a second contract: the values are the ones the JSON holds, written the way the results grid shows them.

| Value | Rendered as |
| --- | --- |
| `NULL` | `NULL` |
| integer, decimal, float | the digits the value carries, exactly — a `DECIMAL(38,10)` keeps its scale, `NaN`/`inf`/`-inf` are spelled out |
| `DATE` | `YYYY-MM-DD` |
| `TIMESTAMP` | `YYYY-MM-DD HH:MM:SS[.ffffff]`, with the fraction only when the value has one. No timezone is appended; `--format json` keeps the raw count |
| `TIME` | `HH:MM:SS[.ffffff]` |
| `INTERVAL` | `1 months 2 days 3000 ns` |
| `BLOB`, `GEOMETRY` | `0x00ff`, elided with `…` when long |
| `LIST`, `STRUCT`, `MAP` | `[1, 2]`, `{x: 1}`, elided with `…` past 120 characters |

A `|` in a cell is escaped and a newline becomes `<br>`, so a value cannot break the table it is in. A result with no rows is followed by `_0 rows._`, and a result that was cut short by `--limit` or the cell budget by `_Truncated at N rows: …_` — a preview should not read like a complete answer. Errors are unchanged: JSON on stderr, empty stdout.

**Read-only is not a filesystem or network sandbox.** SQL such as `COPY` can write files even when the database is read-only. Authorize output paths, overwrites, database mutations, extension installation, and external access before execution. DuckLocal does not automatically install extensions or retry operations. Explicit `INSTALL`/`LOAD` SQL remains possible with authorization; installed extensions may autoload. A failed command can already have performed side effects (for example before an output failure); inspect the destination before retrying.

## Explore and convert files

After confirming the actual file exists:

```bash
ducklocal query --sql "DESCRIBE SELECT * FROM 'sales.csv'"
ducklocal query --sql "SELECT * FROM 'sales.csv'" --limit 20
ducklocal query --sql "SELECT sum(amount) AS total FROM 'sales.csv'"
# Only after authorizing this output path:
ducklocal query --sql "COPY (SELECT * FROM 'sales.csv') TO 'sales.parquet' (FORMAT PARQUET)"
ducklocal query --sql "SELECT count(*), sum(amount) FROM 'sales.parquet'"
ducklocal query --sql "DESCRIBE SELECT * FROM 'events.json'"
```

Do not guess `amount` exists: inspect DESCRIBE first. Compute full aggregates in SQL rather than summing a truncated preview. CLI calls do not share the GUI's registered views or each other's in-memory tables. Persist explicitly when needed:

```bash
# These are separate statements and separate invocations, with explicit write permission.
ducklocal query --database warehouse.duckdb --read-write --sql "CREATE TABLE sales AS SELECT * FROM 'sales.csv'"
ducklocal query --database warehouse.duckdb --sql "SELECT count(*) FROM sales"
```

A database lock conflict is an error. Close the other writer (including the GUI), or use an authorized copy; never delete lock files or silently create a different database.

## JSONL and nested JSON

`read_ndjson_objects` is the reader for newline-delimited JSON (one object per line, `.jsonl`/`.ndjson`); `read_json`/`read_json_auto` read JSON documents (an array or a single object). Name the reader explicitly for a JSONL file rather than relying on auto-detection. `json_extract_string(col, '$.a.b[0].c')` walks a nested path — object keys and array indexes — and answers VARCHAR:

```bash
ducklocal query --sql "SELECT json_extract_string(event, '$.payload.items[0].sku') AS sku, count(*) AS n FROM read_ndjson_objects('events.jsonl') GROUP BY 1 ORDER BY n DESC"
```

Use `json_extract` instead when the leaf is not a string and you want it as JSON.

## Profile a relation

`ducklocal profile` answers the questions that decide a chart, a scale and a number format, before anything is built on them. `DESCRIBE` says a column is a `DOUBLE`; a profile says it uses two decimals, that its largest value is a thousand times its median, and that its dates skip eleven days in the middle.

```bash
ducklocal profile sales.csv
ducklocal profile orders.parquet
ducklocal profile "my orders" --database warehouse.duckdb
```

TARGET is a data file — `csv`, `tsv`, `txt`, `parquet`, `json`, `ndjson`, `jsonl` — or a workbook (`xlsx`, `xls`, `xlsb`, `ods`; its first sheet is imported as a TEMP table and profiled), or, with `--database`, a table or view name. A dotted name is read as `schema.table` and each part is quoted, so a name with a space or a capital letter works as written. A target that names nothing is an argument error (exit 2), not a SQL one.

One JSON object comes back: `target`, `relation` (the SQL the statistics ran against), `row_count`, `elapsed_ms`, and `columns` in the relation's own order. Per column:

| Field | Meaning |
| --- | --- |
| `name`, `type` | as `DESCRIBE` reports them |
| `nulls` | rows where the column is `NULL` |
| `distinct` | exact, not estimated |
| `unique` | present and `true` when `distinct` equals `row_count` |
| `min`, `max` | as text, so a `DECIMAL` keeps its digits |
| `decimals` | digits after the point the column **actually uses**, which is not its declared scale |
| `median` | a value the column holds, not an interpolation between two of them |
| `max_over_median` | how many times the middle value the largest one is — the number that decides a linear axis from a logarithmic one |
| `covered_days`, `span_days`, `missing_days` | days the column names, days between its ends, and the difference: `0` is a continuous series, anything else is holes a line would draw over |

`decimals`, `median` and `max_over_median` appear on numeric columns; the day fields on dates and timestamps. `LIST`, `STRUCT`, `MAP` and `UNION` columns report `nulls` only — `min` and `max` are not defined on them.

Statistics are exact and read the whole relation, so a profile costs a full scan. `--limit` does not apply: a profile of a sample is not a profile.

## Export an app as a static HTML file

`ducklocal export` answers "what was that app showing?" for someone who does not have DuckLocal. It runs the app once — the same runtime, the same host module, the same database rules as `query` — and writes the statements its `query()` calls issued, with their results, into one self-contained HTML file.

```bash
ducklocal export --html examples/analysis_app
ducklocal export --html --out report.html --database warehouse.duckdb apps/sales
```

- `--html` is required, and is the only format there is. APP is an app folder (one holding `main.js`) or its `main.js`, resolved the way the GUI resolves it; a path that names neither is an argument error (exit 2).
- `--out FILE` names the destination and defaults to `./<app folder>.html` in the working directory. An existing file is refused unless `--force` is given — checked before the app runs, so a refusal costs nothing.
- `--database PATH` and `--read-write` mean exactly what they mean for `query`: an existing file, read-only unless asked otherwise, in memory when omitted. The app runs on that connection, so an app that writes needs `--read-write`.
- `--timeout SECONDS` is a positive integer, default 15: the capture stops when the app has stopped asking the database for a moment, or at the deadline, whichever comes first.
- The command starts the window platform, because an app renders and rendering needs a window. The window is hidden and never appears.

One JSON object goes to stdout:

```json
{"html":"/abs/report.html","app":"/abs/app","queries":2,"rows":212,"app_errors":0,"captured_ms":630,"stop_reason":"settled"}
```

`queries` counts the captured statements, `rows` the rows across those that succeeded, and `app_errors` what the app logged as an error while it ran. `stop_reason` is `settled` when the app went quiet on its own, `deadline` when `--timeout` cut the capture short — then the report may be missing statements, and the HTML shows a visible warning saying so.

The report holds the app folder, the database it ran against, the export time, and one section per statement: the SQL, its columns and their Arrow types, the result as a table, and a bar chart when a result is a name and a number per row. A `catalog()` call becomes an appendix of tables, views and columns. Statements an app runs more than once appear once, with the last result.

The report does **not** hold the app's own layout or its charts. An app draws native components, and a chart's bars are decided while it is laid out, not described anywhere that can be read out — so the export shows the data the app was built from, not the interface it built. Filters, toggles and later refreshes are not represented either, and there is no JavaScript in the file. It is a snapshot of the app's loading state, and it says so at the top.

The app's JavaScript runs with the same privileges it always has: `query()` can `COPY`, `ATTACH` and write files. The export is not a sandbox.

Exit codes: **0** with the file written; **2** for a bad command line, an app path that names nothing, or a destination that already exists; **1** when the app could not be loaded (nothing is written) or when it loaded and then failed (the report is written anyway, with the error in it, and the message names the file). An app failure reports the error kind `app`.

## Check a dashboard spec

A `.dash` file declares a dashboard the way Terraform declares infrastructure — queries and plots as blocks, references between them — rather than scripting one as an analysis app. The two formats coexist: the spec covers query + standard plot and is easy for a person or an agent to diff; an app stays for bespoke layout and interaction.

```hcl
query "latency" {
  sql = <<SQL
    SELECT timestamp, service, avg(latency) AS latency
    FROM logs
    GROUP BY timestamp, service
  SQL
}

plot "latency" {
  type   = "line"
  query  = query.latency
  x      = timestamp
  y      = latency
  series = service
}
```

```bash
ducklocal check dashboard.dash
ducklocal check dashboard.dash --database warehouse.duckdb
```

A `query` block holds one `sql` attribute — one statement, as a heredoc or a string. A `plot` block holds `type` (one of `line`, `bar`, `area`, `scatter`, `table`), `query` (a reference like `query.latency` to a query block in the same file), `x` and `y` (result columns, as bare identifiers or quoted strings; `y` is optional for `table`), plus optional `series` and `title`. `#` and `//` comment to end of line. That is the whole language: no functions, no conditionals, no interpolation.

Without `--database` the check is fully static — no table needs to exist and nothing executes. Each query's SQL is validated by the real DuckDB parser on a throwaway connection, the way `query` validates before running. With `--database PATH` (existing file, read-only) every query is additionally described — planned, not run — and each plot's `x`/`y`/`series` is checked against the columns the query actually returns; a `y` that is not numeric is an error for every type but `table`.

Success is one JSON object listing the file's queries (with their result columns when a database was given) and plots. A spec mistake is exit 2 with error kind `spec` and one `file:line: message` per diagnostic — all of them, not just the first. A database or I/O failure is exit 1.

The same file opens as a dashboard tab in the GUI — `ducklocal dashboard.dash`, or drag it onto the window — with each plot in a resizable vertical stack and a plot that cannot draw showing the reason inline. See [the app guide](analysis-app.md#dashboard-specs-dash).

## Edit a dashboard spec with LSP

`ducklocal lsp` is a Language Server Protocol server for `.dash` files, over stdio, for editors to spawn. It gives any LSP-capable editor what the GUI's own spec editor has: the diagnostics `check` reports, published on every open and change (full-document sync, all of them severity Error — they are mistakes, not style); completion for block types, attribute names, plot types and query names, each with a text edit that replaces the word being typed; hover documentation for blocks, attributes and references; and go-to-definition from a `query.name` reference to the query block's name. With `--database PATH` each query's SQL is additionally validated by the real DuckDB parser, the way `check --database` does.

Point your editor's generic LSP support at `ducklocal lsp` for `*.dash` files — the command takes no document arguments; the editor speaks to it on stdio. In Neovim (0.10+):

```lua
vim.api.nvim_create_autocmd("FileType", {
  pattern = "dash",
  callback = function(event)
    vim.lsp.start({
      name = "ducklocal",
      cmd = { "ducklocal", "lsp" },
      root_dir = vim.fs.dirname(vim.api.nvim_buf_get_name(event.buf)),
    })
  end,
})
```

In VS Code, any generic-LSP extension does the same with command `ducklocal`, arguments `["lsp"]`, document selector `dash`. Exit codes: **0** after a clean `shutdown`/`exit`, **1** when the client goes away without one, **2** for a bad command line or a failed protocol handshake.

## JSON contract

Successful queries write exactly one JSON object to stdout, followed by a newline:

```json
{"columns":[{"name":"n","type":"Int32"}],"rows":[[1]],"row_count":1,"truncated":false,"elapsed_ms":1}
```

`columns` and each row preserve column order and duplicate column names. `type` is the **Arrow debug representation**, not the original DuckDB SQL type name (for example HUGEINT and DECIMAL can share an Arrow carrier). `row_count` counts returned rows, not total matches or affected rows. `elapsed_ms` measures preparation, execution, and row encoding, not file loading/connection setup. Zero-row results retain metadata. DDL/DML expose the actual DuckDB result, including `Count` columns: INSERT/COPY commonly return `[[N]]`; CREATE TABLE commonly returns no rows. They are not classified from the first SQL keyword.

Cell encodings:

| SQL value | JSON encoding |
| --- | --- |
| NULL | `null` (never the string `"NULL"`) |
| Boolean, finite float | JSON boolean/number |
| Integer within ±9,007,199,254,740,991 | JSON number |
| Larger integer, including HUGEINT/UHUGEINT | `{"encoding":"integer","value":"9007199254740992"}` |
| Decimal | `{"encoding":"decimal","value":"123.450"}` with exact decimal text |
| Text, enum | JSON string |
| Blob, geometry | `{"encoding":"hex","value":"00ff"}`; lowercase bytes, geometry uses WKB |
| Date | `{"encoding":"date","unit":"Day","value":"1"}`; days since 1970-01-01 |
| Timestamp | `{"encoding":"timestamp","unit":"Nanosecond","value":"123456789"}`; signed count since Unix epoch; unit may also be Second/Millisecond/Microsecond |
| Time | `{"encoding":"time","unit":"Microsecond","value":"123456"}`; count since midnight |
| Interval | `{"encoding":"interval","months":1,"days":2,"nanos":"3000"}` |
| NaN, positive/negative infinity | `{"encoding":"float","value":"NaN"}` / `"inf"` / `"-inf"` |
| List, array | JSON array, recursively encoded (including nested nulls) |
| Struct | `{"encoding":"struct","fields":[["name",VALUE],...]}` |
| Map | `{"encoding":"map","entries":[[KEY,VALUE],...]}`; not a JSON object, so keys retain their types |
| Union | `{"encoding":"union-value","value":VALUE}`; active value only, member tag is not retained |

Temporal counts avoid dropping fractional precision; date/timestamp infinity sentinels remain raw counts, not formatted dates. Arrow metadata carries any timezone. When a person reads the output rather than a program — a report, a dashboard — `CAST(made_at AS VARCHAR)` in SQL returns the ISO text instead of the count; `--format md` already renders dates and timestamps readably without a cast. Because the pinned DuckDB Arrow wrapper cannot distinguish nested HUGEINT, UHUGEINT, and DECIMAL(38,0), non-null occurrences produce a clear error instead of silently changing sign or precision. Explicitly cast such fields to VARCHAR in SQL. Unknown unsupported types also produce an error; do not mistake an error for an empty result. Union encoding is an active-value representation, not a lossless union round trip.

Errors write one JSON object to **stderr**, leaving stdout empty:

```json
{"error":{"kind":"argument","message":"Provide exactly one of --sql and --sql-file"}}
```

Exit codes: **0** success, **2** invalid arguments/statement count, **1** SQL, database, I/O, or output failure. Error kinds are `argument`, `sql`, `database`, `io`, `output`, `spec` for `check`, and — for `export` — `app`. Check exit status before parsing stdout and inspect `truncated` before reporting completeness. Help/version are plain text exceptions. Transport failures such as a closed stdout pipe cannot guarantee an empty/complete output stream.

## Install the official skill

The standard skill is [`skills/ducklocal`](https://github.com/JetSquirrel/ducklocal/tree/main/skills/ducklocal). From a checkout, copy it into the target project's skill directory (example for Claude Code):

```bash
# Run from the DuckLocal checkout; replace the destination with your project.
mkdir -p /path/to/your-project/.claude/skills
cp -R skills/ducklocal /path/to/your-project/.claude/skills/
```

Inspect an existing destination first rather than overwriting local changes. Other agents can use the same `SKILL.md` plus `references/cli.md` in their supported project-level skills directory. No global configuration changes or marketplace installation are needed. The skill teaches schema inspection, bounded previews, SQL aggregates, conversions, and database queries; it does not provide S3 browsing, spatial-specific commands, session memory, or an MCP server.
