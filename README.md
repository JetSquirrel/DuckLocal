# DuckLocal

<img src="assets/logo.png" width="128" alt="DuckLocal logo">

**[中文文档](README.zh-CN.md)**

A local-first, fast and simple DuckDB desktop client, built with GPUI Kit.

![DuckLocal screenshot](assets/intro.png)

## Features

- SQL editor with syntax highlighting, autocompletion and one-click formatting, across multiple query tabs
- Run queries with ⌘↵ (Cmd+Enter); inspect plans with EXPLAIN
- Schema sidebar: browse databases / schemas / tables / columns, generate SELECT queries in one click, alter a column's data type from a dialog
- Attach local CSV/TSV/Parquet/JSON files as queryable views; optional S3 support via httpfs
- Results grid with filtering, cell copy, CSV/Parquet export and built-in charts
- Query history with one-click refill into the editor
- Light and dark themes

## Run

```bash
cargo run
```

## Build the macOS app

```bash
./scripts/bundle.sh
# Produces target/release/DuckLocal.app
```

## License

[Apache-2.0](LICENSE) · Copyright © 2026 JetSquirrel
