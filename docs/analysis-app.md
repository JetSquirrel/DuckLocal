# Analysis apps (JavaScript)

**[中文](zh/analysis-app.md)** · [Docs](index.md)

An analysis app is a workspace tab whose contents are a JavaScript application you
wrote. JavaScript is the declaration: the app's `render` function is the description of
the interface, and the range controls, chips, KPI cards, charts and tables are all yours
to compose. An analysis app reads the database the window already has open — the same
tables, views and attached files — through the host functions below.

This is for building a view over your data: a dashboard, a purpose-built browser for one
table, a report you run every morning. An analysis app opens and closes like a query, and
sits beside your SQL tabs.

## Open an app

Any of these opens one, and each loads the folder immediately:

- the **+** at the end of the tab strip, then **Open app…**;
- drag an app folder onto the window;
- name it on the command line: `ducklocal examples/analysis_app`.

A folder that cannot be an app — a file, a path that is gone, or a folder with no
`main.js` — is refused **inside the tab**, naming the folder and what is missing. An app
that is already working is never thrown away by a folder you cancelled out of or named by
mistake.

Apps close with the tab's close button and rename like a query tab. The app folders you
have open are remembered: they reopen at the next launch, and one that has been moved or
deleted is reported by name rather than silently forgotten.

Apps run on their own DuckDB connection to the database the main window has open, so an
app and the SQL editor see the same tables, views and attached files without waiting on
each other: a dashboard refreshing half a dozen statements no longer freezes the editor.
What a second connection does not carry is connection-local state — a `TEMP` table or a
`SET` made in the SQL editor is not there for an app, and the other way round. All apps
share that one connection, so they still queue behind each other.

## View definition

**View definition** in the app's header shows the folder and the entry file's source,
read-only. An app is an ordinary directory of JavaScript files, so that is the whole
definition — there is no second, hidden form of the app to inspect.

## Reload

Save a `.js` or `.mjs` file in the app's folder and the app reloads. The host polls the
folder every 250 ms and waits 200 ms for writes to settle, so saving six files at once is
one reload. Dotfiles, `node_modules/` and `target/` are ignored. Only the folder currently
shown is watched.

**Reload** in the app's header does the same thing on demand.

A reload that fails **keeps the last app that loaded** and reports the error above it,
inside the tab. A load that fails on an app that has never loaded shows the error as the
tab's whole body instead.

A reload restarts the app's JavaScript, not its connection. The app connection is one
long-lived connection shared by every app, so state an app made on it — an `ATTACH`, a
`SET`, a `TEMP` table — is still there after the reload. Write an app's setup so that
running it twice is safe (`ATTACH IF NOT EXISTS …`, `CREATE OR REPLACE TEMP TABLE …`)
rather than assuming a clean connection on every load.

## Host functions

Import them from the `ducklocal` module:

```js
import { catalog, query, appDir, sqlLiteral, sqlIdentifier } from "ducklocal";
```

They all run against the app connection — the same database the main window is on — and
they throw JavaScript `Error`s on failure. Everything but `appDir` may be called at any
time; `appDir` answers during the app's load.

### `catalog()`

```ts
catalog(): Promise<CatalogEntry[]>

interface CatalogEntry {
  database: string;          // e.g. "memory"
  schema: string;            // e.g. "main"
  name: string;              // e.g. "orders"
  kind: "table" | "view";
  estimated_rows: number | null;
  comment: string | null;
  columns: { name: string; type: string }[];   // type is DuckDB's, e.g. "DECIMAL(38,10)"
}
```

Every table and view of every database on the connection — the one the window opened and
anything `ATTACH`ed, by the window or by the app itself — except DuckDB's own catalogs
(`information_schema`, `pg_catalog`, `system`). `entry.database` says which database an
entry belongs to; use it when you qualify a name. Tables come before views. DuckDB resolves
identifiers case-insensitively and keeps tables and views in one namespace, so qualify a
name when you build SQL from it, and quote it with `sqlIdentifier`.

### `query(sql, limit?)`

```ts
query(sql: string, limit?: number): Promise<QueryResult>

interface QueryResult {
  columns: { name: string; type: string }[];
  rows: any[][];             // one array per row, one entry per column
  row_count: number;         // rows actually returned
  truncated: boolean;        // true when the row limit stopped the result early
  elapsed_ms: number;
}
```

- `limit` defaults to **1000**; a value below 1 is refused and anything above 100000 is
  clamped. A 2,000,000-cell budget applies
  as well, so a very wide result returns fewer rows than `limit`. **Read `truncated`.**
- `type` is the Arrow type as rendered by DuckLocal, e.g. `Int64`, `Utf8`,
  `Decimal(38, 10)`, `Timestamp(Microsecond, None)`.
- The statement is prepared and executed as given; `query()` does not split or validate
  multi-statement SQL.

### `appDir()`

```ts
appDir(): string   // the app's own folder, absolute
```

An app cannot read the filesystem, so this is how it addresses data that ships beside it —
`query("SELECT * FROM " + sqlLiteral(appDir() + "/orders.csv"))` works with nothing
attached and nothing open in the window. It answers while the app loads: call it in `init()`
and keep the result.

`panelDir()` is the deprecated alias from before apps were called apps; it keeps working,
but new code should use `appDir()`.

### `sqlLiteral(value)`

```ts
sqlLiteral(value: string): string   // e.g. "O'Brien" -> 'O''Brien'
```

Quotes and escapes a string as a SQL string literal. Use it for any value that comes from
your app's own state — a filter, a channel id, a date — instead of templating it into SQL
by hand. A `NUL` byte is refused, because a SQL literal cannot carry one.

### `sqlIdentifier(name)`

```ts
sqlIdentifier(name: string): string   // e.g. "my table" -> "my table"
```

The same job for the other half of a statement: a table, view or column **name**, quoted so
it is read as one name and matched exactly as written. A name is not a string literal, and
the names an app builds SQL from are rarely its own — `catalog()` answers with whatever the
database holds, so `my table`, `Order`, and a name holding a `"` all turn up. An empty name
and a `NUL` byte are refused; neither names anything, and quoting them would only hide that.

```js
const entry = (await catalog()).find((table) => table.name === "orders");
const from = `${sqlIdentifier(entry.schema)}.${sqlIdentifier(entry.name)}`;
const rows = await query(`SELECT count(*) FROM ${from}`);
```

### Cell encodings

A cell is plain JSON **when that loses nothing** — `null`, booleans, strings, and numbers
that fit an IEEE double exactly. Anything else arrives as an object with an `encoding`
field, the same encoding the `ducklocal query` CLI prints, so no digit is silently dropped:

| Value | What arrives |
| --- | --- |
| `NULL` | `null` |
| `BOOLEAN` | `true` / `false` |
| integers within ±2^53−1 | number |
| larger integers (`HUGEINT`, `UBIGINT`, …) | `{ "encoding": "integer", "value": "170141183460469231731687303715884105727" }` |
| `DECIMAL` | `{ "encoding": "decimal", "value": "12345678901234567890.1234567890" }` |
| non-finite `FLOAT`/`DOUBLE` | `{ "encoding": "float", "value": "inf" }` |
| `DATE` | `{ "encoding": "date", "unit": "Day", "value": "19783" }` |
| `TIMESTAMP` | `{ "encoding": "timestamp", "unit": "Microsecond", "value": "1709296496789000" }` |
| `TIME` | `{ "encoding": "time", "unit": "Microsecond", "value": "45296789000" }` |
| `INTERVAL` | `{ "encoding": "interval", "months": 0, "days": 1, "nanos": "0" }` |
| `BLOB`, `GEOMETRY` | `{ "encoding": "hex", "value": "00ff" }` |
| `LIST` / `ARRAY` | array |
| `STRUCT` | `{ "encoding": "struct", "fields": [["name", <value>], …] }` |
| `MAP` | `{ "encoding": "map", "entries": [[<key>, <value>], …] }` |
| `UNION` | `{ "encoding": "union-value", "value": <value> }` |

`DATE` value `19783` is days since 1970-01-01; `TIMESTAMP` and `TIME` values are counts of
`unit` since the epoch or midnight. These are the storage values, not formatted text:
format them yourself, or `CAST` the column to `VARCHAR` in SQL if you want the string form.

## Components

An app draws with the same component catalog the shell ships, imported from
`gpui-component`: `GroupBox` (a titled card), `Progress` (a bar), `Toggle`, `Badge`, `Tag`,
`Alert`, `Empty`, `Collapsible`, `DescriptionList`, `DataTable` with `DataTableState`, the
`Table` family, `Sidebar`, `Resizable`, `Scroll`, `DescriptionList`, and the charts —
`BarChart`, `LineChart`, `AreaChart`, `PieChart`, `RadarChart`. Layout primitives
(`div`, `h_flex`, `v_flex`, `Button`) come from `gpui-kit` and `gpui-base`.

There is no `ToggleGroup`: compose a segmented control or a row of chips from several
`Toggle`s. The example app at
[`examples/analysis_app`](https://github.com/JetSquirrel/DuckLocal/tree/main/examples/analysis_app)
is a full dashboard built from exactly these, and it is the best thing to copy from.

Every app folder ships a `jsconfig.json` and a generated `gpui-kit.d.ts`, so check the
app against the API the runtime will actually give it before you save:

```sh
npx --yes -p typescript tsc -p <app-dir>/jsconfig.json --noImplicitAny false
```

That reports a property that does not exist on a catalog component — the kind of mistake
the runtime otherwise refuses only at render, inside the app's own error surface, which
is not written to the app log. Its blind spot is the `gpui-base` primitives (`div`,
`h_flex`, `v_flex`, `Button.new`): they are loosely typed, so a wrong method on one of
those is not caught here and still only shows up at render.

## SQL is not sandboxed

`query()` runs on DuckLocal's own database, **with DuckLocal's privileges and the user's**.
An app can read and write anything the SQL editor can, including `COPY` to files, `ATTACH`
of other databases, `INSTALL`/`LOAD` of extensions, and queries against S3 views whose
credentials are already configured. Treat an app's JavaScript as code you are choosing to
run, exactly as you would a shell script.

So the choice is asked for. The first time a folder opens as an app — from the command line,
a drop, the picker, or a tab restored at launch — its tab says what the app's SQL can do and
waits: **View source** shows the entry file without running it, **Trust and run** runs it.
The answer is remembered per folder, so later launches and every reload after a save run
without asking again. `ducklocal export --html` runs the app you name on the command line
and does not ask.

What an app does **not** get is anything else the process could do:

- **No filesystem, network, process, or environment module.** `fs`, `net`, `process` and
  friends are not available to a script unless the host grants them; DuckLocal grants none.
- **No S3 credentials.** The S3 browser's keys live in DuckLocal's own Rust state and are
  used by DuckLocal's signing client; they are never written into the DuckDB connection, and
  the host module cannot reach them. An app can still run SQL that uses httpfs if the user's
  own connection is configured for it.
- **The host module is the whole surface.** Only the functions above cross into Rust.

## Export an app as HTML

`ducklocal export --html <folder>` runs an app once and writes what it asked the database into one self-contained HTML file, for sending an app's numbers to someone who does not have DuckLocal. The command, its options and its exit codes are in [the CLI guide](cli.md#export-an-app-as-a-static-html-file). The capture ends when the app has gone quiet, or at `--timeout` seconds (default 15), whichever comes first; in the deadline case the JSON's `stop_reason` is `deadline` and the report carries a visible warning that it may be incomplete.

The report is the app's data, not its interface. Only what `query()` returned is exported, in call order, one section per distinct statement — an identical statement run again collapses into its section, marked as run that many times — and every call is captured, including statements that are not `SELECT`, such as `ATTACH`. A `catalog()` call becomes an appendix of tables and columns rather than a numbered statement. A leading `-- title: …` comment on the statement's first line names its section ("Statement N — title"); the SQL itself shows verbatim. Each result shows as a table, with an inline bar chart above it when the result is a label and a number per row: exactly two columns, at most 25 rows, and a non-negative numeric second column (the first column labels the bars). Anything else is only a table.

What the report does **not** hold is anything the app drew: KPI cards, charts, and tables built from JavaScript constants never reach the file, because the export records queries, not the render — an app draws native components and their contents are decided while they are laid out, so nothing describes a chart's bars well enough to reproduce them. Constants a report must show can be sent through SQL instead: `SELECT * FROM (VALUES ('Q1', 120), ('Q2', 95)) AS t(quarter, total)`.

Because it is the loading state, a statement an app only runs on a click is not in the report, and neither is anything after a reload. The app's JavaScript runs with the same privileges as always: exporting is running the app, exactly as opening it in a tab is.

## Dashboard specs (.dash)

A `.dash` file is a dashboard declared as data — query and plot blocks, no JavaScript — for the common case of standard plots over saved queries. The file format and `ducklocal check` validation are in [the CLI guide](cli.md#check-a-dashboard-spec).

Open one like an app: choose **Open dashboard…** from the tab strip's `+` menu, name it on the command line (`ducklocal dashboard.dash`), or drag the file onto the window, and it opens as a dashboard tab beside your queries and apps. The tab runs the spec's queries on the window's own connection — so a dashboard sees connection-local state such as `TEMP` tables, and queues with the editor's queries — and renders each plot as one panel of a vertical stack whose dividers drag to resize. Only read-only statements run: a query that could write (DDL, DML, `COPY`, `ATTACH`, …) is refused before it reaches the connection, so opening a `.dash` file someone sent you cannot change your data. A plot whose query fails shows the reason in its own panel; the rest of the dashboard still draws. The toolbar's reload re-reads the file and re-runs everything, and a reload that fails validation never replaces a working dashboard — the reason appears above it instead. Open dashboards are remembered between launches, exactly like apps.

The tab is also an editor: its source view edits the file with syntax highlighting — the SQL in heredocs coloured as it is in the SQL editor — completion and diagnostics, and writes back with the save button or ⌘S — a save whose spec no longer validates keeps the last working dashboard up and says why. Outside the GUI, `ducklocal lsp` serves the same completion, diagnostics, hover and go-to-definition to any LSP-capable editor (see [the CLI guide](cli.md#edit-a-dashboard-spec-with-lsp)).

## Known limits

- **An app in a tab has no window-level overlay.** The shell's dialogs, sheets, toasts and
  tooltip layer are found only when the shell's own root view is the window's first view. In
  a tab DuckLocal's root holds that place — it has to, or DuckLocal's own dialogs would break — so
  `window.open_dialog`, `window.open_sheet` and `window.push_toast` throw a `TypeError`
  reading *needs a ShellRoot as the window's first view*, which surfaces the way any other
  error from your code does. A tooltip is the exception, and the one silent one: it is
  attached by a hover listener, which has nothing to throw into, so a tooltip in an app
  simply never appears. Build app UI that stays inside its own bounds — an expanded region
  rather than a modal.
- **Reload is implemented by DuckLocal, not by the shell's watcher.** gpui-shell's hot reload
  hangs off `runtime.watch` / `runtime.refresh`, which only work for a `ShellRoot` the runtime
  built itself; that path is not reachable from an embedding host, so DuckLocal watches the
  folder itself and reloads through `load_application` / `mount_application`. The consequence:
  reload watches `.js`/`.mjs` files only. Any other file an app reads is not watched.
- **A reload compiles on the UI thread.** The script runtime is not sendable, so mounting an
  app happens on the main thread; a very large module graph will make the window stutter
  while it loads. The first load is deferred until after the tab's first frame, so the tab
  appears immediately either way.
- **One connection for all apps, and no cancellation.** Apps do not block the main
  window, but they do block each other: they share one connection behind one lock, so a long
  query in one app delays every other app's. One connection each is not available — a
  host function is handed arguments, not a caller, so nothing at the moment `query()` runs
  says which app asked. Cancelling a query in flight is not exposed either; keep an app's
  statements bounded rather than counting on stopping one.
- **`row_count` is what came back, not what matched.** With `truncated: true`, the rows that
  would have followed are not available; aggregate in SQL rather than counting in JavaScript.
