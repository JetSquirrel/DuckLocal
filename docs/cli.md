# AI CLI and official skill

**[中文](zh/cli.md)** · [Docs](index.md)

## Run without a window

`ducklocal query` executes SQL without initializing the GUI, history, registered files, language settings, or the GUI's global connection. Each invocation owns a new connection. With no command (or file, directory, and glob arguments), the existing GUI still opens. To open a GUI path named `query`, use `./query`.

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
ducklocal query --sql-file analysis.sql --limit 100
ducklocal query --sql-file - < analysis.sql
ducklocal query --database warehouse.duckdb --sql "SHOW TABLES"
```

- Exactly one of `--sql SQL` and `--sql-file FILE` is required. Files must be UTF-8; `-` reads stdin. SQL must contain exactly one statement. DuckDB's parser validates this before opening the target database or executing anything; comments, quoted semicolons, and trailing semicolons work. Scripts are rejected, not partially executed.
- `--database PATH` opens an **existing file, read-only by default**. Omit it for a fresh in-memory database. `--read-write` requires `--database` and explicitly permits writes and creating a database (not parent directories). Database errors never fall back to memory.
- `--limit N` is a positive integer, defaults to 1000, and limits returned rows. A 2,000,000-cell budget also applies; an extra row is read to determine `truncated`. Neither limit constrains query computation or DuckDB's result buffering. Large individual cells can still be large.
- Repeated/unknown flags, missing values, empty SQL, and invalid limits are errors. Options use separate values, not `--flag=value`. Relative paths resolve against the process working directory, not the SQL file directory. Quote shell paths; inside SQL, double apostrophes (`'O''Brien.csv'`). SQL identifiers use double quotes.

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

## Profile a relation

`ducklocal profile` answers the questions that decide a chart, a scale and a number format, before anything is built on them. `DESCRIBE` says a column is a `DOUBLE`; a profile says it uses two decimals, that its largest value is a thousand times its median, and that its dates skip eleven days in the middle.

```bash
ducklocal profile sales.csv
ducklocal profile orders.parquet
ducklocal profile "my orders" --database warehouse.duckdb
```

TARGET is a data file — `csv`, `tsv`, `txt`, `parquet`, `json`, `ndjson`, `jsonl` — or, with `--database`, a table or view name. A dotted name is read as `schema.table` and each part is quoted, so a name with a space or a capital letter works as written. A target that names nothing is an argument error (exit 2), not a SQL one.

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

Temporal counts avoid dropping fractional precision; date/timestamp infinity sentinels remain raw counts, not formatted dates. Arrow metadata carries any timezone. Because the pinned DuckDB Arrow wrapper cannot distinguish nested HUGEINT, UHUGEINT, and DECIMAL(38,0), non-null occurrences produce a clear error instead of silently changing sign or precision. Explicitly cast such fields to VARCHAR in SQL. Unknown unsupported types also produce an error; do not mistake an error for an empty result. Union encoding is an active-value representation, not a lossless union round trip.

Errors write one JSON object to **stderr**, leaving stdout empty:

```json
{"error":{"kind":"argument","message":"Provide exactly one of --sql and --sql-file"}}
```

Exit codes: **0** success, **2** invalid arguments/statement count, **1** SQL, database, I/O, or output failure. Error kinds are `argument`, `sql`, `database`, `io`, and `output`. Check exit status before parsing stdout and inspect `truncated` before reporting completeness. Help/version are plain text exceptions. Transport failures such as a closed stdout pipe cannot guarantee an empty/complete output stream.

## Install the official skill

The standard skill is [`skills/ducklocal`](https://github.com/JetSquirrel/ducklocal/tree/main/skills/ducklocal). From a checkout, copy it into the target project's skill directory (example for Claude Code):

```bash
# Run from the DuckLocal checkout; replace the destination with your project.
mkdir -p /path/to/your-project/.claude/skills
cp -R skills/ducklocal /path/to/your-project/.claude/skills/
```

Inspect an existing destination first rather than overwriting local changes. Other agents can use the same `SKILL.md` plus `references/cli.md` in their supported project-level skills directory. No global configuration changes or marketplace installation are needed. The skill teaches schema inspection, bounded previews, SQL aggregates, conversions, and database queries; it does not provide S3 browsing, spatial-specific commands, session memory, or an MCP server.
