---
description: "Install DuckLocal on macOS, add the ducklocal command, open your first CSV, Parquet, JSON or Excel file and run a DuckDB SQL query in five minutes."
---

# Getting started

**[中文](zh/getting-started.md)** · [Docs](index.md)

DuckLocal is a desktop app for asking questions of the data files on your
machine with SQL. There is no server to run, no connection to configure, and
no account — you point it at files and start querying. This page takes you
from download to your first result in about five minutes.

## 1. Install {#install}

You need **macOS 12 or later on Apple silicon** (M1 or newer).

1. Download [`ducklocal-macos-arm64.dmg`](https://github.com/JetSquirrel/DuckLocal/releases/latest)
   from the latest release.
2. Open it and drag **DuckLocal** into **Applications**.
3. Launch DuckLocal from Applications or Spotlight.

The disk image is signed and notarized by Apple, so it opens without any
Gatekeeper workaround.

::: details Building from source instead
You need Rust stable 1.85.1 or newer and a C++ toolchain (Xcode Command Line
Tools). DuckDB is compiled from vendored sources, so the first build takes
several minutes and a few gigabytes of disk.

```bash
git clone https://github.com/JetSquirrel/DuckLocal
cd DuckLocal
cargo build --release
./target/release/ducklocal
```

The binary is `./target/release/ducklocal`; use it wherever this guide says
`ducklocal`. See [Development](development.md) for bundling a `.app`.
:::

## 2. Add the `ducklocal` command (optional) {#add-command}

Everything in the app works without a terminal. If you also want to open data
from the command line — or let an AI agent run queries with the
[CLI](cli.md) — link the binary inside the app onto your `PATH`:

```bash
sudo mkdir -p /usr/local/bin
sudo ln -sf /Applications/DuckLocal.app/Contents/MacOS/ducklocal /usr/local/bin/ducklocal
ducklocal --version
```

The link follows the app, so updating DuckLocal in Applications updates the
command too. To remove it: `sudo rm /usr/local/bin/ducklocal`.

## 3. Open some data

The first launch opens on a screen titled **Drop your data in**. From here,
any of these gets data in:

- **Drag** CSV, TSV, Parquet, JSON or Excel files — or a whole folder — onto
  the window.
- Click **Open files…** or **Open folder…**.
- From a terminal, name the paths:

  ```bash
  ducklocal ./sales.csv         # one file
  ducklocal ./logs/             # every data file under a folder, recursively
  ducklocal './data/*.parquet'  # a pattern — quote it so the shell leaves it alone
  ducklocal warehouse.duckdb    # an existing DuckDB database
  ```

Each file becomes a view named after the file — `sales.csv` becomes `sales` —
and shows up under **Local files** in the sidebar with its columns and row
count. DuckLocal reads the files where they are; nothing is copied or
uploaded.

Files you open are remembered, so the next launch starts with the same
workspace. [Data sources](data-sources.md) has the details on formats,
folders, patterns and databases.

::: tip No data handy?
The [first-10-minutes tutorial](tutorial.md) walks through a small sample
file step by step.
:::

## 4. Run a query

Click **New query** (or open some data — the editor appears on its own). Type
SQL and press **⌘↵** (Cmd+Enter):

```sql
SELECT * FROM sales LIMIT 20;
```

The result appears in the panel under the editor. Switch it to **Chart** for a
quick picture, or export it as CSV or Parquet. The toolbar above the editor
has **Run**, **Format** and **EXPLAIN**.

A shortcut worth knowing: in the sidebar, click the play button beside a
table to get a ready-made `SELECT * … LIMIT 100`, or a column name to select
just that column.

## Where to go next

- [Your first 10 minutes](tutorial.md) — a guided tour with sample data
- [Data sources](data-sources.md) — formats, folders, patterns, databases
- [SQL editor](sql-editor.md) — tabs, shortcuts, autocompletion, EXPLAIN
- [Results and charts](results-and-charts.md) — filtering, export, charts
- [Troubleshooting](troubleshooting.md) — when something does not behave
