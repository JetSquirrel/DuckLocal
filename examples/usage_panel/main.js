import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import {
  BarChart,
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
  Progress,
  Resizable,
  ResizablePanel,
} from "gpui-component";
import { catalog, appDir, query, sqlIdentifier } from "ducklocal";

/** The relations this app is about. A usage export and its cost export are
 *  named after the range they cover, so the app looks them up by prefix and
 *  takes the latest rather than being pinned to one dated pair. */
const USAGE_PREFIX = "amount-";
const COST_PREFIX = "cost-";

/** Rows a supporting query may return. The aggregates are one row per day or
 *  per (model, type); this only has to be above any real answer. */
const LIMIT = 500;

const TOKEN_TYPES = [
  { key: "input_cache_hit_tokens", label: "Input (cache hit)" },
  { key: "input_cache_miss_tokens", label: "Input (cache miss)" },
  { key: "output_tokens", label: "Output" },
  { key: "request_count", label: "Requests" },
];

/** A number from a cell that may be plain JSON, a string, or one of the host's
 *  encodings. Aggregates are DECIMAL and BIGINT, so this is the normal case
 *  rather than an edge. */
function toNumber(value) {
  if (value === null || value === undefined) return 0;
  if (typeof value === "number") return isFinite(value) ? value : 0;
  if (typeof value === "string") {
    const parsed = Number(value);
    return isFinite(parsed) ? parsed : 0;
  }
  if (typeof value === "object" && "value" in value) return toNumber(value.value);
  return 0;
}

/** Days since 1970-01-01, the form a DATE crosses the boundary in, as
 *  `YYYY-MM-DD`. */
function dayText(value) {
  if (value === null || value === undefined) return "—";
  if (typeof value === "object" && "value" in value) {
    return dayText(value.value);
  }
  const days = Number(value);
  if (!isFinite(days)) return String(value);
  return new Date(days * 86400000).toISOString().slice(0, 10);
}

/** A count with thousands separators: these are token counts, and 1163542912
 *  is not readable at a glance. */
function grouped(value) {
  const rounded = Math.round(toNumber(value));
  return String(rounded).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

/** Big numbers as B / M / K, which is how the size of a token count is read. */
function compact(value) {
  const n = toNumber(value);
  const abs = Math.abs(n);
  if (abs >= 1000000000) return `${(n / 1000000000).toFixed(2)} B`;
  if (abs >= 1000000) return `${(n / 1000000).toFixed(2)} M`;
  if (abs >= 1000) return `${(n / 1000).toFixed(1)} K`;
  return grouped(n);
}

function money(value) {
  const n = toNumber(value);
  return n.toFixed(Math.abs(n) >= 100 ? 2 : 4);
}

function describe(error) {
  return error && error.message ? String(error.message) : String(error);
}

export default class UsagePanel extends View {
  init(_props, cx) {
    this.directory = appDir();
    this.loading = true;
    this.loaded = false;
    this.error = "";
    /** The relations actually read, so the header can name them: a dashboard
     *  that hides which range it is showing is worse than one that says it. */
    this.sources = { usage: "", cost: "" };
    this.types = [];
    this.models = [];
    this.modelTable = null;
    this.dailyTokens = [];
    this.dailyCost = [];
    this.totalCost = 0;
    this.currency = "";
    this.load(cx);
  }

  load(cx) {
    if (this.loading && this.loaded) return; // a load is already in flight
    this.loading = true;
    this.error = "";
    cx.notify();

    cx.spawn(async (cx) => {
      try {
        await this.fetchAll();
      } catch (error) {
        this.error = describe(error);
      } finally {
        this.loading = false;
        this.loaded = true;
        cx.notify();
      }
    });
  }

  async fetchAll() {
    const entries = await catalog();
    const usage = latest(entries, USAGE_PREFIX);
    const cost = latest(entries, COST_PREFIX);
    if (!usage || !cost) {
      this.sources = { usage: "", cost: "" };
      this.types = [];
      this.models = [];
      this.modelTable = null;
      this.dailyTokens = [];
      this.dailyCost = [];
      this.totalCost = 0;
      this.currency = "";
      return;
    }
    this.sources = { usage: `${usage.schema}.${usage.name}`, cost: `${cost.schema}.${cost.name}` };
    const usage_rel = `${sqlIdentifier(usage.schema)}.${sqlIdentifier(usage.name)}`;
    const cost_rel = `${sqlIdentifier(cost.schema)}.${sqlIdentifier(cost.name)}`;

    // One query per question, each aggregated in SQL: the app never sums a
    // truncated page in JavaScript.
    const byType = await query(
      `SELECT type, sum(amount) AS total, count(*) AS rows FROM ${usage_rel} GROUP BY 1 ORDER BY 2 DESC`,
      LIMIT,
    );
    this.types = byType.rows.map((row) => ({
      type: String(row[0]),
      total: toNumber(row[1]),
      rows: toNumber(row[2]),
    }));

    const byModel = await query(
      `SELECT model, type, sum(amount) AS total FROM ${usage_rel} GROUP BY 1, 2 ORDER BY 1, 2`,
      LIMIT,
    );
    this.models = byModel.rows.map((row) => ({
      model: String(row[0]),
      type: String(row[1]),
      total: toNumber(row[2]),
    }));

    // Two steps rather than one: a table cell carries the model/type keys, and
    // the renderer reads them by name.
    this.modelTable = {
      state: DataTableState(["Model", "Type", "Total"]),
      rows: this.models.map((entry) => ({
        model: entry.model,
        type: labelOf(entry.type),
        total: compact(entry.total),
      })),
    };

    // `CAST(CAST(... AS VARCHAR) AS DATE)`: this DuckDB build cannot cast a
    // TIMESTAMP WITH TIME ZONE straight to DATE, and the export's column is
    // exactly that type.
    const dailyTokens = await query(
      `SELECT CAST(CAST(start_time_iso AS VARCHAR) AS DATE) AS day, sum(amount) AS tokens
         FROM ${usage_rel} WHERE type = 'output_tokens' GROUP BY 1 ORDER BY 1`,
      LIMIT,
    );
    this.dailyTokens = dailyTokens.rows.map((row) => ({
      label: dayText(row[0]),
      value: toNumber(row[1]),
    }));

    const dailyCost = await query(
      `SELECT CAST(CAST(start_time_iso AS VARCHAR) AS DATE) AS day, sum(cost) AS cost
         FROM ${cost_rel} GROUP BY 1 ORDER BY 1`,
      LIMIT,
    );
    this.dailyCost = dailyCost.rows.map((row) => ({
      label: dayText(row[0]),
      value: toNumber(row[1]),
    }));

    const total = await query(
      `SELECT sum(cost) AS total, any_value(currency) AS currency FROM ${cost_rel}`,
      1,
    );
    this.totalCost = total.rows.length ? toNumber(total.rows[0][0]) : 0;
    this.currency = total.rows.length && total.rows[0][1] ? String(total.rows[0][1]) : "";
  }

  /** Usage of one `type`, summed across models. */
  typeTotal(type) {
    return this.types
      .filter((entry) => entry.type === type)
      .reduce((sum, entry) => sum + entry.total, 0);
  }

  inputTokens() {
    return this.typeTotal("input_cache_hit_tokens") + this.typeTotal("input_cache_miss_tokens");
  }

  render(cx) {
    const colors = cx.theme().colors;
    // No scroll at the root: the sections below are resizable panels, and a
    // resizable group needs a bounded height. A section whose content
    // overflows its panel scrolls inside the panel instead.
    return v_flex()
      .id("usage-panel")
      .size_full()
      .p_3()
      .gap_3()
      .bg(colors.background)
      .text_color(colors.foreground)
      .child(this.renderHeader(cx))
      .child(div().w_full().flex_1().min_h_0().child(this.renderBody(cx)));
  }

  renderHeader(cx) {
    const colors = cx.theme().colors;
    const source = this.sources.usage
      ? `${this.sources.usage} + ${this.sources.cost}`
      : `Waiting for ${USAGE_PREFIX}* and ${COST_PREFIX}*`;
    return v_flex()
      .w_full()
      .flex_none()
      .gap_1()
      .child(
        h_flex()
          .w_full()
          .items_center()
          .justify_between()
          .gap_2()
          .child(div().text_lg().font_semibold().child("Usage and cost"))
          .child(
            new Button("usage-refresh")
              .label(this.loading ? "Refreshing…" : "Refresh")
              .outline()
              .size("small")
              .disabled(this.loading)
              .on_click((_event, cx) => this.load(cx)),
          ),
      )
      .child(div().text_xs().text_color(colors.muted_foreground).child(source))
      .child(
        div()
          .text_xs()
          .text_color(colors.muted_foreground)
          .child(`${this.directory} · the numbers are aggregated in SQL; the app only presents them`),
      );
  }

  renderBody(cx) {
    const colors = cx.theme().colors;
    if (this.error) {
      return new ErrorAlert("usage-error", this.error).title("Could not read the data").visible(true);
    }
    if (this.loading && !this.loaded) {
      return v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_6()
        .child(div().text_sm().text_color(colors.muted_foreground).child("Reading…"));
    }
    if (!this.sources.usage) {
      return v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_6()
        .child(
          new Empty().child(
            new EmptyHeader()
              .child(new EmptyMedia())
              .child(new EmptyTitle().child("No usage or cost export found"))
              .child(
                new EmptyDescription().child(
                  `This app reads relations in the current connection whose names start with ${USAGE_PREFIX} and ${COST_PREFIX}; open those two exports in the main window first.`,
                ),
              ),
          ),
        );
    }
    // Each section is one panel of a vertical resizable stack: drag a divider
    // to give a section more of the window. The last panel takes what is left.
    const scrollable = (content) => div().size_full().overflow_y_scroll().child(content);
    return new Resizable("usage-sections")
      .axis("vertical")
      .child(
        new ResizablePanel()
          .size(120)
          .size_range(96, 200)
          .child(scrollable(this.renderKpis(cx))),
      )
      .child(
        new ResizablePanel()
          .size(280)
          .size_range(180, 560)
          .child(this.renderDaily(cx)),
      )
      .child(
        new ResizablePanel()
          .size(220)
          .size_range(140, 480)
          .child(scrollable(this.renderTypes(cx))),
      )
      .child(new ResizablePanel().child(this.renderModels(cx)));
  }

  renderKpis(cx) {
    const colors = cx.theme().colors;
    const cards = [
      { id: "requests", label: "Requests", value: grouped(this.typeTotal("request_count")), unit: "requests" },
      { id: "input", label: "Input tokens", value: compact(this.inputTokens()), unit: "including cache hits" },
      { id: "output", label: "Output tokens", value: compact(this.typeTotal("output_tokens")), unit: "generated" },
      {
        id: "cost",
        label: "Cost",
        value: money(this.totalCost),
        unit: this.currency || "—",
      },
    ];
    return h_flex()
      .w_full()
      .flex_none()
      .gap_3()
      .children(
        cards.map((card) =>
          new GroupBox()
            .variant("outline")
            .flex_1()
            .min_w_0()
            .title(card.label)
            .child(
              h_flex()
                .items_center()
                .gap_2()
                .child(div().text_xl().font_semibold().child(card.value))
                .child(div().text_xs().text_color(colors.muted_foreground).child(card.unit)),
            ),
        ),
      );
  }

  renderDaily(cx) {
    // The two daily charts split horizontally with their own draggable
    // divider — a resizable group nested in one panel of the outer one. Each
    // chart fills the panel the divider leaves it instead of owning a fixed
    // height.
    const chart = (title, bars) =>
      new GroupBox()
        .variant("outline")
        .size_full()
        .title(title)
        .child(
          bars.length
            ? new BarChart(() => bars)
                .size_full()
                .grid(true)
                .label_axis(true)
                .value_axis(true)
            : this.nothingYet(cx),
        );
    return new Resizable("usage-daily")
      .axis("horizontal")
      .child(
        new ResizablePanel()
          .size_range(240, 720)
          .child(chart(`Daily cost (${this.currency || "—"})`, this.dailyCost)),
      )
      .child(new ResizablePanel().child(chart("Daily output tokens", this.dailyTokens)));
  }

  renderTypes(cx) {
    const colors = cx.theme().colors;
    const total = this.types.reduce((sum, entry) => sum + entry.total, 0) || 1;
    return new GroupBox()
      .variant("outline")
      .title("Usage by type")
      .child(
        v_flex()
          .w_full()
          .gap_2()
          .children(
            this.types.map((entry) =>
              v_flex()
                .id(`usage-type-${entry.type}`)
                .w_full()
                .gap_1()
                .child(
                  h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().child(labelOf(entry.type)))
                    .child(
                      h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                          div()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child(`${entry.rows} rows`),
                        )
                        .child(div().text_sm().child(grouped(entry.total))),
                    ),
                )
                .child(
                  // A share of the total is meaningful within one type, so the
                  // bar shows that type's weight in the export rather than
                  // mixing tokens with request counts on one scale.
                  new Progress(`usage-progress-${entry.type}`)
                    .value(Math.round((entry.total / total) * 1000) / 10)
                    .size("small"),
                ),
            ),
          ),
      );
  }

  renderModels(cx) {
    const table = this.modelTable;
    if (!table) return div();
    return new GroupBox()
      .variant("outline")
      .size_full()
      .title(`By model and type (${this.models.length} rows)`)
      .child(
        new DataTable(
          table.state,
          () => table.rows,
          (row, column, cx) => this.renderModelCell(row, column, cx),
        )
          .size_full()
          .stripe(true)
          .bordered(false)
          .sortable(false),
      );
  }

  /** Cells arrive by column label; the row keeps its own keys, so the label is
   *  mapped here rather than guessed by the table. */
  renderModelCell(row, column, cx) {
    const colors = cx.theme().colors;
    const text =
      column === "Model"
        ? row.model
        : column === "Type"
          ? row.type
          : row.total;
    return div()
      .text_sm()
      .text_color(column === "Type" ? colors.muted_foreground : colors.foreground)
      .child(String(text ?? ""));
  }

  nothingYet(cx) {
    return div()
      .text_xs()
      .text_color(cx.theme().colors.muted_foreground)
      .child("This export has no per-day data");
  }
}

/** The export whose name starts with `prefix`, latest by name. The names carry
 *  their range, so the newest export sorts last. */
function latest(entries, prefix) {
  return entries
    .filter((entry) => entry.name.startsWith(prefix))
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
    .at(-1) || null;
}

function labelOf(type) {
  const known = TOKEN_TYPES.find((entry) => entry.key === type);
  return known ? known.label : String(type);
}
