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

Without `--database`, each call has a new in-memory database. `--database PATH` opens an existing file read-only. `--read-write` requires `--database` and authorizes database writes/creation at that path, not parent directory creation. Do not silently fall back to memory after open/lock errors. No GUI history/settings/registered views are read or changed. A GUI path named `query` uses `./query`.

## Profile a relation

```bash
ducklocal profile sales.csv
ducklocal profile "my orders" --database warehouse.duckdb
```

TARGET is a data file or, with `--database`, a table/view name; a dotted name is `schema.table` and each part is quoted. It takes no other options. A target that names nothing is an argument error (exit 2).

One JSON object: `target`, `relation`, `row_count`, `elapsed_ms`, and `columns` in relation order. Per column `name`, `type`, `nulls`, `distinct`, `unique` (only when `distinct` equals `row_count`), `min`, `max` as text; numeric columns add `decimals` (the digits after the point actually used, not the declared scale), `median` (a value the column holds, never an interpolation) and `max_over_median`; DATE/TIMESTAMP columns add `covered_days`, `span_days` and `missing_days`. `LIST`/`STRUCT`/`MAP`/`UNION` columns report `nulls` only.

Read it before choosing how to present a column. `missing_days` above 0 means the series has holes, so a continuous line or an evenly spaced axis would misstate it. A large `max_over_median` means a linear scale rounds the small values to nothing. `decimals` is what a number should be formatted to; `DECIMAL(38,10)` holding two-decimal money reports 2.

Statistics are exact and scan the whole relation; `--limit` does not apply. It is a read, but a full one: on a very large file it costs a full pass.

## Export a dashboard as a static HTML file

```bash
ducklocal dash export --html panels/sales
ducklocal dash export --html --out report.html --database warehouse.duckdb panels/sales
ducklocal dash export --html --timeout 60 panels/sales
```

Runs an analysis panel once (hidden window, no visible UI) and writes the statements its `query()` calls issued, with their results, into one self-contained HTML file: `--html` is required, `--out` defaults to `./<panel folder>.html`, an existing file is refused without `--force`. `--database`/`--read-write` behave as for `query`. `--timeout SECONDS` is a positive integer, default 15: the capture stops when the panel has gone quiet, or at the deadline, whichever comes first. One JSON object on stdout names the file and counts `queries`, `rows` and `panel_errors`; its `stop_reason` is `"settled"` when the panel went quiet in time and `"deadline"` when the time limit cut the capture short — then the report may be missing statements, and the HTML carries a visible warning saying so. The report holds the panel's data — tables, and a bar chart where a result is a name and a number per row — never its layout or interactive state. Exit 1 with kind `panel` means the panel failed; when it failed after loading, the report is still written and the message names it.

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
{"error":{"kind":"sql","message":"..."}}
```

Kinds: `argument`, `sql`, `database`, `io`, `output`, and `panel` for a dashboard export whose panel failed. Help/version are plain text. A broken stdout pipe cannot guarantee a complete/empty stream. Errors may follow already-performed side effects; inspect before retrying. Never report a failed query as empty data or use truncated previews as complete statistics.
