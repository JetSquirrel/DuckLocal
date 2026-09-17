# Data sources

**[中文](zh/data-sources.md)** · [Docs](index.md)

DuckLocal turns files into views in an in-memory DuckDB connection, or in a
database file if you opened one. There is nothing to configure.

## Ways to open data

All three entry points go through the same resolution rules:

1. Paths on the command line — `ducklocal ./logs/ billing.parquet`
2. The **Open data…** dialog in the title bar, which accepts typed paths or
   paths from the native picker, and can open several at once
3. Dropping files or folders anywhere on the window

## Supported file formats

Extensions are matched case-insensitively.

| Extension | DuckDB reader |
| --- | --- |
| `.csv`, `.tsv`, `.txt` | `read_csv_auto` |
| `.parquet` | `read_parquet` |
| `.json`, `.ndjson`, `.jsonl` | `read_json_auto` |

Anything else is not a data file — see [Database files](#database-files).

## Folders and patterns

A **directory** is walked recursively, and every data file inside becomes a
view. Two rules apply to the walk:

- Hidden entries (names starting with `.`) are skipped, and their subtrees are
  not descended into.
- Symlinks are not followed. A link back up the tree would not terminate, and
  the linked-to directory is normally walked where it actually lives.

Files are attached in sorted order, so the same folder always produces the
same workspace.

A **pattern** may contain `*` (any run of characters) and `?` (exactly one).
Matching happens per path component, against the filesystem, so a pattern can
span directories:

```bash
ducklocal './data/2026-0*/*.parquet'
```

There is no escape character. `**` is not special — it behaves like a single
`*` and does not recurse. A pattern that matches a directory ignores it; only
files are attached. Hidden entries are skipped here too.

Unquoted patterns are expanded by your shell before DuckLocal sees them, which
is fine — quote them only when you want DuckLocal to do the expansion (for
example when a shell has no match and would otherwise error out).

## Database files

Any path that **exists** and is not a data file is opened as a DuckDB database
instead of being attached as a view. The extension does not matter: a `.db`,
`.duckdb`, or even a `.md` path is treated as a database, and DuckDB decides
whether it can actually be opened.

Two consequences are worth knowing:

- **You cannot create a new database by naming one.** A path that does not
  exist is reported as not found. Open an existing database file, or start
  in-memory.
- If the file cannot be opened as DuckDB, DuckLocal falls back to an in-memory
  workspace and reports the error, rather than failing to start.

Attaching data files while a database file is open **writes those views into
that database file** — they persist there, not merely in the session.

## Naming

A file's view is named after its file stem, cleaned up into a valid identifier.
When that name is already taken in the connection, DuckLocal appends a counter:
`events`, then `events_2`, `events_3`. Two files with the same stem in
different directories therefore get distinct views.

Duplicate detection is by the path text as written, so `./a.csv` and `a.csv`
are treated as two different files.

## Limits

- **256 data files per open request.** The cap covers all the paths in one
  request together, not each folder separately — it exists so that dropping a
  home directory by accident does not tie up the connection for minutes. When
  it bites, DuckLocal says so and attaches the first 256 files.
- Files re-attached at startup from the previous session are not subject to
  that cap.

## Managing registered sources

Registered files appear in the sidebar's **Local files** group, each with its
view name, file name, and row count. Selecting a file generates a `SELECT`
statement for it.

The **×** on a file row removes it, after a confirmation dialog. Removal drops
the view and forgets the registration; **the underlying file is untouched.**

**Refresh schema** reloads the catalog and the registered files.

## What is remembered

The list of registered files — their paths, view names, and kinds — is stored
in DuckLocal's own history database and re-attached on the next launch, so the
workspace you left is the workspace you get back. The database file itself is
*not* remembered: each launch starts in memory unless you name a database.

See [Settings and app data](settings-and-data.md) for the on-disk locations.
