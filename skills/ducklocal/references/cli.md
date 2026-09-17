# DuckLocal CLI reference

## Invocation

```bash
ducklocal --help
ducklocal --version
ducklocal query --help
ducklocal query --sql "SELECT 1 AS n"
ducklocal query --sql-file analysis.sql --limit 100
ducklocal query --sql-file - < analysis.sql
ducklocal query --database warehouse.duckdb --sql "SHOW TABLES"
```

Use exactly one `--sql`/`--sql-file`. A file is UTF-8, `-` means stdin. Exactly one SQL statement is accepted using DuckDB's real parser; comment/quoted semicolons work and scripts are rejected before execution. Flags cannot repeat. Unknown flags, missing values, and invalid limits fail. Use separate option values, not `--flag=value`.

`--limit N` is positive, defaults to 1000, and caps output rows. A 2,000,000-cell budget also applies. An extra row determines `truncated`; this is not a computation or memory limit. Paths resolve against the working directory. Quote shell paths and escape SQL apostrophes by doubling them: `'O''Brien.csv'`. Quote SQL identifiers with double quotes.

Without `--database`, each call has a new in-memory database. `--database PATH` opens an existing file read-only. `--read-write` requires `--database` and authorizes database writes/creation at that path, not parent directory creation. Do not silently fall back to memory after open/lock errors. No GUI history/settings/registered views are read or changed. A GUI path named `query` uses `./query`.

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

Check exit status before parsing. Codes: 0 success, 2 arguments/statement count, 1 SQL/database/I/O/output error. Errors are one object on stderr with empty stdout:

```json
{"error":{"kind":"sql","message":"..."}}
```

Kinds: `argument`, `sql`, `database`, `io`, `output`. Help/version are plain text. A broken stdout pipe cannot guarantee a complete/empty stream. Errors may follow already-performed side effects; inspect before retrying. Never report a failed query as empty data or use truncated previews as complete statistics.
