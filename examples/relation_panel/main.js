import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import {
  Button,
  DataTable,
  DataTableState,
  ErrorAlert,
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
  GroupBox,
  Toggle,
} from "gpui-component";
import { catalog, panelDir, query, sqlIdentifier } from "ducklocal";

/** Rows one page of the table asks for. */
const LIMIT = 100;

/** A date, time or timestamp encoding as readable text.
 *
 *  The host hands over the storage value — days since the epoch, microseconds
 *  since the epoch or since midnight — because that is what loses no digits. A
 *  panel that shows 1786896000000 has not shown the timestamp, so this is where
 *  the value becomes a time. Timestamps print as UTC: the encoding carries the
 *  zone as metadata, and a browser that silently shifted every row into the
 *  viewer's zone would be inventing an offset the data did not state. */
function temporalText(encoding) {
  const raw = Number(encoding.value);
  if (!isFinite(raw)) return String(encoding.value);
  if (encoding.encoding === "date") {
    return new Date(raw * 86400000).toISOString().slice(0, 10);
  }
  if (encoding.encoding === "time") {
    const millis = raw / 1000;
    const pad = (n) => String(n).padStart(2, "0");
    return [
      pad(Math.floor(millis / 3600000)),
      pad(Math.floor((millis % 3600000) / 60000)),
      pad(Math.floor((millis % 60000) / 1000)),
    ].join(":");
  }
  const perUnit = {
    Second: 1000,
    Millisecond: 1,
    Microsecond: 0.001,
    Nanosecond: 0.000001,
  }[encoding.unit];
  if (perUnit === undefined) return String(encoding.value);
  return new Date(raw * perUnit).toISOString().replace("T", " ").slice(0, 19);
}

/** One cell as text. A cell that lost nothing is already a string, number or
 *  boolean; the rest arrive as the CLI's encodings, so this reads the value out
 *  of them rather than printing `[object Object]`. */
function cellText(value) {
  if (value === null || value === undefined) return "NULL";
  if (Array.isArray(value)) return JSON.stringify(value);
  if (typeof value === "object") {
    if (value.encoding === "date" || value.encoding === "time" || value.encoding === "timestamp") {
      return temporalText(value);
    }
    if ("value" in value) return String(value.value);
    return JSON.stringify(value);
  }
  return String(value);
}

/** The thrown value as a line of text. */
function describe(error) {
  return error && error.message ? String(error.message) : String(error);
}

export default class RelationPanel extends View {
  init(_props, cx) {
    this.directory = panelDir();
    this.loading = true;
    this.loaded = false;
    this.error = "";
    this.relations = [];
    /** The relation under the table, by its qualified name: two schemas can
     *  hold the same table and the picker has to keep them apart. */
    this.selected = "";
    this.columns = [];
    this.rows = [];
    this.tableState = null;
    this.result = null;
    this.total = null;
    this.load(cx);
  }

  /** Read the catalog, then page whichever relation was chosen. The first load
   *  prefers `amount` when the connection has it, and the first relation
   *  otherwise, so the panel says something about the connection it was opened
   *  in without being asked. */
  load(cx) {
    if (this.loading && this.loaded) return; // a load is already in flight
    this.loading = true;
    this.error = "";
    cx.notify();

    cx.spawn(async (cx) => {
      try {
        const entries = await catalog();
        this.relations = entries.map((entry) => ({
          name: entry.name,
          schema: entry.schema,
          kind: entry.kind,
          qualified: `${sqlIdentifier(entry.schema)}.${sqlIdentifier(entry.name)}`,
          label: entry.schema === "main" ? entry.name : `${entry.schema}.${entry.name}`,
          columns: entry.columns.map((column) => `${column.name} ${column.type}`),
        }));
        if (!this.relations.some((relation) => relation.qualified === this.selected)) {
          const preferred =
            this.relations.find((relation) => relation.name === "amount") || this.relations[0];
          this.selected = preferred ? preferred.qualified : "";
        }
        await this.fetchRows();
      } catch (error) {
        this.fail(error);
      } finally {
        this.loading = false;
        this.loaded = true;
        cx.notify();
      }
    });
  }

  /** Page the selected relation again, without re-reading the catalog. */
  refresh(cx) {
    this.loading = true;
    this.error = "";
    cx.notify();

    cx.spawn(async (cx) => {
      try {
        await this.fetchRows();
      } catch (error) {
        this.fail(error);
      } finally {
        this.loading = false;
        cx.notify();
      }
    });
  }

  async fetchRows() {
    const relation = this.relation();
    if (!relation) {
      this.columns = [];
      this.rows = [];
      this.tableState = null;
      this.result = null;
      this.total = null;
      return;
    }

    const page = await query(`SELECT * FROM ${relation.qualified} LIMIT ${LIMIT}`, LIMIT);
    this.columns = page.columns.map((column) => column.name);
    // Keyed by column name, the way the table's own cell path expects; the
    // row's array rides along as `cells` so a repeated column name still has
    // somewhere to live.
    this.rows = page.rows.map((row) => {
      const keyed = { cells: row };
      this.columns.forEach((name, ix) => {
        if (!Object.prototype.hasOwnProperty.call(keyed, name)) keyed[name] = row[ix];
      });
      return keyed;
    });
    this.tableState = DataTableState(this.columns.slice());
    this.result = {
      rows: page.row_count,
      truncated: page.truncated,
      elapsed: page.elapsed_ms,
    };

    const counted = await query(`SELECT count(*) AS n FROM ${relation.qualified}`, 1);
    this.total = counted.rows.length ? cellText(counted.rows[0][0]) : null;
  }

  fail(error) {
    this.error = describe(error);
    this.columns = [];
    this.rows = [];
    this.tableState = null;
    this.result = null;
    this.total = null;
  }

  relation() {
    return this.relations.find((relation) => relation.qualified === this.selected) || null;
  }

  render(cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .id("relation-panel")
      .size_full()
      .overflow_y_scroll()
      .p_3()
      .gap_3()
      .bg(colors.background)
      .text_color(colors.foreground)
      .child(this.renderHeader(cx))
      .child(this.renderBody(cx));
  }

  renderHeader(cx) {
    const colors = cx.theme().colors;
    const relation = this.relation();
    const result = this.result;
    const summary = result
      ? `${result.rows} rows returned${result.truncated ? " (truncated)" : ""} · ${this.total ?? "…"} rows total · ${result.elapsed} ms`
      : this.loading
        ? "Reading…"
        : "No relation selected";

    return v_flex()
      .w_full()
      .flex_none()
      .gap_2()
      .child(
        h_flex()
          .w_full()
          .items_center()
          .justify_between()
          .gap_2()
          .child(
            v_flex()
              .gap_1()
              .child(div().text_lg().font_semibold().child("Relation browser"))
              .child(
                div()
                  .text_xs()
                  .text_color(colors.muted_foreground)
                  .child(`${summary} · ${this.directory}`),
              ),
          )
          .child(
            new Button("relation-refresh")
              .label(this.loading ? "Refreshing…" : "Refresh")
              .outline()
              .size("small")
              .disabled(this.loading)
              .on_click((_event, cx) => this.refresh(cx)),
          ),
      )
      .when(this.relations.length > 1, (row) =>
        row.child(
          h_flex()
            .w_full()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(div().text_xs().text_color(colors.muted_foreground).child("Relation"))
            .children(
              this.relations.map((entry) =>
                new Toggle(`relation-${entry.name}`)
                  .label(entry.label)
                  .outline()
                  .size("small")
                  .checked(this.selected === entry.qualified)
                  .on_change((_checked, cx) => {
                    this.selected = entry.qualified;
                    this.refresh(cx);
                  }),
              ),
            ),
        ),
      )
      .when(relation !== null, (column) =>
        column.child(
          div()
            .text_xs()
            .text_color(colors.muted_foreground)
            .child(
              relation === null
                ? ""
                : `${relation.kind} · ${relation.columns.join(" · ")}`,
            ),
        ),
      );
  }

  renderBody(cx) {
    const colors = cx.theme().colors;
    if (this.error) {
      return new ErrorAlert("relation-error", this.error).title("Query failed").visible(true);
    }
    if (this.loading && !this.result) {
      return v_flex()
        .items_center()
        .justify_center()
        .p_6()
        .child(div().text_sm().text_color(colors.muted_foreground).child("Reading…"));
    }
    if (!this.relation()) {
      return v_flex()
        .items_center()
        .justify_center()
        .p_6()
        .child(
          new Empty().child(
            new EmptyHeader()
              .child(new EmptyMedia())
              .child(new EmptyTitle().child("This connection has no relations"))
              .child(
                new EmptyDescription().child(
                  "Open a data file or database in the main window first; this panel queries that connection.",
                ),
              ),
          ),
        );
    }
    if (!this.rows.length) {
      return v_flex()
        .items_center()
        .justify_center()
        .p_6()
        .child(div().text_sm().text_color(colors.muted_foreground).child("This relation has no rows"));
    }
    return new GroupBox()
      .variant("outline")
      .title(`${this.columns.length} columns × ${this.rows.length} rows (up to ${LIMIT} rows)`)
      .child(
        new DataTable(
          this.tableState,
          () => this.rows,
          (row, column, cx) => this.renderCell(row, column, cx),
        )
          .h_96()
          .w_full()
          .stripe(true)
          .bordered(false)
          .sortable(false),
      );
  }

  /** Cells arrive keyed by column name; a repeated name falls back to the
   *  row's own array, so the second copy of a column is not lost. */
  renderCell(row, column, cx) {
    const colors = cx.theme().colors;
    const keyed = row[column];
    const value = keyed === undefined ? row.cells[this.columns.indexOf(column)] : keyed;
    return div()
      .text_sm()
      .text_color(value === null || value === undefined ? colors.muted_foreground : colors.foreground)
      .child(cellText(value));
  }
}
