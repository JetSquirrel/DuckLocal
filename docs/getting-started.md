# Getting started

**[中文](zh/getting-started.md)** · [Docs](index.md)

## Requirements

The released build runs on **macOS 12.0 or later, Apple silicon only**. The
distributed disk image is signed and notarized, so it opens without a
Gatekeeper workaround.

To build from source you need Rust stable, **1.85.1 or newer**, and a C++
toolchain — DuckDB is compiled from vendored sources. The first build takes a
while and needs a few gigabytes of disk.

## Install

**From the disk image.** Download `ducklocal-macos-arm64.dmg`, open it, and
drag `DuckLocal.app` to Applications.

**From source.**

```bash
git clone https://github.com/JetSquirrel/DuckLocal
cd DuckLocal
cargo build --release
./target/release/ducklocal
```

`cargo run` works too, and is quicker to iterate on. Builds are slow the first
time because of DuckDB; afterwards only DuckLocal itself recompiles.

## Open your data

DuckLocal will open whatever you name on the command line:

```bash
ducklocal ./logs/             # every data file under a folder, recursively
ducklocal ./billing.parquet   # one file
ducklocal './data/*.csv'      # a pattern (quote it, so the shell does not expand it)
ducklocal warehouse.duckdb    # or an existing DuckDB database
```

You can name several paths at once, and mix them freely.

There are two more ways in, both equivalent:

- **Open data…** in the title bar opens a dialog where you type or browse for
  paths. An **In-memory** button in that dialog switches to a plain in-memory
  workspace with nothing attached.
- **Drag files or folders onto the window.**

Every CSV, TSV, Parquet, JSON, or Excel file becomes a queryable relation, and every file is
registered — the next launch starts with the same workspace. See
[Data sources](data-sources.md) for the details.

## Run a query

The workspace opens with one query tab. Type SQL and press **⌘↵** (Cmd+Enter)
to run it. `Run`, `Format`, and `EXPLAIN` sit in the toolbar above the editor.

Results appear in the panel below, as a grid or a chart.

## The first-run screen

When nothing is attached and nothing has been registered yet, the workspace
shows three buttons instead of the editor: **Open file…**, **Open folder…**,
and **New query**. Pick one of the first two to attach data, or **New query**
to get an editor anyway.

One quirk worth knowing: on this screen **⌘↵** reveals the editor rather than
running anything.

## Where to go next

- [Data sources](data-sources.md) — formats, folders, patterns, databases
- [SQL editor](sql-editor.md) — tabs, shortcuts, EXPLAIN
- [Results and charts](results-and-charts.md) — what you can do with a result
- [Settings and app data](settings-and-data.md) — where everything is stored
