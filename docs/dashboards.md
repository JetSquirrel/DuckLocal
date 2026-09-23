---
description: "Write a DuckLocal dashboard as a .dash file — query and plot blocks over DuckDB SQL — then open it as a tab, validate it with ducklocal check, and edit it with LSP support."
---

# Dashboards (.dash)

**[中文](zh/dashboards.md)** · [Docs](index.md)

A `.dash` file is a dashboard written down as data: a few **query** blocks
holding SQL, and **plot** blocks that draw their results. There is no code to
write, so a person or an AI agent can produce one, review it in a diff, and
check it before anyone opens it.

Use a `.dash` file for standard charts and tables over saved queries. For a
custom layout or interaction — buttons, filters, your own components — write
an [analysis app](analysis-app.md) instead.

## A complete example

This dashboard reads the `sales` view you get by opening the
[tutorial's sample file](tutorial.md#sample-file). Download it as
[sales.dash](https://jetsquirrel.github.io/DuckLocal/samples/sales.dash).

```hcl
query "by_channel" {
  sql = <<SQL
    SELECT channel, round(sum(revenue), 2) AS revenue
    FROM sales
    GROUP BY channel
    ORDER BY revenue DESC
  SQL
}

query "daily" {
  sql = <<SQL
    SELECT date, channel, round(sum(revenue), 2) AS revenue
    FROM sales
    GROUP BY date, channel
    ORDER BY date
  SQL
}

plot "revenue_by_channel" {
  type  = "bar"
  query = query.by_channel
  x     = channel
  y     = revenue
  title = "Revenue by channel"
}

plot "daily_revenue" {
  type   = "line"
  query  = query.daily
  x      = date
  y      = revenue
  series = channel
  title  = "Daily revenue, per channel"
}

plot "daily_table" {
  type  = "table"
  query = query.daily
  x     = date
  title = "The numbers behind it"
}
```

Each query runs once, however many plots draw from it: `daily` feeds both the
line chart and the table.

## Open a dashboard

Any of these opens a `.dash` file as a dashboard tab, next to your queries:

- the **+** at the end of the tab strip, then **Open dashboard…**;
- dragging the file onto the window;
- naming it on the command line: `ducklocal sales.dash`.

The plots stack vertically; drag the dividers to resize them. A plot that
cannot draw — its query failed, or a column is missing — shows the reason in
its own panel, and the rest of the dashboard still draws. Open dashboards come
back on the next launch, and recently opened ones are listed under
**Dashboards** in the sidebar.

The toolbar's **View source** switches the tab to an editor for the file, with
syntax highlighting (the SQL in heredocs is coloured like the SQL editor's),
completion and inline diagnostics. **Save** or **⌘S** writes it back and
redraws. A save that no longer validates keeps the last working dashboard on
screen and shows why above it; so does a change made to the file by another
program. **Reload** re-reads the file and runs every query again.

## Blocks

A file is a sequence of blocks, each a type, a quoted name and a body:

```hcl
query "name" { ... }
plot "name" { ... }
```

Names must be unique within their kind: two queries cannot share a name, and
neither can two plots — but a query and a plot may.

### query

| Attribute | Required | Value |
| --- | --- | --- |
| `sql` | yes | One read-only SQL statement, as a [heredoc](#heredocs) or a string |

A query block holds `sql` and nothing else.

### plot

| Attribute | Required | Value |
| --- | --- | --- |
| `type` | yes | `"line"`, `"bar"`, `"area"`, `"scatter"` or `"table"` — a quoted string |
| `query` | yes | A reference to a query block: `query.by_channel` |
| `x` | yes | The column on the horizontal axis. A `table` shows the whole result, so its `x` only has to name one of the columns |
| `y` | yes, except for `table` | The column to plot; must be numeric |
| `series` | no | A column whose values split the rows into one series each |
| `title` | no | A heading for the plot, as a quoted string |

`x`, `y` and `series` name columns of the query's result, matched without
regard to case. Write a plain name bare (`x = date`); quote one that is not a
plain identifier (`y = "revenue (EUR)"`).

### Plot types

| Type | Draws |
| --- | --- |
| `line` | A line per series. With more than one series it reads as lightly filled areas |
| `area` | A filled area per series |
| `bar` | A bar per row. With `series`, one small bar chart per series, stacked |
| `scatter` | Points. With more than one series they are drawn as unmarked lines |
| `table` | The query's result as a grid; `y` and `series` are not used |

Rows are plotted in the order the query returns them, so end a query that
feeds a chart with `ORDER BY`.

## Values

| Form | Example | Used for |
| --- | --- | --- |
| String | `"Revenue by channel"` | `type`, `title`, quoted column names, short `sql` |
| Heredoc | `<<SQL` … `SQL` | `sql` |
| Reference | `query.daily` | `query` |
| Bare name | `channel` | `x`, `y`, `series` |

A string ends at its line and understands `\n`, `\t`, `\"` and `\\`.

`#` and `//` start a comment that runs to the end of the line. Attributes need
no separator; line breaks are just whitespace.

That is the whole language: no variables, no functions, no conditionals, no
interpolation.

### Heredocs

A heredoc holds text across lines — in practice, SQL:

```hcl
  sql = <<SQL
    SELECT 1
  SQL
```

- The delimiter after `<<` is any word (`SQL` by convention), and nothing may
  follow it on its line.
- The heredoc ends at the first line that is exactly the delimiter, give or
  take surrounding spaces.
- The closing line's indentation is removed from every line of the text, so
  the SQL can sit at the block's indentation without carrying it.

## SQL rules

A dashboard runs its queries whenever it is opened, reloaded or saved, and when
DuckLocal starts with the tab restored — so a query may only **read**:

- **Allowed:** `SELECT`, `WITH …`, `FROM`-first queries, `VALUES`, `SHOW`,
  `DESCRIBE`, `SUMMARIZE`, and `PIVOT` with an explicit `IN (…)` list.
- **Refused:** anything that could change data or files — `CREATE`, `INSERT`,
  `UPDATE`, `DELETE`, `DROP`, `COPY`, `ATTACH`, `INSTALL`, `LOAD`, and so on.
  Opening a `.dash` file someone sent you cannot change your data.
- **One statement per query.** A `PIVOT` without an `IN (…)` list counts as
  two, because DuckDB runs it as a statement that finds the values plus the
  pivot itself; list the values instead.

Queries run on the window's own connection, so they see everything the SQL
editor does: opened files, attached databases, and connection-level state such
as `TEMP` tables.

### Paths in SQL

A relative path such as `FROM 'sales.csv'` resolves against DuckLocal's working
directory — the folder you ran `ducklocal` in, or `/` when it was launched from
Finder or the Dock. To write a dashboard that works however it is opened:

- query a **view** instead — open the file once, and use the view name it gets
  (`FROM sales`), as the example above does; or
- use an **absolute path** (`FROM '/Users/me/data/sales.csv'`).

## Limits

| Limit | What happens |
| --- | --- |
| 12 series per plot | Further series are left out, and a notice says how many there were |
| 1,500 points per series | Points are bucket-averaged down to fit |
| 200 bars | Further bars are left out |

## Validate a dashboard

`ducklocal check` validates a file without opening a window — the step to run
before handing a dashboard to anyone:

```bash
ducklocal check sales.dash
ducklocal check sales.dash --database warehouse.duckdb
```

- **Without `--database`** the check is static: the syntax, the names and
  references, the required attributes, and each query's SQL through DuckDB's
  own parser. Nothing runs and no table needs to exist.
- **With `--database PATH`** every query also runs against that database
  (opened read-only), and each plot's `x`, `y` and `series` is checked against
  the columns its query really returns — including that `y` is numeric.

Success prints a JSON summary of the queries and plots, with their result
columns when a database was given. A mistake exits with status **2** and lists
every problem at once, one `file:line: message` each; a database or file error
exits with **1**. The [CLI guide](cli.md#check-a-dashboard-spec) has the
details.

### Common messages

| Message | Fix |
| --- | --- |
| `type is a string in quotes` | Write `type = "line"`, not `type = line` |
| `Unknown plot type: "pie"; one of line, bar, area, scatter, table` | Use one of the listed types |
| `plot "p" requires y` | Every type but `table` needs a `y` |
| `plot "p" draws query "q", which the file does not define` | Add the query block, or fix the name in `query = query.…` |
| `query names a query block: query = query.some_name` | The `query` attribute takes a reference, not a string |
| `plot "p" draws y = "channel" (VARCHAR), which is not numeric` | Point `y` at a number column, or use `type = "table"` |
| `a dashboard query must be a single read-only statement` | Move writes to the SQL editor; see [SQL rules](#sql-rules) |
| `Unterminated heredoc: no line is exactly SQL` | Add the closing delimiter on a line of its own |

## Edit in your own editor

`ducklocal lsp` is a language server for `.dash` files: diagnostics as you
type, completion for block types, attributes, plot types and query names,
hover documentation, and go-to-definition from `query.name` to its block.
Setup for Neovim and VS Code is in the
[CLI guide](cli.md#edit-a-dashboard-spec-with-lsp).
