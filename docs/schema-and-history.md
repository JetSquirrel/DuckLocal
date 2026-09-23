# Schema browser and history

**[中文](zh/schema-and-history.md)** · [Docs](index.md)

The sidebar has two tabs: **Schema** and **History** (**表结构** and **查询历史** in Chinese). Beside them, the collapse button (or **⌘B**) hides the sidebar; the button at the left of the title bar, or ⌘B again, brings it back. Whether it is hidden is remembered.

## The schema tree

The tree shows, from the top:

- **Local files** — the data files you have registered, each named by its
  view, with its row count. The file name appears beside the view name only
  when the two differ (`sales_2` from `sales.csv`), and the folder when two
  files share a view name; hover a row for the full path. Its children are the
  view's columns.
- **Apps** / **Dashboards** — the analysis apps and `.dash` dashboards you
  have opened recently. Click one to reopen it as a tab, the way you would a
  file; an entry whose path no longer exists is dropped from the list.
- **S3** — present once [S3 is configured](s3.md). Loads lazily.
- Each database, then its schemas, then tables and views, then columns.

Tables show an estimated row count; views show none. Column rows show the
DuckDB data type. The system database and the `information_schema`,
`pg_catalog`, and `system` schemas are hidden.

**Refresh schema** reloads the catalog and re-attaches the registered files.

## Generating a SELECT

Three things fill the active editor when clicked:

| Click | Generated SQL |
| --- | --- |
| A table's play button | `SELECT * FROM db.schema.table LIMIT 100;` |
| A table's column name | `SELECT col FROM db.schema.table LIMIT 100;` |
| An S3 object that is a data file | `SELECT * FROM 's3://bucket/key' LIMIT 100;` |

Names are fully qualified as `database.schema.table`, and double-quoted only
when they are not plain identifiers.

Note that all three **replace the entire buffer** of the active tab — they do
not insert at the cursor. The same is true of clicking a history entry. Save
anything you care about before clicking around.

## Altering a column type

A column row offers a type action (not available on view columns, so attached
CSV and Parquet columns cannot be retyped). It opens a free-text dialog
pre-filled with the current type and runs:

```sql
ALTER TABLE db.schema.table ALTER COLUMN "col" SET DATA TYPE <what you typed>;
```

There is no picker and no validation list — any DuckDB type string works,
including parameterised ones like `DECIMAL(10,2)`. An invalid type comes back
as an error notification. Success reloads the catalog.

Renaming and dropping columns, and editing column comments, are not available.

## Removing a registered file

The **×** on a file row asks for confirmation, then drops the view and forgets
the registration. **The file on disk is not touched.**

## Query history

Every run is recorded — successes and failures alike — with a local-time
timestamp, duration in milliseconds, row count (or affected-row count for
DML), and the error text if it failed. Failed entries are shown in red with a
*Failed* tag.

The list shows the newest **200** entries. Clicking a row refills the active
editor with that SQL and focuses it, replacing whatever was there.

There is no retention policy and no pruning: the history table grows without
bound, and the UI simply shows the most recent 200. There is also no search,
no filter, no export, and no way to delete an entry from the interface.

History is stored in DuckLocal's own database file, not in the database you
have open, so it survives switching databases and losing your workspace. See
[Settings and app data](settings-and-data.md) for where it lives.
