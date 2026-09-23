---
description: "Work with DuckLocal query results: filter and copy cells, export to CSV or Parquet, and draw built-in bar and area charts, with their row limits."
---

# Results and charts

**[中文](zh/results-and-charts.md)** · [Docs](index.md)

The panel under the editor has two tabs: **Results** and **Chart**.

## The results grid

The grid shows a leading `#` row-number column, then your columns. Numeric
columns are right-aligned, and column widths can be dragged. The `#` column
cannot be resized.

**There is no sorting.** Clicking a header does nothing — sort in SQL with
`ORDER BY` instead.

### Cell rendering

| Value | Shown as |
| --- | --- |
| `NULL` | the literal `NULL`, in a muted color |
| BLOB | `<blob N bytes>` |
| GEOMETRY | `<geometry N bytes>` |
| Integral float | `3.0` |
| Timestamp | `2026-09-17 04:21:09` — **in UTC, not local time** |
| List / struct / map | pretty-printed |

The UTC rendering is worth remembering if you are eyeballing timestamps rather
than comparing them in SQL.

### Filtering

The filter box does a case-insensitive substring match **across all cells** of
the loaded rows. It is client-side: it never queries the database, and it
counts against the rows that were loaded, not the true result size.

The filter applies when you press Enter or clear the box, not on every
keystroke. Beside it, the grid reports `visible / total rows`.

## Row limits

Results are truncated at whichever of these binds first:

- **100,000 rows**
- **2,000,000 cells** — so a 400-column result materializes only about 5,000
  rows

The cell budget exists because a row cap alone does not bound the work: a
`PIVOT` over a high-cardinality column returns one column per distinct value,
and formatting tens of millions of cells would look like a hang.

Truncation is reported in a banner that quotes the real number of rows that
were materialized. There is **no pagination and no "load more"** — narrow the
query instead.

## Copying

Hovering a cell reveals a copy button that puts that cell's text on the
clipboard. That is the only copy affordance: there is no row copy, no range
selection, no "copy as SQL", and no context menu.

## Export

`Export CSV` and `Export Parquet` open a small dialog with a path field,
pre-filled with `~/Desktop/export.csv` or `~/Desktop/export.parquet`.

Two things to know:

- The export **re-runs the SQL that produced the current rows**, so the file
  holds the query's full result set — not the filtered view, and not the
  truncated grid.
- There is no file picker and no overwrite prompt. Type a path, and it is
  written.

## Charts

Charts are derived from the rows already in the grid, in the order the query
returned them. **No aggregation is applied** — these are not `GROUP BY` charts.

The chart type is chosen for you, based on the first column:

| First column | Chart |
| --- | --- |
| Temporal (date / timestamp) | Area chart; x is column 0, one series per numeric column |
| Anything else | Bar chart; column 0 is the label, the first numeric column is the value |

Rows whose value cells are not numeric — `NULL`, for instance — are dropped
before plotting.

Limits, all announced in a notice line under the chart title:

| Limit | Behaviour |
| --- | --- |
| 12 series | Extra numeric columns are dropped |
| 1,500 points | The series is bucketed and **averaged** within each bucket |
| 200 bars | Surplus bars are truncated, not merged |

The title always reports how many points were plotted, so check it before
trusting the shape of a curve.
