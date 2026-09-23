# Troubleshooting

**[中文](zh/troubleshooting.md)** · [Docs](index.md)

Find the symptom, read the cause, apply the fix. If yours is not here,
[open an issue](https://github.com/JetSquirrel/DuckLocal/issues) with what you
did and what you saw.

## Installing and launching

| Symptom | Cause and fix |
| --- | --- |
| The app will not open on an Intel Mac | Releases are built for Apple silicon only. [Build from source](getting-started.md#install) on Intel |
| `ducklocal: command not found` | The disk image installs the app, not a command. [Link the binary onto your PATH](getting-started.md#add-command) |
| The terminal is busy while the window is open | Opening the GUI from a terminal runs it in the foreground. Launch from Applications instead, or leave that terminal to it |
| History, settings or remembered files stop being saved | A second DuckLocal is running and holds the app-data file. Quit the other one and relaunch |

## Opening data

| Symptom | Cause and fix |
| --- | --- |
| `File not found` on a path you named | The path does not exist, or is relative to a different directory than you think. Use an absolute path to be sure |
| A folder reports no data files | Nothing under it matched `.csv`, `.tsv`, `.txt`, `.parquet`, `.json`, `.ndjson`, `.jsonl`, `.xlsx`, `.xls`, `.xlsb` or `.ods` |
| A folder produced fewer files than expected | Hidden entries and symlinks are skipped, and one open request attaches at most 256 files. Open subfolders separately |
| A path opened as a database unexpectedly | Any existing file that is not a data file is treated as a DuckDB database. If it will not open, DuckLocal falls back to memory and shows the error |
| **Not attached …: the database already has a table or view named …** | The open database has its own relation with the name a remembered file uses. DuckLocal never replaces it. Rename or drop one of the two, or remove the file from **Local files** and open it again to get a new name |
| **Cannot restore relative path …** | A file was remembered by a relative path, which cannot be resolved in a new session. Remove it from **Local files** and open it again |
| A view shows `sales_2` instead of `sales` | Another file or table already had that name, so the new one got a suffix |
| A file's view is stale | Views read the file on every query. If a CSV's columns changed shape, use **Refresh schema** in the sidebar |

## Running queries

| Symptom | Cause and fix |
| --- | --- |
| **Run** does nothing | A query is already running. There is no cancel and no timeout; quitting and relaunching is the only way to abandon one |
| **⌘↵** shows the editor instead of running | On the first-run screen, ⌘↵ means "give me an editor". Press it again to run |
| Results stop early with a truncation notice | The 100,000-row or 2,000,000-cell cap was reached. Narrow the query; there is no pagination |
| Timestamps look shifted | The grid renders timestamps in UTC, not local time |
| Clicking a header does not sort | There is no client-side sorting. Use `ORDER BY` |
| Clicking a table or history entry wiped my SQL | Those clicks replace the whole editor buffer. Use a new query tab for scratch work |
| An export silently replaced a file | Export writes to the path you give without asking. Choose a new name |

## S3

| Symptom | Cause and fix |
| --- | --- |
| S3 credentials were forgotten | They are session-only by design, and opening or switching a database clears them. Configure S3 again |
| Configuring S3 fails offline | The first use runs `INSTALL httpfs`, which downloads the extension. Connect once, then it is cached |
| Temporary (STS) credentials are rejected | No session token is sent. Use long-lived keys — see [S3 and httpfs](s3.md#limitations) |
| A bucket looks smaller than it is | Listing stops at 1,000 entries per level. Query with a glob such as `'s3://bucket/prefix/*.parquet'` instead |

## Apps and dashboards

| Symptom | Cause and fix |
| --- | --- |
| An app tab asks **Run this app?** | The first time a folder opens as an app, DuckLocal asks before running its code, because an app's SQL can read and write anything the SQL editor can. **View source** first if you did not write it; **Trust and run** is remembered for that folder |
| A dashboard plot says a query must be read-only | A `.dash` query tried to write (DDL, DML, `COPY`, `ATTACH`, …). Dashboards only run read-only statements; move the setup into the SQL editor |
| A `PIVOT` in a dashboard is rejected | Without an explicit `IN (…)` list, DuckDB splits `PIVOT` into more than one statement. List the values: `PIVOT t ON channel IN ('Web', 'Store') USING sum(revenue)` |

## Appearance

| Symptom | Cause and fix |
| --- | --- |
| The theme resets to light on every launch | The theme is not persisted; the language is |
| The interface is in the wrong language | Use the `EN` / `中` button in the title bar; the choice is remembered |

## Starting over

To clear history, remembered files, open tabs and settings, quit DuckLocal and
remove its app-data directory:

```bash
rm -rf ~/Library/Application\ Support/DuckLocal
```

Your data files are never touched — only DuckLocal's own records. See
[Settings and app data](settings-and-data.md) for what lives there.
