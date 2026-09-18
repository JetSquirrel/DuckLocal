// A channel-analysis dashboard: what an agent leaves behind for a panel tab.
//
// Open it with the tab strip's `+` → "Open panel…", or on the command line
// with `ducklocal examples/analysis_app`, or by dropping the folder on the
// window. Closing the tab closes it; it comes back on the next launch.
//
// It reads its own `orders.csv` rather than whatever the user has open, so it
// works on a machine that has never seen this data: `panelDir()` names the
// folder the panel was loaded from, and `sqlLiteral()` quotes it.
//
// `catalog()` and `query()` run on the app's own DuckDB connection, so a panel
// can do anything the SQL editor can — reads and writes alike.

import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import {
  BarChart,
  Button,
  Collapsible,
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
  Toggle,
} from "gpui-component";
import { panelDir, query, sqlLiteral } from "ducklocal";

/** Rows the panel asks for; the host caps this at its own maximum. */
const ROW_LIMIT = 5000;

/** The time ranges the segmented control offers, in days. */
const RANGES = [7, 14];

/** Chinese market convention: up is red, down is cyan, and the sign is always
 *  shown so colour is never the only signal. The token palette has one negative
 *  role (`destructive`) and no dedicated "falling" role, so "down" uses the
 *  theme's cool `info` role — a theme token, not a hue invented here, so a
 *  custom theme still decides what falling looks like. */
function deltaColors(change, colors) {
  return change >= 0 ? colors.destructive : colors.info;
}

function arrow(change) {
  if (change > 0) return "▲";
  if (change < 0) return "▼";
  return "–";
}

/** Percent change from `previous` to `current`, or `null` when the previous
 *  period gives no baseline. A missing value arrives as 0, and a percent of
 *  nothing is not a number worth printing. */
function percentChange(current, previous) {
  const base = toNumber(previous);
  if (!isFinite(base) || base === 0) return null;
  return ((toNumber(current) - base) / base) * 100;
}

/** `+12.4%` / `-3.0%`, with the sign always visible. */
function signedPercent(value) {
  const rounded = Math.round(value * 10) / 10;
  return (rounded < 0 ? "-" : "+") + Math.abs(rounded).toFixed(1) + "%";
}

function grouped(value) {
  return String(Math.round(value)).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

/** Revenue as K or M. The exports are in units of one, and a bare eight-digit
 *  integer is not readable at a glance. */
function money(value) {
  const amount = toNumber(value);
  const abs = Math.abs(amount);
  if (abs >= 1000000) return (amount / 1000000).toFixed(2) + " M";
  if (abs >= 1000) return (amount / 1000).toFixed(1) + " K";
  return grouped(amount);
}

/** A number out of a cell, plain or encoded.
 *
 * The host sends plain JSON only when that loses nothing, so a DECIMAL or a
 * wider-than-double integer arrives as `{ encoding, value }`. Reading one with
 * `Number()` alone is how a dashboard quietly shows zeroes. */
function toNumber(value) {
  if (typeof value === "number") return value;
  if (value === null || value === undefined) return 0;
  if (typeof value === "object" && value.value !== undefined) {
    return Number(value.value) || 0;
  }
  return Number(value) || 0;
}

function share(part, whole) {
  if (!whole) return 0;
  return Math.round((part / whole) * 1000) / 10;
}

export default class ChannelDashboard extends View {
  init(_props, cx) {
    // `panelDir()` is answered while the panel loads, which is why it is read
    // here and kept: outside `init` the host has no panel to name.
    this.directory = panelDir();
    this.dataFile = this.directory + "/orders.csv";

    this.range = 14;
    this.selected = null; // filled from the first load: every channel
    this.channels = [];
    this.kpis = null;
    this.composition = [];
    this.trend = [];
    this.regions = [];
    this.source = "";
    this.loading = true;
    this.loaded = false;
    this.error = "";
    this.definitionOpen = false;

    this.regionTable = DataTableState(["Region", "Orders", "Revenue", "Share"]);
    this.load(cx);
  }

  /** Run every query the dashboard needs for the current range and channels. */
  load(cx) {
    if (this.loading && this.loaded) {
      return; // a refresh is already in flight
    }
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
    // The window is the last N days of the data itself, so the panel works
    // whatever the file holds — and it is computed in SQL, not by guessing.
    // `CAST(... AS VARCHAR)`: a DATE crosses as { encoding: "date", value:
    // "20546" }, and the string form is what this panel wants. The comparison
    // against the cutoff stays a DATE comparison, in SQL.
    const window = await this.rows(
      `SELECT CAST(max(date) AS VARCHAR) AS last_day FROM ${this.file()}`,
      ROW_LIMIT,
    );
    if (!window.length) {
      this.kpis = null;
      this.composition = [];
      this.trend = [];
      this.regions = [];
      this.source = "";
      return;
    }
    const lastDay = String(window[0][0]);

    // Every channel the file has, so the chips come from the data.
    const all = await this.rows(
      `SELECT channel, count(*) FROM ${this.file()} GROUP BY channel ORDER BY 2 DESC`,
      ROW_LIMIT,
    );
    this.channels = all.map((row) => String(row[0]));
    if (this.selected === null) {
      this.selected = this.channels.slice();
    } else {
      this.selected = this.selected.filter((name) => this.channels.indexOf(name) >= 0);
    }
    const cutoff = dayString(lastDay, this.range);
    const previous = dayString(lastDay, this.range * 2);
    const channelFilter = this.channelFilter();
    this.source = this.describeSource(cutoff, previous, channelFilter);

    const totals = await this.rows(
      `SELECT
         sum(CASE WHEN date > ${sqlLiteral(cutoff)} THEN revenue ELSE 0 END) AS revenue,
         sum(CASE WHEN date > ${sqlLiteral(cutoff)} THEN orders ELSE 0 END) AS orders,
         sum(CASE WHEN date <= ${sqlLiteral(cutoff)} AND date > ${sqlLiteral(previous)}
                  THEN revenue ELSE 0 END) AS prev_revenue,
         sum(CASE WHEN date <= ${sqlLiteral(cutoff)} AND date > ${sqlLiteral(previous)}
                  THEN orders ELSE 0 END) AS prev_orders
       FROM ${this.file()} WHERE ${channelFilter}`,
      ROW_LIMIT,
    );
    this.kpis = totals.length ? totals[0] : null;

    const channels = await this.rows(
      `SELECT channel, sum(orders) AS orders, sum(revenue) AS revenue
       FROM ${this.file()}
       WHERE date > ${sqlLiteral(cutoff)} AND ${channelFilter}
       GROUP BY 1 ORDER BY 3 DESC`,
      ROW_LIMIT,
    );
    const total = channels.reduce((sum, row) => sum + toNumber(row[2]), 0);
    this.composition = channels.map((row) => ({
      channel: String(row[0]),
      orders: toNumber(row[1]),
      revenue: toNumber(row[2]),
      share: share(toNumber(row[2]), total),
    }));

    const daily = await this.rows(
      `SELECT CAST(date AS VARCHAR) AS day, sum(revenue) AS revenue
       FROM ${this.file()}
       WHERE date > ${sqlLiteral(cutoff)} AND ${channelFilter}
       GROUP BY 1 ORDER BY 1`,
      ROW_LIMIT,
    );
    // Plotted in thousands of yuan so the value axis reads `1014.4` instead of a
    // full `10143576`; the section's unit line names the unit.
    this.trend = daily.map((row) => ({
      label: String(row[0]).slice(5),
      value: Math.round(toNumber(row[1]) / 100) / 10,
    }));

    const regions = await this.rows(
      `SELECT region, sum(orders) AS orders, sum(revenue) AS revenue
       FROM ${this.file()}
       WHERE date > ${sqlLiteral(cutoff)} AND ${channelFilter}
       GROUP BY 1 ORDER BY 3 DESC`,
      ROW_LIMIT,
    );
    const regionTotal = regions.reduce((sum, row) => sum + toNumber(row[2]), 0);
    this.regions = regions.map((row) => ({
      region: String(row[0]),
      orders: toNumber(row[1]),
      revenue: toNumber(row[2]),
      share: share(toNumber(row[2]), regionTotal),
    }));
  }

  file() {
    return sqlLiteral(this.dataFile);
  }

  channelFilter() {
    if (!this.selected || !this.selected.length) {
      // Nothing selected is nothing to show, which is a state, not a query
      // that quietly returns everything.
      return "FALSE";
    }
    return (
      "channel IN (" + this.selected.map((name) => sqlLiteral(name)).join(", ") + ")"
    );
  }

  describeSource(cutoff, previous, channelFilter) {
    return [
      "SELECT … FROM " + this.file(),
      "WHERE date > " + sqlLiteral(cutoff),
      "  AND " + channelFilter,
      "-- comparison window " + previous + " … " + cutoff,
    ].join("\n");
  }

  async rows(sql, limit) {
    const result = await query(sql, limit);
    return result.rows;
  }

  toggleChannel(name, cx) {
    const at = this.selected.indexOf(name);
    if (at >= 0) {
      this.selected = this.selected.filter((entry) => entry !== name);
    } else {
      this.selected = this.selected.concat([name]);
    }
    this.load(cx);
  }

  setRange(days, cx) {
    if (this.range === days) {
      return;
    }
    this.range = days;
    this.load(cx);
  }

  render(cx) {
    if (!this.loaded && this.loading) {
      return this.renderLoading(cx);
    }
    if (this.error) {
      return this.renderFailure(cx);
    }
    if (!this.kpis) {
      return this.renderEmpty(cx);
    }
    return this.renderDashboard(cx);
  }

  renderLoading(cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .size_full()
      .items_center()
      .justify_center()
      .gap_2()
      .child(div().text_sm().text_color(colors.muted_foreground).child("Reading the data…"));
  }

  renderFailure(cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .size_full()
      .gap_2()
      .p_4()
      .child(
        new ErrorAlert("panel-error", this.error)
          .title("Could not load the panel's data")
          .visible(true),
      )
      .child(
        div()
          .text_xs()
          .text_color(colors.muted_foreground)
          .child("Data file: " + this.dataFile),
      );
  }

  renderEmpty(cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .size_full()
      .items_center()
      .justify_center()
      .p_6()
      .child(
        new Empty().child(
          new EmptyHeader()
            .child(new EmptyMedia())
            .child(new EmptyTitle().child("No data in this range"))
            .child(
              new EmptyDescription().child(
                "Try another time range or channel; this panel reads the orders.csv in its own folder.",
              ),
            ),
        ),
      )
      .child(
        div()
          .text_xs()
          .text_color(colors.muted_foreground)
          .child(this.directory),
      );
  }

  renderDashboard(cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .id("channel-dashboard")
      .size_full()
      .overflow_y_scroll()
      .p_3()
      .gap_3()
      .bg(colors.background)
      .text_color(colors.foreground)
      .child(this.renderHeader(cx))
      .child(this.renderKpis(cx))
      .child(this.renderComposition(cx))
      .child(this.renderTrend(cx))
      .child(this.renderRegions(cx))
      .child(this.renderDefinition(cx));
  }

  renderHeader(cx) {
    const colors = cx.theme().colors;
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
              .child(div().text_lg().font_semibold().child("Channel dashboard"))
              .child(
                div()
                  .text_xs()
                  .text_color(colors.muted_foreground)
                  .child(
                    `Last ${this.range} days · ${this.selected.length}/${this.channels.length} channels · ${this.directory}`,
                  ),
              ),
          )
          .child(
            new Button("refresh")
              .label(this.loading ? "Refreshing…" : "Refresh")
              .ghost()
              .size("small")
              .disabled(this.loading)
              .on_click((_event, cx) => this.load(cx)),
          ),
      )
      .child(
        h_flex()
          .w_full()
          .items_center()
          .gap_2()
          .child(
            div()
              .text_xs()
              .text_color(colors.muted_foreground)
              .child("Time range"),
          )
          .children(
            // Outlined, like the channel chips below: the selected control
            // fills with the accent, so the two rows read as one control
            // language and the state is visible without leaning on colour.
            RANGES.map((days) =>
              new Toggle(`range-${days}`)
                .label(`Last ${days} days`)
                .checked(this.range === days)
                .outline()
                .on_change((_checked, cx) => this.setRange(days, cx)),
            ),
          ),
      )
      .child(
        h_flex()
          .w_full()
          .flex_wrap()
          .items_center()
          .gap_2()
          .child(
            div()
              .text_xs()
              .text_color(colors.muted_foreground)
              .child("Channel"),
          )
          .children(
            this.channels.map((name) =>
              new Toggle(`channel-${name}`)
                .label(name)
                .checked(this.selected.indexOf(name) >= 0)
                .outline()
                .on_change((_checked, cx) => this.toggleChannel(name, cx)),
            ),
          ),
      );
  }

  renderKpis(cx) {
    const colors = cx.theme().colors;
    const kpis = this.kpis;
    const revenue = toNumber(kpis[0]);
    const orders = toNumber(kpis[1]);
    const previousRevenue = toNumber(kpis[2]);
    const previousOrders = toNumber(kpis[3]);
    const aov = orders ? revenue / orders : 0;
    const previousAov = previousOrders ? previousRevenue / previousOrders : 0;
    const best = this.composition.length ? this.composition[0].channel : "—";

    const cards = [
      { id: "revenue", label: "Revenue", value: money(revenue), current: revenue, previous: previousRevenue },
      { id: "orders", label: "Orders", value: grouped(orders), current: orders, previous: previousOrders },
      { id: "aov", label: "Average order value", value: money(aov), current: aov, previous: previousAov },
      { id: "best", label: "Top channel by revenue", value: best, current: null, previous: null },
    ];

    return h_flex()
      .w_full()
      .flex_none()
      .gap_3()
      .children(
        cards.map((card) => {
          // The best channel has no previous period to compare against, so it
          // carries no percentage — only the period it belongs to.
          const change =
            card.current === null ? null : percentChange(card.current, card.previous);
          const comparison =
            change === null
              ? div()
                  .text_xs()
                  .text_color(colors.muted_foreground)
                  .child("This period")
              : h_flex()
                  .gap_1()
                  .items_center()
                  .child(
                    div()
                      .text_xs()
                      .text_color(deltaColors(change, colors))
                      .child(`${arrow(change)} ${signedPercent(change)}`),
                  )
                  .child(
                    div()
                      .text_xs()
                      .text_color(colors.muted_foreground)
                      .child("vs. previous period"),
                  );
          // `flex_1` is what makes the four cards one equal-width row: a
          // GroupBox is `w_full` on its own, so without it each card claims the
          // whole strip. `variant("outline")` is the card surface — the default
          // variant draws none.
          return new GroupBox()
            .variant("outline")
            .flex_1()
            .min_w_0()
            .title(card.label)
            .child(
              v_flex()
                .gap_1()
                .child(div().text_xl().font_semibold().child(card.value))
                .child(comparison),
            );
        }),
      );
  }

  renderComposition(cx) {
    const colors = cx.theme().colors;
    return new GroupBox()
      .title("Channel mix")
      .child(
        v_flex()
          .w_full()
          .gap_2()
          .children(
            this.composition.map((entry) =>
              v_flex()
                .id(`composition-${entry.channel}`)
                .w_full()
                .gap_1()
                .child(
                  h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().child(entry.channel))
                    .child(
                      h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                          div()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child(`${grouped(entry.orders)} orders`),
                        )
                        .child(div().text_sm().child(money(entry.revenue)))
                        .child(
                          div()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child(`${entry.share}%`),
                        ),
                    ),
                )
                .child(
                  new Progress(`progress-${entry.channel}`)
                    .value(entry.share)
                    .size("small"),
                ),
            ),
          ),
      );
  }

  renderTrend(cx) {
    const colors = cx.theme().colors;
    return new GroupBox()
      .title(`Daily revenue (last ${this.range} days)`)
      .child(
        v_flex()
          .w_full()
          .gap_1()
          .child(
            div()
              .text_xs()
              .text_color(colors.muted_foreground)
              .child("Unit: thousand CNY"),
          )
          .child(
            new BarChart(() => this.trend)
              .h_48()
              .w_full()
              .grid(true)
              .label_axis(true)
              .value_axis(true),
          ),
      );
  }

  renderRegions(cx) {
    return new GroupBox().title("Regions").child(
      new DataTable(
        this.regionTable,
        () => this.regions,
        (row, column, cx) => renderRegionCell(row, column, cx),
      )
        .w_full()
        .h_48()
        .stripe(true)
        .bordered(true)
        .sortable(false),
    );
  }

  renderDefinition(cx) {
    const colors = cx.theme().colors;
    return new Collapsible()
      .open(this.definitionOpen)
      .motion_id("definition")
      .child(
        new Button("definition-toggle")
          .label(this.definitionOpen ? "▾ View definition" : "▸ View definition")
          .ghost()
          .size("xsmall")
          .on_click((_event, cx) => {
            this.definitionOpen = !this.definitionOpen;
            cx.notify();
          }),
      )
      .content(
        v_flex()
          .w_full()
          .gap_2()
          .p_3()
          .border_1()
          .border_color(colors.border)
          .child(
            div()
              .text_xs()
              .text_color(colors.muted_foreground)
              .child("Panel folder: " + this.directory),
          )
          .child(
            div()
              .text_xs()
              .text_color(colors.muted_foreground)
              .child("The panel itself is a gpui-shell View in main.js; below is the SQL this load used."),
          )
          .child(div().text_xs().font_family("monospace").child(this.source)),
      );
  }
}

/** One cell of the region table. The share column carries its own bar, so the
 *  number and the picture are the same fact. */
function renderRegionCell(row, column, cx) {
  const colors = cx.theme().colors;
  const region = String(row.region);
  switch (column) {
    case "Orders":
      return div().text_sm().child(grouped(row.orders));
    case "Revenue":
      return div().text_sm().child(money(row.revenue));
    case "Share":
      return v_flex()
        .w_full()
        .gap_1()
        .child(div().text_xs().text_color(colors.muted_foreground).child(`${row.share}%`))
        .child(
          new Progress(`region-share-${region}`).value(row.share).size("xsmall"),
        );
    default:
      return div().text_sm().child(region);
  }
}

/** `YYYY-MM-DD` shifted back by `days`, done on the string the data gave back
 *  so the panel does not need a date library. */
function dayString(lastDay, days) {
  const parts = String(lastDay).split("-");
  const date = new Date(Date.UTC(Number(parts[0]), Number(parts[1]) - 1, Number(parts[2])));
  date.setUTCDate(date.getUTCDate() - days);
  return date.toISOString().slice(0, 10);
}

function describe(error) {
  return error && error.message ? String(error.message) : String(error);
}
