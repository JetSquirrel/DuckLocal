---
description: "A ten-minute DuckLocal tutorial with a sample CSV: open it, query it with SQL, chart the results, export them, and ask the same question from the CLI."
---

# Your first 10 minutes

**[中文](zh/tutorial.md)** · [Docs](index.md)

A guided tour: open a small sales file, ask it a few questions, chart the
answers, and export one. It assumes DuckLocal is installed — if not, start
with [Getting started](getting-started.md).

## 1. Get the sample file {#sample-file}

Download [sales.csv](/samples/sales.csv) — two weeks of made-up orders, 224
rows. Or from a terminal:

```bash
curl -LO https://jetsquirrel.github.io/DuckLocal/samples/sales.csv
```

It has five columns:

| Column | Type | Example |
| --- | --- | --- |
| `date` | DATE | `2026-09-01` |
| `channel` | VARCHAR | `Web`, `Mobile app`, `Store`, `Phone` |
| `region` | VARCHAR | `North`, `South`, `East`, `West` |
| `orders` | BIGINT | `72` |
| `revenue` | DOUBLE | `4181.04` |

You did not have to tell DuckLocal any of that: DuckDB reads the header and
infers the types on its own.

## 2. Open it

Drag `sales.csv` onto the DuckLocal window. (Or click **Open files…**, or run
`ducklocal sales.csv` if you [added the command](getting-started.md#add-command).)

A notification confirms **Created view sales.** In the sidebar, **Local
files** now lists `sales` with its 224 rows; expand it to see the columns and
their types.

## 3. Look at the rows

In the sidebar, click the play button beside `sales`. The editor fills with a
`SELECT * … LIMIT 100` for it — press **⌘↵** to run it.

The grid under the editor shows the rows. Try the filter box above it: type
`Store` and press Enter to keep only the rows that mention it. The filter
works on the rows already loaded and never re-queries.

## 4. Ask a question

Replace the SQL with this and press **⌘↵**:

```sql
SELECT channel, round(sum(revenue), 2) AS revenue
FROM sales
GROUP BY channel
ORDER BY revenue DESC;
```

| channel | revenue |
| --- | --- |
| Mobile app | 290549.76 |
| Web | 289544.8 |
| Store | 240067.6 |
| Phone | 82511.2 |

Now switch the results panel from **Results** to **Chart**. Because the first
column is text, you get a bar per channel, sized by the first numeric column.

::: tip Autocompletion
While you type, the editor offers table and column names from the sidebar
first, then DuckDB functions and SQL keywords — type `rev` and `revenue` is
the first suggestion.
:::

## 5. Chart a trend

Charts are drawn from the rows exactly as the query returns them, so shape
the result the way you want it plotted. One column per channel gives one line
per channel:

```sql
SELECT
  date,
  round(sum(revenue) FILTER (WHERE channel = 'Web'), 2)        AS web,
  round(sum(revenue) FILTER (WHERE channel = 'Mobile app'), 2) AS mobile,
  round(sum(revenue) FILTER (WHERE channel = 'Store'), 2)      AS store
FROM sales
GROUP BY date
ORDER BY date;
```

With a date in the first column, **Chart** draws an area chart over time,
one series per numeric column. [Results and charts](results-and-charts.md)
covers the rules and limits.

## 6. Export the answer

With the trend still in the results panel, click **Export CSV**. The dialog
suggests `~/Desktop/export.csv`; change the path if you like and confirm.

Export re-runs the query, so the file holds the full result — not just what
is visible in the grid. **It overwrites an existing file without asking.**

## 7. Find it again tomorrow

Open the **History** tab in the sidebar: every query you ran is there, the
failed ones included. Click one to put it back in the editor — note that this
replaces what the editor holds.

Quit DuckLocal and launch it again. `sales` is still under **Local files**:
opened files are remembered by path and re-read on every launch, so an edit
to the CSV shows up the next time you query it.

## 8. Bonus: the same question from a terminal

If you added the `ducklocal` command, the CLI answers without opening a
window — handy for scripts and for AI agents:

```bash
ducklocal query --format md --sql "
  SELECT channel, round(sum(revenue), 2) AS revenue
  FROM 'sales.csv' GROUP BY channel ORDER BY revenue DESC"
```

Leave out `--format md` for structured JSON. The [CLI guide](cli.md) and the
official agent skill are the place to start.

## Next

- Point DuckLocal at your own files or folders — see [Data sources](data-sources.md)
- Query files in S3-compatible storage — see [S3 and httpfs](s3.md)
- Build a dashboard of saved queries — see [Analysis apps and dashboards](analysis-app.md#dashboard-specs-dash)
