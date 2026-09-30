# DuckLocal CLI reference

## Invocation

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

`--format json` (default) is the contract below; `--format md` renders the same result as a Markdown table to read or quote: dates and timestamps in ISO form, DECIMALs with their digits, `NULL` as `NULL`, `|` escaped and newlines as `<br>` so a cell cannot break the table, `_0 rows._` after an empty result and `_Truncated at N rows:…_` when a limit cut it short. Use JSON whenever a program parses the value; the counts and digits are the same either way.

Use exactly one `--sql`/`--sql-file`. A file is UTF-8, `-` means stdin. Exactly one SQL statement is accepted using DuckDB's real parser; comment/quoted semicolons work and scripts are rejected before execution. Flags cannot repeat. Unknown flags, missing values, and invalid limits fail. Use separate option values, not `--flag=value`.

`--limit N` is positive, defaults to 1000, and caps output rows. A 2,000,000-cell budget also applies. An extra row determines `truncated`; this is not a computation or memory limit. Paths resolve against the working directory. Quote shell paths and escape SQL apostrophes by doubling them: `'O''Brien.csv'`. Quote SQL identifiers with double quotes.

Without `--database`, each call has a new in-memory database. `--database PATH` opens an existing file read-only. `--read-write` requires `--database` and authorizes database writes/creation at that path, not parent directory creation. Do not silently fall back to memory after open/lock errors. No GUI history/settings/registered views are read or changed. A GUI path named like a command (`query`, `schema`, `open`, `check`, `export`, and the hidden `profile`, `lsp`, `dash`) must be written with `./`, e.g. `./query`.

## Map what can be queried

```bash
ducklocal schema ./data/                         # files, folders, globs
ducklocal schema --database warehouse.duckdb     # tables and views
ducklocal schema warehouse.duckdb --table orders # one relation, whole
ducklocal schema ./logs/ --full                  # everything, however large
```

One JSON object: `{detail, relations, without_columns, hint?}`. Each relation has `name`, `kind` (`file`, `table`, `view` or `workbook`), `from` — the SQL that reads it, ready to follow `FROM` (a quoted path for a file, a quoted identifier where needed) — `rows`, `column_count` and, unless summarized, `columns: [{name, type}]`. `rows` is the catalog's estimate for a table, exact for Parquet, and `null` where it would need a scan (CSV/JSON, views). Up to 12 relations and 400 columns, `detail` is `"full"`; past that it is `"summary"`: the 5 relations with the most rows keep their columns, the rest list name, kind, rows and column count, and `hint` says to use `--table NAME` or `--full`. PATHs expand like a drop on the GUI (folders recursively, hidden files skipped, at most 256 files); a PATH that is a database file is the same as `--database`. A workbook is listed with its `sheets` and no columns — `--stats` reads its first sheet. A missing PATH or an unknown `--table` is exit 2.

## Column statistics

```bash
ducklocal schema sales.csv --stats
ducklocal schema --database warehouse.duckdb --table "my orders" --stats
```

`--stats` profiles one relation: the only one the arguments name, or the one `--table` picks (two or more without `--table` is exit 2). The relation is a data file (`csv`/`tsv`/`txt`/`parquet`/`json`/`ndjson`/`jsonl`), a workbook (`xlsx`/`xls`/`xlsb`/`ods`; its first sheet is imported as a TEMP table and profiled), or, with `--database`, a table/view name; a dotted name is `schema.table` and each part is quoted. It cannot be combined with `--full`. `ducklocal profile TARGET [--database PATH]` is the older, deprecated spelling of the same report.

One JSON object: `target`, `relation`, `row_count`, `elapsed_ms`, and `columns` in relation order. Per column `name`, `type`, `nulls`, `distinct`, `unique` (only when `distinct` equals `row_count`), `min`, `max` as text; numeric columns add `decimals` (the digits after the point actually used, not the declared scale), `median` (a value the column holds, never an interpolation) and `max_over_median`; DATE/TIMESTAMP columns add `covered_days`, `span_days` and `missing_days`. `LIST`/`STRUCT`/`MAP`/`UNION` columns report `nulls` only.

Read it before choosing how to present a column. `missing_days` above 0 means the series has holes, so a continuous line or an evenly spaced axis would misstate it. A large `max_over_median` means a linear scale rounds the small values to nothing. `decimals` is what a number should be formatted to; `DECIMAL(38,10)` holding two-decimal money reports 2.

Statistics are exact and scan the whole relation; `--limit` does not apply. It is a read, but a full one: on a very large file it costs a full pass.

## Export an analysis app as a static HTML file

```bash
ducklocal export --html apps/sales
ducklocal export --html --out report.html --database warehouse.duckdb apps/sales
ducklocal export --html --timeout 60 apps/sales
```

Runs an analysis app once (hidden window, no visible UI) and writes the statements its `query()` calls issued, with their results, into one self-contained HTML file: `--html` is required, `--out` defaults to `./<app folder>.html`, an existing file is refused without `--force`. `--database`/`--read-write` behave as for `query`. `--timeout SECONDS` is a positive integer, default 15: the capture stops when the app has gone quiet, or at the deadline, whichever comes first. One JSON object on stdout names the file and counts `queries`, `rows` and `app_errors`; its `stop_reason` is `"settled"` when the app went quiet in time and `"deadline"` when the time limit cut the capture short — then the report may be missing statements, and the HTML carries a visible warning saying so. The report holds the app's data — tables, and a bar chart where a result is a name and a number per row — never its layout or interactive state. Exit 1 with kind `app` means the app failed; when it failed after loading, the report is still written and the message names it.

## Check a dashboard spec

```bash
ducklocal check dashboard.dash
ducklocal check dashboard.dash --database warehouse.duckdb
```

Validates a `.dash` file — `query "name" { sql = <<SQL … SQL }` blocks and `plot "name" { type/query/x/y/series/title }` blocks joined by `query.name` references, plus `source` and `filter` blocks — without opening a window. A `$name` in a query's SQL must name a `filter` block and is checked (and, with `--database`, run) as `TRUE`, which is what the dashboard runs before anything is picked; with `--database` a filter's `column` must be one its plot's query returns. The output's `filters` list each filter's `plot`, `column` and the `queries` that read it. Static checks (syntax, duplicate names, dangling references, SQL through the real parser — one read-only statement per query) need no database; `--database` (existing file, read-only) additionally describes each query and checks plot columns against what it returns, including a numeric-type check on `y`. Success prints one JSON object with the spec's queries and plots. A spec mistake is exit 2, kind `spec`, one `file:line: message` per diagnostic, all diagnostics at once; database/I/O failures are exit 1.

## Show work in the running window

```bash
ducklocal open --title "Revenue by channel" --run --sql "SELECT channel, sum(amount) AS total FROM 'orders.csv' GROUP BY 1"
ducklocal open dashboard.dash
ducklocal open ./data/ --sql-file analysis.sql
echo "SELECT 42" | ducklocal open --sql-file - --no-launch
```

Sends one request to the DuckLocal window that is already running and brings it forward: PATHs open as a drop on the window would (a `.dash` or app folder as a tab, data files/folders/patterns attached), then `--sql`/`--sql-file` opens in a new query tab titled `--title`; `--run` runs it once the PATHs have attached. Relative PATHs resolve against the command's directory. With no window running it starts one (up to 30s) unless `--no-launch` is given. Success prints `{"delivered": true, "launched": BOOL}` — delivery, not the query's result: it runs in the window's session (its connection, attached views and history), and any error appears there, not on stdout. Exit 2 for arguments, 1 with kind `not_running` (no window reachable), `refused` (the endpoint rejected the request) or `launch`.

## Read what the window shows

```bash
ducklocal open --state
```

Asks the running window — never starts one, and opens nothing — what it shows, as one JSON object: `database` (a path, or `":memory:"`), `attached` (`[{path, name, rows}]` for the files the window registered as views), `active_tab`, and `tabs`. A query tab has `sql` (the editor's text, run or not) and `result`: `status` (`empty`, `running`, `rows`, `affected`, `failed`, `explain`), and for rows `columns`, `row_count`, `truncated`, `preview` (the first 20 rows as the grid shows them, strings) and the `sql` that produced them; for a failure `error`. A dashboard tab has `path`, `unsaved`, and `dashboard`: `plots` (`name`, `type`, `query`, `rows`, `failure`) and `filters` (`name`, `plot`, `column`, `picked`, `value` — `null` with `picked: true` means the NULL value was picked). An app tab has `directory`. `--state` takes no other option or PATH (exit 2). Exit 1 with kind `not_running` when no window is up, `timeout` if it does not answer within 5s. The names in `attached` are the window's views; a CLI query cannot read them — use their `path`.

## Discover, aggregate, convert

Confirm the actual schema before using `amount`:

```bash
ducklocal query --sql "DESCRIBE SELECT * FROM 'sales.csv'"
ducklocal query --sql "SELECT * FROM 'sales.csv'" --limit 20
ducklocal query --sql "SELECT sum(amount) AS total FROM 'sales.csv'"
ducklocal query --sql "DESCRIBE SELECT * FROM 'events.json'"
```

Only after authorizing the destination and checking for an existing file:

```bash
ducklocal query --sql "COPY (SELECT * FROM 'sales.csv') TO 'sales.parquet' (FORMAT PARQUET)"
ducklocal query --sql "SELECT count(*), sum(amount) FROM 'sales.parquet'"
```

Explicit authorized persistence across calls:

```bash
ducklocal query --database warehouse.duckdb --read-write --sql "CREATE TABLE sales AS SELECT * FROM 'sales.csv'"
ducklocal query --database warehouse.duckdb --sql "SELECT count(*) FROM sales"
```

CSV, JSON, and Parquet work with bundled support; no extra DuckDB CLI is required. Extensions are not automatically installed, though already installed extensions may autoload. Explicit INSTALL/LOAD requires authorization. Read-only database mode does not prevent filesystem/network side effects: COPY may write output even from a read-only database. Never automatically retry writes.

## JSONL and nested JSON

`read_ndjson_objects` is the reader for newline-delimited JSON (one object per line, `.jsonl`/`.ndjson`); `read_json`/`read_json_auto` read JSON documents (an array or a single object). Name the reader explicitly for a JSONL file rather than relying on auto-detection. `json_extract_string(col, '$.a.b[0].c')` walks a nested path — object keys and array indexes — and answers VARCHAR:

```bash
ducklocal query --sql "SELECT json_extract_string(event, '$.payload.items[0].sku') AS sku, count(*) AS n FROM read_ndjson_objects('events.jsonl') GROUP BY 1 ORDER BY n DESC"
```

Use `json_extract` instead when the leaf is not a string and you want it as JSON.

## Output

Success is one JSON object on stdout:

```json
{"columns":[{"name":"n","type":"Int32"}],"rows":[[1]],"row_count":1,"truncated":false,"elapsed_ms":1}
```

Columns/row arrays preserve order and duplicate names. `type` uses Arrow debug names, not SQL type names. `row_count` is the number returned, not the total matching/affected count. Zero-row results retain columns. DDL/DML are real result sets, often with a `Count` column; INSERT/COPY typically return `[[N]]`, CREATE TABLE often returns zero rows. `elapsed_ms` covers preparation, execution, and row encoding, not connection/input setup.

- NULL → `null`; text/enum → strings; booleans/finite floats → native JSON.
- Integers within ±9,007,199,254,740,991 → numbers; larger values → `{"encoding":"integer","value":"..."}` exact decimal text. Decimal → `{"encoding":"decimal","value":"123.450"}`.
- Blob/geometry → `{"encoding":"hex","value":"00ff"}` lowercase bytes (WKB for geometry).
- Date → `{"encoding":"date","unit":"Day","value":"1"}` days since 1970-01-01. Timestamp → `{"encoding":"timestamp","unit":"Nanosecond","value":"123456789"}` counts since Unix epoch. Time → `{"encoding":"time","unit":"Microsecond","value":"123456"}` since midnight. Units can be Second/Millisecond/Microsecond/Nanosecond as appropriate; counts are exact strings, including infinity sentinels. Timezone is in Arrow metadata.
- Interval → `{"encoding":"interval","months":1,"days":2,"nanos":"3000"}`.
- Nonfinite floats → `{"encoding":"float","value":"NaN"}`, `"inf"`, or `"-inf"`.
- List/array → recursively encoded arrays. Struct → `{"encoding":"struct","fields":[["name",VALUE],...]}`. Map → `{"encoding":"map","entries":[[KEY,VALUE],...]}`. Nested NULL remains null, never the string `"NULL"`.
- Union → `{"encoding":"union-value","value":VALUE}`; active value only, not the member tag, so not a lossless union round trip.
- Non-null nested HUGEINT/UHUGEINT/DECIMAL(38,0) fails because the pinned Arrow wrapper cannot reliably distinguish them. Explicitly CAST these fields to VARCHAR in SQL rather than risk precision/sign loss. Other unsupported types fail clearly too.

Temporal values are counts, not text. When a person reads the output rather than a program — a dashboard, a report — CAST the column to VARCHAR in SQL (`CAST(made_at AS VARCHAR)`) for the ISO form. (`--format md` already renders dates and timestamps readably.)

Check exit status before parsing. Codes: 0 success, 2 arguments/statement count, 1 SQL/database/I/O/output error. Errors are one object on stderr with empty stdout:

```json
{"error":{"kind":"sql","message":"...","hint":"..."}}
```

`hint` is present when there is a known next step — a misspelled flag's likely meaning, `--read-write` for a write refused on a read-only database, `ducklocal schema` for a table that does not exist, `TRY_CAST` for a value that will not convert — and absent otherwise.

Kinds: `argument`, `sql`, `database`, `io`, `output`, `app` for an app export whose app failed, `spec` for a `.dash` mistake, and `not_running`/`refused`/`timeout`/`launch` for `open`. Help/version are plain text. A broken stdout pipe cannot guarantee a complete/empty stream. Errors may follow already-performed side effects; inspect before retrying. Never report a failed query as empty data or use truncated previews as complete statistics.
