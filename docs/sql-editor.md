# SQL editor

**[中文](zh/sql-editor.md)** · [Docs](index.md)

Each query tab holds its own editor buffer. The editor highlights real SQL via
tree-sitter, indents with 2 spaces, and shows line numbers.

## Tabs

| Control | What it does |
| --- | --- |
| `+` | Appends a new query tab and focuses it |
| Click a tab | Switches to it |
| `×` | Closes a tab — hidden while only one tab remains, so the last one cannot be closed |
| Rename (toolbar) | Opens a dialog to retitle the active tab; an empty name falls back to `Query N` |

Tabs live in memory only. Their SQL is not saved, so quitting loses every tab
except the fact that you get one fresh tab next launch.

## Running SQL

Press **⌘↵** (Cmd+Enter) with the editor focused, or use the `Run` button.

While a query is running, pressing run again does nothing: there is **no
cancel and no timeout**. Long queries run as long as DuckDB needs, and other
database work queues behind them. If something appears hung, that is why —
check the status bar, or quit and restart.

How a statement is treated depends on its leading keyword. `SELECT`, `WITH`,
`SHOW`, `DESCRIBE`, `EXPLAIN`, `PRAGMA`, `SUMMARIZE`, `VALUES`, `FROM`,
`TABLE`, `PIVOT`, and `CALL` produce a result set. Anything else (DDL, DML)
reports `Done · N rows affected · took …` instead of a grid. Leading `--`
comments are skipped when deciding; block comments are not.

## Errors

A failed statement shows an alert titled *Query failed* with DuckDB's message
verbatim. The message comes from the engine and is always in English,
whatever the interface language is. Nothing is written to the grid, and the
attempt is still recorded in [history](schema-and-history.md) with a failure
marker.

## Format

`Format` runs the buffer through `sqlformat` with a 2-space indent and
uppercased keywords, then replaces the entire buffer. It is a no-op on an
empty buffer.

## EXPLAIN

`EXPLAIN` runs `EXPLAIN <your SQL>` and shows the plan as plain monospace text
with the elapsed time, switching the results panel back to the Results tab.

## Autocompletion

The completion menu is catalog-aware. Candidates are ranked:

1. Tables and columns from the schema (up to 35 entries)
2. DuckDB function names
3. SQL keywords

At most 50 items are offered. Matching is a case-insensitive **prefix** match
on the identifier immediately to the left of the cursor, so `sel` completes to
`SELECT` but `elect` completes to nothing. A function inserts its name and an
opening parenthesis; identifiers that are not plain are inserted double-quoted.

The keyword and function lists are a curated subset — not every DuckDB
function will appear.

## Keyboard shortcuts

DuckLocal defines a handful of shortcuts itself; everything else comes from
the editor and table components, and behaves as it does in any macOS text
field:

| Shortcut | Action |
| --- | --- |
| ⌘↵ | Run the query |
| ⌘S | Save a dashboard's source, in its source view |
| ⌘+ / ⌘− / ⌘0 | Make the interface larger / smaller / default size — see [Interface size](settings-and-data.md#interface-size) |
| ⌘F / ⇧⌘F | Find / replace in the editor |
| ⌘Z / ⇧⌘Z | Undo / redo |
| ⌘A | Select all |
| ⌘C / ⌘X / ⌘V | Copy / cut / paste |
| ⌥⌘↑ / ⌥⌘↓ | Add a cursor above / below |
| ⌘← / ⌘→ / ⌥← / ⌥→ | Move by line / word |
| ⌘↑ / ⌘↓ | Move to start / end of document |
| ⌘⌫ / ⌥⌫ | Delete to start of line / previous word |

There is no menu bar and no command palette. Every other action is a button:
in the toolbar above the editor, in the tab bar, in the sidebar, and in the
title bar.
