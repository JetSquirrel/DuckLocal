# Settings and app data

**[中文](zh/settings-and-data.md)** · [Docs](index.md)

DuckLocal keeps one file on disk. Everything else lives in memory for the
duration of a session.

## Where things are stored

| Platform | Path |
| --- | --- |
| macOS | `~/Library/Application Support/DuckLocal/history.duckdb` |
| Linux (if built there) | `~/.local/share/ducklocal/history.duckdb` |

That single DuckDB file holds three tables: the query history, the registered
data files, and a small key/value settings table. It lives beside a `.wal`
while the app is running.

Your data is never copied into it. Registered files are referenced by path and
re-attached on launch.

## What survives a restart

| Remembered | Where |
| --- | --- |
| Registered data files — path, view name, kind | app data file |
| Query history, including failures | app data file |
| Interface language | app data file |

| Not remembered | Notes |
| --- | --- |
| Open tabs and their SQL | in memory only |
| The results panel contents | in memory only |
| Which database file is open | each launch starts in memory unless you name one |
| Window size and position | fixed at 1440×900, minimum 960×600 |
| Sidebar and results panel sizes | fixed per launch |
| The results filter | in memory only |
| **The theme** | always starts light — see below |
| S3 credentials | session-only by design |

The theme is the odd one out: the language toggle persists, the theme toggle
does not, and every launch begins in light mode.

## Language

The `EN` / `中` button in the title bar switches the interface language
immediately and records the choice.

On first run — before any setting is stored — the language is taken from the
system locale: `LC_ALL` or `LANG` containing `zh` selects Chinese, anything
else selects English. An unrecognized stored value falls back to Chinese.

A few strings are deliberately not translated: `DuckLocal`, the `S3` and
`EXPLAIN` button labels, the `EN` / `中` toggle, and the credential placeholders.
Engine and network error messages are surfaced verbatim and are always in
English, whatever the interface language is.

## Themes

The sun/moon button toggles between the light and dark theme. There is no
theme picker and no theme file to edit — the two modes are what you get. The
base font size is 14px.

## Resetting

To start over — clearing history, registered files, and the language setting —
quit DuckLocal and remove its app data directory:

```bash
rm -rf ~/Library/Application\ Support/DuckLocal
```

The next launch creates it fresh. **Your data files are never affected by
this**, only DuckLocal's own records.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `File not found` on a path you named | The path does not exist. DuckLocal cannot create a database by naming a new file |
| A folder reports no data files | Nothing under it matched `.csv`, `.tsv`, `.txt`, `.parquet`, `.json`, `.ndjson`, or `.jsonl` |
| A folder produced fewer files than expected | Hidden entries and symlinks are skipped, and one open request attaches at most 256 files |
| A path opened as a database unexpectedly | Any existing non-data file is treated as a DuckDB database; if it will not open, DuckLocal falls back to memory and shows the error |
| Run does nothing | A query is already running. There is no cancel and no timeout |
| Results stop early with a truncation notice | The 100,000-row or 2,000,000-cell cap was reached |
| S3 credentials forgotten | Opening or switching a database clears them |
| Timestamps look shifted | The grid renders timestamps in UTC, not local time |
| The theme resets to light | It is not persisted |

If a query seems stuck, the status bar shows the DuckDB version and connection
state. Quitting and relaunching is the only way to abandon a running query.
