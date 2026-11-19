//! A dashboard as one HTML file: what its tab shows, to send or publish.
//!
//! The page is drawn from the plots the view has already prepared, so it is
//! the dashboard as it stands — the picks in force, the rows each query
//! returned — not a fresh run. It is one file with nothing to fetch: charts
//! are inline SVG, style is inline, and no script is needed to read it, so
//! it opens the same from a disk, an email attachment or a static host.
//!
//! What the window draws with a canvas of its own (a `map`, a `timeline`)
//! is written as its query's rows instead, and the page says so.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use super::format::{full_text, short_text, NumberFormat};
use super::prepare::{Mood, PreparedPlot};
use crate::i18n::tr;
use crate::query::{ColumnKind, QueryResult};

/// The chart's drawing box, in SVG units; the page scales it to the panel.
const W: f64 = 640.;
const H: f64 = 260.;
const LEFT: f64 = 56.;
const RIGHT: f64 = 12.;
const TOP: f64 = 12.;
const BOTTOM: f64 = 30.;
/// At most this many x labels; the rest are thinned out evenly.
const X_LABELS: usize = 8;
/// A table panel stops here; the rest of the rows are counted, not written.
const TABLE_ROWS: usize = 500;

/// The whole page: `title` over `plots` laid out on the dashboard's grid,
/// with `picks` (each already `column = value`) said under the title.
pub(crate) fn render(
    title: &str,
    plots: &[PreparedPlot],
    results: &HashMap<String, Arc<QueryResult>>,
    picks: &[String],
) -> String {
    let mut body = String::new();
    let _ = write!(body, "<header><h1>{}</h1>", escape(title));
    if !picks.is_empty() {
        let _ = write!(
            body,
            "<p class=\"meta\">{} {}</p>",
            escape(tr("dashboard.filter.label")),
            escape(&picks.join(" · "))
        );
    }
    body.push_str("</header><main class=\"grid\">");
    for plot in plots {
        panel(&mut body, plot, results.get(&plot.query).map(Arc::as_ref));
    }
    body.push_str("</main>");
    page(title, &body)
}

fn panel(out: &mut String, plot: &PreparedPlot, result: Option<&QueryResult>) {
    let span = plot.width.clamp(1, super::model::GRID_COLUMNS);
    if plot.kind == "text" {
        let _ = write!(out, "<section class=\"text\" style=\"--span:{span}\">");
        if !plot.title.is_empty() {
            let _ = write!(out, "<h2>{}</h2>", escape(&plot.title));
        }
        if let Some(text) = &plot.text {
            for para in text.split("\n\n") {
                let _ = write!(out, "<p>{}</p>", escape(para.trim()));
            }
        }
        out.push_str("</section>");
        return;
    }
    let tone = plot
        .tone
        .as_deref()
        .filter(|tone| super::model::TONES.contains(tone));
    let _ = write!(
        out,
        "<section class=\"panel {}\" style=\"--span:{span}\"",
        escape(&plot.kind)
    );
    if let Some(tone) = tone {
        let _ = write!(out, " data-tone=\"{tone}\"");
    }
    out.push('>');
    let _ = write!(out, "<h3>{}</h3>", escape(&plot.title));
    if let Some(description) = &plot.description {
        let _ = write!(out, "<p class=\"meta\">{}</p>", escape(description));
    }
    if let Some(failure) = &plot.failure {
        let _ = write!(out, "<p class=\"warn\">{}</p></section>", escape(failure));
        return;
    }
    match plot.kind.as_str() {
        "card" => card(out, plot),
        "table" => table(out, result),
        "pie" => pie(out, plot),
        "heatmap" => heatmap(out, plot),
        "line" | "area" | "bar" | "stacked_bar" | "histogram" | "scatter" => chart(out, plot),
        _ => {
            let _ = write!(
                out,
                "<p class=\"meta\">{}</p>",
                escape(tr("dashboard.export_html.as_table"))
            );
            table(out, result);
        }
    }
    if let Some(notice) = &plot.notice {
        let _ = write!(out, "<p class=\"meta\">{}</p>", escape(notice));
    }
    out.push_str("</section>");
}

fn card(out: &mut String, plot: &PreparedPlot) {
    let value = plot.card.as_deref().unwrap_or("–");
    let _ = write!(out, "<div class=\"value\">{}</div>", escape(value));
    if let Some(delta) = &plot.delta {
        let mood = match delta.mood {
            Mood::Good => "good",
            Mood::Bad => "bad",
            Mood::Flat => "flat",
        };
        let _ = write!(
            out,
            "<div class=\"delta {mood}\">{}</div>",
            escape(&delta.text)
        );
    }
    if let Some(note) = &plot.note {
        let _ = write!(out, "<p class=\"meta\">{}</p>", escape(note));
    }
}

fn table(out: &mut String, result: Option<&QueryResult>) {
    let Some(result) = result else {
        return;
    };
    let numeric: Vec<bool> = result
        .columns
        .iter()
        .map(|c| c.kind == ColumnKind::Numeric)
        .collect();
    out.push_str("<div class=\"scroll\"><table><thead><tr>");
    for (ix, column) in result.columns.iter().enumerate() {
        let class = if numeric[ix] { " class=\"num\"" } else { "" };
        let _ = write!(out, "<th{class}>{}</th>", escape(&column.name));
    }
    out.push_str("</tr></thead><tbody>");
    for (r, row) in result.rows.iter().take(TABLE_ROWS).enumerate() {
        out.push_str("<tr>");
        for (c, cell) in row.iter().enumerate() {
            let mut classes = Vec::new();
            if numeric.get(c).copied().unwrap_or(false) {
                classes.push("num");
            }
            if result.is_sql_null(r, c) {
                classes.push("null");
            }
            if classes.is_empty() {
                out.push_str("<td>");
            } else {
                let _ = write!(out, "<td class=\"{}\">", classes.join(" "));
            }
            out.push_str(&escape(cell));
            out.push_str("</td>");
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
    let shown = result.rows.len().min(TABLE_ROWS);
    if shown < result.rows.len() || result.truncated {
        let _ = write!(
            out,
            "<p class=\"meta\">{}</p>",
            escape(&crate::i18n::trf(
                "dashboard.export_html.rows_cut",
                &[&shown.to_string()]
            ))
        );
    }
}

/// Swatches for each series, when there is more than one to tell apart.
fn legend(out: &mut String, names: &[(String, Option<usize>)]) {
    if names.len() < 2 {
        return;
    }
    out.push_str("<ul class=\"legend\">");
    for (name, slot) in names {
        let _ = write!(
            out,
            "<li><i style=\"background:{}\"></i>{}</li>",
            color(*slot),
            escape(name)
        );
    }
    out.push_str("</ul>");
}

/// A palette slot's colour, as the page's own variable so it follows the
/// reader's light or dark appearance; `None` is the folded "other".
fn color(slot: Option<usize>) -> String {
    match slot {
        Some(slot) => format!("var(--s{})", slot % crate::ui::palette::SLOTS),
        None => "var(--other)".to_string(),
    }
}

/// Round axis bounds and the ticks between them, 0 included.
fn ticks(lo: f64, hi: f64) -> Vec<f64> {
    let (lo, hi) = if lo == hi {
        (lo.min(0.), hi.max(lo + 1.))
    } else {
        (lo, hi)
    };
    let raw = (hi - lo) / 4.;
    let magnitude = 10f64.powf(raw.log10().floor());
    let step = [1., 2., 2.5, 5., 10.]
        .iter()
        .map(|m| m * magnitude)
        .find(|step| *step >= raw)
        .unwrap_or(raw);
    let start = (lo / step).floor() * step;
    let mut out = Vec::new();
    let mut t = start;
    loop {
        out.push((t * 1e9).round() / 1e9);
        if t >= hi - step * 1e-9 || out.len() > 20 {
            break;
        }
        t += step;
    }
    out
}

fn tick_labels(format: Option<NumberFormat>, ticks: &[f64]) -> Vec<String> {
    match format {
        Some(format) => format.ticks(ticks),
        None => crate::ui::chart::format_ticks(ticks),
    }
}

/// Bars, lines, areas and points over the plot's bands (or, for a scatter
/// or a line over times, over its x values).
fn chart(out: &mut String, plot: &PreparedPlot) {
    let points = &plot.points;
    if points.is_empty() {
        let _ = write!(
            out,
            "<p class=\"meta\">{}</p>",
            escape(tr("dashboard.export_html.empty"))
        );
        return;
    }
    let series = plot.series_names.len().max(1);
    let kind = plot.kind.as_str();
    let stacked = kind == "stacked_bar";
    let bars = matches!(kind, "bar" | "stacked_bar" | "histogram");

    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for point in points {
        if stacked {
            let (mut up, mut down) = (0., 0.);
            for v in &point.values {
                if *v >= 0. {
                    up += v;
                } else {
                    down += v;
                }
            }
            lo = lo.min(down);
            hi = hi.max(up);
        } else {
            for (s, v) in point.values.iter().enumerate() {
                if point.present.get(s).copied().unwrap_or(true) && v.is_finite() {
                    lo = lo.min(*v);
                    hi = hi.max(*v);
                }
            }
        }
    }
    if !lo.is_finite() {
        lo = 0.;
        hi = 1.;
    }
    if bars || kind == "area" {
        lo = lo.min(0.);
        hi = hi.max(0.);
    }
    let yt = ticks(lo, hi);
    let (y0, y1) = (yt[0], *yt.last().unwrap_or(&hi));
    let plot_w = W - LEFT - RIGHT;
    let plot_h = H - TOP - BOTTOM;
    let y = |v: f64| TOP + plot_h * (1. - (v - y0) / (y1 - y0).max(f64::EPSILON));

    // x: a band's centre, or a linear position over the x values.
    let linear = plot
        .x_values
        .as_ref()
        .filter(|xs| xs.len() == points.len() && !bars);
    let band_w = plot_w / points.len() as f64;
    let (x_lo, x_hi) = linear
        .map(|xs| {
            xs.iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| {
                    (a.min(*x), b.max(*x))
                })
        })
        .unwrap_or((0., 1.));
    let x_numeric_ticks = linear
        .filter(|_| !plot.x_is_time)
        .map(|_| ticks(x_lo, x_hi));
    let (x_lo, x_hi) = match &x_numeric_ticks {
        Some(t) => (t[0], *t.last().unwrap_or(&x_hi)),
        None => (x_lo, x_hi),
    };
    let x = |i: usize| match linear {
        Some(xs) => {
            let pad = if x_numeric_ticks.is_some() { 0. } else { 8. };
            LEFT + pad + (plot_w - 2. * pad) * (xs[i] - x_lo) / (x_hi - x_lo).max(f64::EPSILON)
        }
        None => LEFT + band_w * (i as f64 + 0.5),
    };

    let _ = write!(
        out,
        "<svg viewBox=\"0 0 {W} {H}\" role=\"img\" aria-label=\"{}\">",
        escape(&plot.title)
    );
    // Grid lines and y labels.
    for (t, label) in yt.iter().zip(tick_labels(plot.format, &yt)) {
        let ty = y(*t);
        let _ = write!(
            out,
            "<line class=\"grid\" x1=\"{LEFT}\" x2=\"{}\" y1=\"{ty:.1}\" y2=\"{ty:.1}\"/>\
             <text class=\"tick\" x=\"{}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
            W - RIGHT,
            LEFT - 6.,
            ty + 4.,
            escape(&label)
        );
    }
    // x labels.
    match &x_numeric_ticks {
        Some(xt) => {
            for (t, label) in xt.iter().zip(tick_labels(None, xt)) {
                let tx = LEFT + plot_w * (t - x_lo) / (x_hi - x_lo).max(f64::EPSILON);
                let _ = write!(
                    out,
                    "<text class=\"tick\" x=\"{tx:.1}\" y=\"{}\" text-anchor=\"middle\">{}</text>",
                    H - 10.,
                    escape(&label)
                );
            }
        }
        None => {
            let step = points.len().div_ceil(X_LABELS).max(1);
            for i in (0..points.len()).step_by(step) {
                let _ = write!(
                    out,
                    "<text class=\"tick\" x=\"{:.1}\" y=\"{}\" text-anchor=\"middle\">{}</text>",
                    x(i),
                    H - 10.,
                    escape(&clip(&points[i].band, 14))
                );
            }
        }
    }

    let tip = |i: usize, s: usize, v: f64| {
        let name = plot.series_names.get(s).map(String::as_str).unwrap_or("");
        let value = full_text(plot.format, v);
        if series > 1 {
            format!("{} · {name}: {value}", points[i].band)
        } else {
            format!("{}: {value}", points[i].band)
        }
    };
    let present = |i: usize, s: usize| points[i].present.get(s).copied().unwrap_or(true);

    if bars {
        let inner = band_w * if kind == "histogram" { 0.96 } else { 0.8 };
        for (i, point) in points.iter().enumerate() {
            let left = x(i) - inner / 2.;
            let (mut up, mut down) = (0., 0.);
            for (s, v) in point.values.iter().enumerate() {
                if !present(i, s) || !v.is_finite() {
                    continue;
                }
                let (bx, bw, from, to) = if stacked {
                    let base = if *v >= 0. { &mut up } else { &mut down };
                    let from = *base;
                    *base += v;
                    (left, inner, from, *base)
                } else {
                    let bw = inner / series as f64;
                    (left + bw * s as f64, bw, 0., *v)
                };
                let (top, bottom) = (y(from.max(to)), y(from.min(to)));
                let _ = write!(
                    out,
                    "<rect x=\"{bx:.1}\" y=\"{top:.1}\" width=\"{:.1}\" height=\"{:.1}\" \
                     style=\"fill:{}\"><title>{}</title></rect>",
                    (bw - 1.).max(0.5),
                    (bottom - top).max(0.),
                    color(Some(plot.slot(s))),
                    escape(&tip(i, s, *v))
                );
            }
        }
    } else {
        for s in 0..series {
            let stroke = color(Some(plot.slot(s)));
            if kind != "scatter" {
                // A run of present points is one path; a gap breaks it.
                let mut runs: Vec<Vec<(f64, f64)>> = vec![Vec::new()];
                for (i, point) in points.iter().enumerate() {
                    match point.values.get(s) {
                        Some(v) if present(i, s) && v.is_finite() => {
                            runs.last_mut().unwrap().push((x(i), y(*v)));
                        }
                        _ => runs.push(Vec::new()),
                    }
                }
                for run in runs.iter().filter(|run| !run.is_empty()) {
                    let line: Vec<String> = run
                        .iter()
                        .map(|(px, py)| format!("{px:.1},{py:.1}"))
                        .collect();
                    if kind == "area" {
                        let base = y(0f64.clamp(y0, y1));
                        let _ = write!(
                            out,
                            "<polygon points=\"{:.1},{base:.1} {} {:.1},{base:.1}\" \
                             style=\"fill:{stroke};fill-opacity:.18\"/>",
                            run[0].0,
                            line.join(" "),
                            run[run.len() - 1].0
                        );
                    }
                    let _ = write!(
                        out,
                        "<polyline points=\"{}\" style=\"fill:none;stroke:{stroke};stroke-width:2\"/>",
                        line.join(" ")
                    );
                }
            }
            // Points carry the tooltips; a line too dense for dots has none.
            if kind != "scatter" && points.len() > 60 {
                continue;
            }
            let (r, opacity) = if kind == "scatter" {
                (3.5, ";fill-opacity:.7")
            } else {
                (2.5, "")
            };
            for (i, point) in points.iter().enumerate() {
                let Some(v) = point.values.get(s).filter(|v| v.is_finite()) else {
                    continue;
                };
                if !present(i, s) {
                    continue;
                }
                let _ = write!(
                    out,
                    "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"{r}\" style=\"fill:{stroke}{opacity}\">\
                     <title>{}</title></circle>",
                    x(i),
                    y(*v),
                    escape(&tip(i, s, *v))
                );
            }
        }
    }
    out.push_str("</svg>");
    let names: Vec<(String, Option<usize>)> = plot
        .series_names
        .iter()
        .enumerate()
        .map(|(s, name)| (name.clone(), Some(plot.slot(s))))
        .collect();
    legend(out, &names);
}

fn pie(out: &mut String, plot: &PreparedPlot) {
    let total: f64 = plot.pie.iter().map(|slice| slice.amount().max(0.)).sum();
    if total <= 0. {
        let _ = write!(
            out,
            "<p class=\"meta\">{}</p>",
            escape(tr("dashboard.export_html.empty"))
        );
        return;
    }
    let (cx, cy, outer, inner) = (110., 110., 100., 62.);
    out.push_str("<div class=\"pie\"><svg viewBox=\"0 0 220 220\" role=\"img\">");
    let mut angle = -std::f64::consts::FRAC_PI_2;
    let at = |r: f64, a: f64| (cx + r * a.cos(), cy + r * a.sin());
    for slice in &plot.pie {
        let share = slice.amount().max(0.) / total;
        if share <= 0. {
            continue;
        }
        let tip = format!(
            "{}: {} ({:.1}%)",
            slice.name(),
            full_text(plot.format, slice.amount()),
            share * 100.
        );
        let fill = color(slice.slot());
        if share >= 0.9999 {
            let _ = write!(
                out,
                "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{}\" style=\"fill:none;stroke:{fill};stroke-width:{}\">\
                 <title>{}</title></circle>",
                (outer + inner) / 2.,
                outer - inner,
                escape(&tip)
            );
            break;
        }
        let end = angle + share * std::f64::consts::TAU;
        let large = u8::from(share > 0.5);
        let (a, b, c, d) = (
            at(outer, angle),
            at(outer, end),
            at(inner, end),
            at(inner, angle),
        );
        let _ = write!(
            out,
            "<path d=\"M{:.2},{:.2} A{outer},{outer} 0 {large} 1 {:.2},{:.2} L{:.2},{:.2} \
             A{inner},{inner} 0 {large} 0 {:.2},{:.2} Z\" style=\"fill:{fill}\" class=\"slice\">\
             <title>{}</title></path>",
            a.0,
            a.1,
            b.0,
            b.1,
            c.0,
            c.1,
            d.0,
            d.1,
            escape(&tip)
        );
        angle = end;
    }
    let _ = write!(
        out,
        "<text x=\"{cx}\" y=\"{}\" text-anchor=\"middle\" class=\"total\">{}</text></svg>",
        cy + 6.,
        escape(&short_text(plot.format, total))
    );
    let names: Vec<(String, Option<usize>)> = plot
        .pie
        .iter()
        .map(|slice| {
            (
                format!(
                    "{} · {}",
                    slice.name(),
                    short_text(plot.format, slice.amount())
                ),
                slice.slot(),
            )
        })
        .collect();
    out.push_str("<ul class=\"legend\">");
    for (name, slot) in &names {
        let _ = write!(
            out,
            "<li><i style=\"background:{}\"></i>{}</li>",
            color(*slot),
            escape(name)
        );
    }
    out.push_str("</ul></div>");
}

/// x's bands across, the series down, each cell shaded by its value.
fn heatmap(out: &mut String, plot: &PreparedPlot) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for point in &plot.points {
        for (s, v) in point.values.iter().enumerate() {
            if point.present.get(s).copied().unwrap_or(true) && v.is_finite() {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
    }
    if !lo.is_finite() {
        let _ = write!(
            out,
            "<p class=\"meta\">{}</p>",
            escape(tr("dashboard.export_html.empty"))
        );
        return;
    }
    out.push_str("<div class=\"scroll\"><table class=\"heat\"><thead><tr><th></th>");
    for point in &plot.points {
        let _ = write!(out, "<th>{}</th>", escape(&point.band));
    }
    out.push_str("</tr></thead><tbody>");
    for (s, name) in plot.series_names.iter().enumerate() {
        let _ = write!(out, "<tr><th>{}</th>", escape(name));
        for point in &plot.points {
            match point.values.get(s) {
                Some(v) if point.present.get(s).copied().unwrap_or(true) && v.is_finite() => {
                    let t = if hi > lo { (v - lo) / (hi - lo) } else { 1. };
                    let _ = write!(
                        out,
                        "<td style=\"--t:{:.0}%\" title=\"{} · {name}: {}\">{}</td>",
                        8. + t * 92.,
                        escape(&point.band),
                        escape(&full_text(plot.format, *v)),
                        escape(&short_text(plot.format, *v))
                    );
                }
                _ => out.push_str("<td class=\"null\"></td>"),
            }
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(max - 1).collect();
        cut.push('…');
        cut
    }
}

fn page(title: &str, body: &str) -> String {
    let palette = |hexes: &[u32]| {
        hexes
            .iter()
            .enumerate()
            .map(|(i, hex)| format!("--s{i}:#{hex:06x};"))
            .collect::<String>()
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"generator\" content=\"DuckLocal {}\">\
         <title>{}</title><style>:root{{{}}}\
         @media (prefers-color-scheme: dark){{:root{{{}}}}}{STYLE}</style></head>\
         <body>{body}</body></html>",
        env!("CARGO_PKG_VERSION"),
        escape(title),
        palette(&crate::ui::palette::LIGHT),
        palette(&crate::ui::palette::DARK),
    )
}

/// Text for an element or a double-quoted attribute.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Light and dark both: the page follows the reader's system appearance.
const STYLE: &str = "
:root { color-scheme: light dark; --page: #f7f7f5; --panel: #ffffff; --fg: #1d1d1f;
  --muted: #6e6e73; --line: #e5e5ea; --head: #f5f5f7; --other: #aeaeb2; --heat: 42,120,214;
  --good: #0ca30c; --good-ink: #006300; --warning: #fab219; --warning-ink: #b45309;
  --critical: #d03b3b; --critical-ink: #d03b3b; --accent: var(--s0); --accent-ink: var(--s0); }
@media (prefers-color-scheme: dark) { :root { --page: #111110; --panel: #1c1c1b; --fg: #f5f5f7;
  --muted: #98989d; --line: #3a3a3c; --head: #2c2c2e; --other: #636366; --heat: 57,135,229;
  --good-ink: #0ca30c; --warning-ink: #fab219; --critical-ink: #e66767; } }
* { box-sizing: border-box; }
body { margin: 0; padding: 16px; background: var(--page); color: var(--fg);
  font: 14px system-ui, -apple-system, 'Segoe UI', sans-serif; }
header { max-width: 1400px; margin: 0 auto 12px; }
h1 { font-size: 20px; margin: 0 0 4px; }
h2 { font-size: 18px; margin: 0 0 4px; }
h3 { font-size: 13px; font-weight: 600; margin: 0 0 6px; }
p { margin: 0 0 4px; }
.grid { display: grid; grid-template-columns: repeat(12, minmax(0, 1fr)); gap: 12px;
  max-width: 1400px; margin: 0 auto; }
.grid > section { grid-column: span var(--span); min-width: 0; }
@media (max-width: 720px) { .grid > section { grid-column: span 12; } }
.panel { background: var(--panel); border: 1px solid var(--line); border-radius: 10px;
  padding: 12px 14px; }
.panel[data-tone] { border-top: 3px solid var(--tone); }
.panel[data-tone=accent] { --tone: var(--accent); --ink: var(--accent-ink); }
.panel[data-tone=good] { --tone: var(--good); --ink: var(--good-ink); }
.panel[data-tone=warning] { --tone: var(--warning); --ink: var(--warning-ink); }
.panel[data-tone=critical] { --tone: var(--critical); --ink: var(--critical-ink); }
.text { padding: 4px 2px; }
.text p { color: var(--muted); }
.meta { color: var(--muted); font-size: 12px; }
.warn { color: var(--critical-ink); }
.value { font-size: 30px; font-weight: 650; font-variant-numeric: tabular-nums;
  color: var(--ink, var(--fg)); line-height: 1.2; }
.delta { font-size: 13px; font-weight: 600; }
.delta.good { color: var(--good-ink); } .delta.bad { color: var(--critical-ink); }
.delta.flat { color: var(--muted); }
svg { display: block; width: 100%; height: auto; overflow: visible; }
svg .grid { stroke: var(--line); stroke-width: 1; }
svg .tick { fill: var(--muted); font-size: 11px; }
svg .total { fill: var(--fg); font-size: 18px; font-weight: 650; }
svg rect:hover, svg circle:hover, svg .slice:hover { opacity: .8; }
.pie { display: flex; gap: 16px; align-items: center; flex-wrap: wrap; }
.pie svg { width: 200px; flex: none; }
.legend { list-style: none; margin: 6px 0 0; padding: 0; display: flex; flex-wrap: wrap;
  gap: 4px 12px; font-size: 12px; color: var(--muted); }
.pie .legend { flex-direction: column; }
.legend i { display: inline-block; width: 10px; height: 10px; border-radius: 2px;
  margin-right: 6px; vertical-align: -1px; }
.scroll { overflow: auto; max-height: 480px; }
table { border-collapse: collapse; white-space: nowrap; font-size: 12px; width: 100%;
  font-variant-numeric: tabular-nums; }
th, td { padding: 4px 8px; border-bottom: 1px solid var(--line); text-align: left; }
thead th { background: var(--head); position: sticky; top: 0; font-weight: 600; }
.num { text-align: right; }
td.null { color: var(--other); font-style: italic; }
.heat td { text-align: center; background: rgba(var(--heat), var(--t)); }
.heat tbody th { font-weight: 500; color: var(--muted); }
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::model;
    use crate::spec::prepare::prepare;

    fn rows(columns: &[(&str, ColumnKind)], rows: &[&[&str]]) -> QueryResult {
        QueryResult {
            columns: columns
                .iter()
                .map(|(name, kind)| crate::query::ColumnMeta {
                    name: (*name).to_string(),
                    duck_type: String::new(),
                    kind: kind.clone(),
                })
                .collect(),
            rows: rows
                .iter()
                .map(|row| row.iter().map(|cell| (*cell).to_string()).collect())
                .collect(),
            elapsed_ms: 0,
            truncated: false,
            text_nulls: Default::default(),
        }
    }

    fn render_spec(source: &str, data: HashMap<&str, QueryResult>) -> String {
        let spec = model::validate(&model::parse(source).unwrap()).unwrap();
        let results: HashMap<String, Arc<QueryResult>> = data
            .into_iter()
            .map(|(name, result)| (name.to_string(), Arc::new(result)))
            .collect();
        let plots: Vec<PreparedPlot> = spec
            .plots
            .iter()
            .map(|plot| {
                let result = results
                    .get(&plot.query)
                    .map(|r| Ok::<_, String>(QueryResult::clone(r)));
                prepare(plot, result.as_ref())
            })
            .collect();
        render("orders", &plots, &results, &["channel = Web".to_string()])
    }

    const SPEC: &str = r#"
query "q" {
  sql = "SELECT 1"
}
plot "intro" {
  type = "text"
  title = "Hello <world>"
  text = "Words"
}
plot "rev" {
  type = "card"
  query = query.q
  value = n
  tone = "good"
}
plot "by_day" {
  type = "bar"
  query = query.q
  x = day
  y = n
}
plot "trend" {
  type = "line"
  query = query.q
  x = day
  y = n
}
plot "share" {
  type = "pie"
  query = query.q
  x = day
  y = n
}
plot "grid" {
  type = "table"
  query = query.q
}
"#;

    #[test]
    fn every_kind_is_drawn_and_nothing_is_fetched() {
        let data = rows(
            &[("day", ColumnKind::Text), ("n", ColumnKind::Numeric)],
            &[&["Mon", "3"], &["Tue", "5"], &["<b>", "2"]],
        );
        let page = render_spec(SPEC, HashMap::from([("q", data)]));
        assert!(page.starts_with("<!doctype html>"));
        assert!(
            !page.contains("http://") && !page.contains("https://"),
            "{page}"
        );
        assert!(!page.contains("<script"), "{page}");
        assert!(page.contains("Hello &lt;world&gt;"), "{page}");
        assert!(page.contains("&lt;b&gt;"), "{page}");
        assert!(!page.contains("<b>"), "{page}");
        assert!(page.contains("data-tone=\"good\""), "{page}");
        assert!(page.contains("<rect"), "{page}");
        assert!(page.contains("<polyline"), "{page}");
        assert!(page.contains("<path d=\"M"), "{page}");
        assert!(page.contains("<td class=\"num\">5</td>"), "{page}");
        assert!(page.contains("channel = Web"), "{page}");
    }

    #[test]
    fn a_failed_plot_says_why() {
        let page = render_spec(SPEC, HashMap::new());
        assert!(page.contains("class=\"warn\""), "{page}");
    }

    #[test]
    fn ticks_are_round_and_cover_the_range() {
        let t = ticks(0., 87.);
        assert_eq!(t.first(), Some(&0.));
        assert!(*t.last().unwrap() >= 87.);
        assert!(t.len() <= 7, "{t:?}");
    }
}
