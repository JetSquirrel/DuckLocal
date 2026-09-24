# DuckLocal

<img src="assets/logo.png" width="128" alt="DuckLocal logo">

**[ducklocal.app](https://ducklocal.app/)** · **[Docs](https://docs.ducklocal.app/)** · **[中文文档](README.zh-CN.md)**

A local-first workspace for querying and exploring your data, built natively on DuckDB. Point it at your files — there is no connection to configure, no schema to create, and nothing is uploaded anywhere.

![How DuckLocal works](assets/intro.png)

## Open your data

```bash
ducklocal ./logs/             # every data file under a folder, recursively
ducklocal ./billing.parquet   # one file
ducklocal './data/*.csv'      # a pattern
ducklocal warehouse.duckdb    # or an existing DuckDB database
```

Each CSV, TSV, Parquet, JSON, or Excel file becomes queryable as the window opens. You can also drag files or folders onto the window, or pick them from the file dialog — and they stay registered, so the next launch starts with the same workspace. One open request attaches at most 256 files.

Your data stays on your machine: nothing is uploaded, and there is no account.

## Features

- Open local CSV / TSV / Parquet / JSON / Excel files, or whole folders — from the command line, the picker, or a drop on the window
- SQL editor with syntax highlighting, autocompletion and one-click formatting, across multiple query tabs
- Run queries with ⌘↵ (Cmd+Enter); inspect plans with EXPLAIN
- Schema sidebar: browse databases / schemas / tables / columns, generate SELECT queries in one click, alter a column's data type from a dialog
- Results grid with filtering, cell copy, CSV/Parquet export and built-in charts
- Query history with one-click refill into the editor
- Optional S3 support via httpfs; credentials are session-only
- Light and dark themes, four interface sizes (⌘+ / ⌘− / ⌘0), English and 简体中文

## AI CLI and official skill

Run one SQL statement without opening a window or reading GUI history/settings:

```bash
ducklocal --help
ducklocal query --sql "DESCRIBE SELECT * FROM 'sales.csv'"
ducklocal query --sql "SELECT * FROM 'sales.csv'" --limit 20
ducklocal query --database warehouse.duckdb --sql "SHOW TABLES"
```

Results are structured JSON, with explicit truncation and precision-preserving value encodings; `--format md` prints the same values as a Markdown table for a document to quote. File databases default to read-only; writes require `--read-write`. Read-only is not a filesystem/network sandbox: `COPY` can still write files.

An analysis app can be exported as one standalone HTML file — the statements its `query()` calls issued, with their results — for sharing with someone who does not have DuckLocal:

```bash
ducklocal export --html examples/analysis_app
```

A dashboard can also be declared as data: a `.dash` file of query and plot blocks, opened as a tab like an app, editable right there in the GUI. `ducklocal check` validates a spec without a window, and `ducklocal lsp` gives any LSP-capable editor the same diagnostics, completion, hover and go-to-definition:

```bash
ducklocal dashboard.dash                  # open as a dashboard tab
ducklocal check dashboard.dash            # validate; JSON diagnostics, exit 2 on mistakes
ducklocal lsp                             # language server over stdio, for editors
```

See the [CLI guide](https://docs.ducklocal.app/cli) for conversion, stdin, output, app export, and safety details.

The [official agent skill](skills/ducklocal/SKILL.md) teaches schema-first exploration, SQL analysis, and verified file conversion. Copy it into your target project's supported skills directory; for example, from this checkout:

```bash
mkdir -p /path/to/your-project/.claude/skills
cp -R skills/ducklocal /path/to/your-project/.claude/skills/
```

Inspect existing destinations before overwriting. No global configuration is changed. Current releases target macOS 12+ on Apple silicon; the app binary is `/Applications/DuckLocal.app/Contents/MacOS/ducklocal`. Check its help/version for CLI support, or build this checkout with `cargo build --locked` and use `./target/debug/ducklocal`.

## Documentation

The guides are at **[docs.ducklocal.app](https://docs.ducklocal.app/)**: [getting started](https://docs.ducklocal.app/getting-started), a [10-minute tutorial](https://docs.ducklocal.app/tutorial) with sample data, [troubleshooting](https://docs.ducklocal.app/troubleshooting), [data sources](https://docs.ducklocal.app/data-sources), [S3 and httpfs](https://docs.ducklocal.app/s3), [SQL editor](https://docs.ducklocal.app/sql-editor), [schema browser and history](https://docs.ducklocal.app/schema-and-history), [results and charts](https://docs.ducklocal.app/results-and-charts), [settings and app data](https://docs.ducklocal.app/settings-and-data), [dashboards](https://docs.ducklocal.app/dashboards), and [development](https://docs.ducklocal.app/development).

The website — the product page at [ducklocal.app](https://ducklocal.app/) and the docs — lives in its own repository, [JetSquirrel/ducklocal-site](https://github.com/JetSquirrel/ducklocal-site). A change here that changes what the docs say needs a pull request there too.

## Run

```bash
cargo run                     # start with an empty workspace
cargo run -- ./data/logs/     # or open something straight away
```

## Build the macOS app

```bash
./scripts/bundle.sh
# Produces target/release/DuckLocal.app
```

The released disk image runs on macOS 12 or later, Apple silicon only. `bundle.sh` does not sign what it builds; `scripts/package-macos.sh` is the shipping path and signs and notarizes the dmg. See [development](https://docs.ducklocal.app/development).

## License

[Apache-2.0](LICENSE) · Copyright © 2026 JetSquirrel
