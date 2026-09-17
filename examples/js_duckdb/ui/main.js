import { View, div } from "gpui-kit";
import { v_flex, Button } from "gpui-base";
import { queryDemo } from "duckdb-demo";

export default class DuckDBDemo extends View {
  init() {
    this.loading = false;
    this.rows = [];
    this.error = "";
  }

  query(cx) {
    if (this.loading) return;
    this.loading = true;
    this.error = "";
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        this.rows = await queryDemo();
      } catch (error) {
        this.error = `Query failed: ${String(error)}. Try again.`;
      } finally {
        this.loading = false;
        cx.notify();
      }
    });
  }

  render(cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .size_full()
      .p_4()
      .gap_3()
      .bg(colors.background)
      .text_color(colors.foreground)
      .child(div().text_lg().child("DuckDB from JavaScript"))
      .child("Edit ui/main.js and save to reload this view.")
      .child(
        Button.new("query-demo")
          .self_start()
          .px_3()
          .py_2()
          .border_1()
          .border_color(colors.border)
          .bg(colors.surface)
          .disabled(this.loading)
          .on_click((_event, cx) => this.query(cx))
          .child(this.loading ? "Querying…" : "Run query"),
      )
      .child(this.error || (this.rows.length ? "id | name" : "Run the query to see three real DuckDB rows."))
      .children(this.rows.map((row) => div().id(`row-${row.id}`).child(`${row.id} | ${row.name}`)));
  }
}
