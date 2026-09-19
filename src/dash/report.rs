//! A panel's captured statements as one self-contained HTML file.
//!
//! Everything here is pure: the report is a string built from captures, so what
//! the file says can be tested without running a panel. The one thing this does
//! not do is reproduce the panel. The panel's charts and tables are native
//! widgets whose contents are decided while they are laid out — the description
//! says *that* a chart is there, never what its bars are — so the file shows
//! the statements and their results, which is the part that travels.
//!
//! No JavaScript, no external stylesheet, no font to fetch: a file that needs
//! the network is not a file you can hand to someone.

use std::fmt::Write as _;
use std::path::Path;

use crate::dash::capture::{Capture, Table};
use crate::query::{plain_text, CliResult};

/// Longest a cell is given before the table stops being a table. The JSON
/// format keeps the whole value; a document has a reader.
const CELL_CAP: usize = 400;

/// Bars are for a comparison a reader can take in at a glance. Past this many
/// rows it is a table with a chart stapled to it.
const CHART_ROWS: usize = 25;

pub struct Report<'a> {
    pub panel: &'a Path,
    pub entry: &'a str,
    pub database: &'a str,
    pub exported_at: &'a str,
    pub version: &'a str,
    pub captures: &'a [Capture],
    pub panel_error: Option<&'a str>,
    /// Errors the panel logged while it ran: a promise it left unhandled, a
    /// callback the shell could not attach. It still produced what is below.
    pub reported: &'a [String],
    /// "settled" when the panel went quiet on its own, "deadline" when the
    /// time limit cut the capture short and the panel may not be done.
    pub stop_reason: &'a str,
}

pub fn build(report: &Report<'_>) -> String {
    let name = report
        .panel
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| report.panel.display().to_string());
    let statements = report.captures.iter().filter_map(Capture::sql).count();
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(out, "<title>{} — DuckLocal export</title>", escape(&name));
    out.push_str(STYLE);
    out.push_str("</head>\n<body>\n<header>\n");
    let _ = writeln!(out, "<h1>{}</h1>", escape(&name));
    let _ = writeln!(
        out,
        "<p class=\"where\">{}</p>",
        escape(&report.panel.display().to_string())
    );
    out.push_str("<dl class=\"meta\">");
    let _ = writeln!(
        out,
        "<div><dt>Exported</dt><dd>{} UTC</dd></div>",
        escape(report.exported_at)
    );
    let _ = writeln!(
        out,
        "<div><dt>Database</dt><dd>{}</dd></div>",
        escape(report.database)
    );
    let _ = writeln!(out, "<div><dt>Statements</dt><dd>{statements}</dd></div>");
    let _ = writeln!(
        out,
        "<div><dt>DuckLocal</dt><dd>{}</dd></div>",
        escape(report.version)
    );
    out.push_str("</dl>\n");
    out.push_str(
        "<p class=\"note\">What this panel asked the database while it loaded. The panel's own \
         interface is not part of this file, and neither is anything that depends on using it: \
         filters, toggles and later refreshes are not represented, and there is no JavaScript here \
         to run.</p>\n",
    );
    out.push_str("</header>\n");

    if let Some(error) = report.panel_error {
        out.push_str("<section class=\"panel-error\">\n<h2>The panel did not finish</h2>\n");
        let _ = writeln!(out, "<pre>{}</pre>", escape(error));
        out.push_str(
            "<p class=\"note\">The statements below are what it managed before it stopped.</p>\n</section>\n",
        );
    }

    if report.stop_reason == "deadline" {
        out.push_str(
            "<section class=\"panel-error\">\n<h2>The capture hit the time limit</h2>\n\
             <p class=\"note\">The panel was still working when the capture stopped, so the \
             statements below may not be everything it asks. A longer \
             <code>--timeout</code> gives it more time.</p>\n</section>\n",
        );
    }

    if !report.reported.is_empty() {
        out.push_str("<section class=\"panel-error\">\n<h2>The panel reported errors</h2>\n");
        out.push_str(
            "<p class=\"note\">It kept running and asked the database the statements below, \
             but something in it failed.</p>\n",
        );
        for message in report.reported {
            let _ = writeln!(out, "<pre>{}</pre>", escape(message));
        }
        out.push_str("</section>\n");
    }

    out.push_str("<main>\n");
    let mut index = 1;
    for capture in report.captures {
        match &capture.statement {
            crate::dash::capture::Statement::Query {
                sql,
                limit,
                outcome,
            } => {
                write_query(&mut out, index, sql, *limit, outcome, capture.runs);
                index += 1;
            }
            crate::dash::capture::Statement::Catalog { tables } => {
                write_catalog(&mut out, tables, capture.runs);
            }
        }
    }
    if report.captures.is_empty() {
        out.push_str(
            "<p class=\"note\">The panel ran no statement against the database while it loaded. \
             A panel that queries on a click has nothing to show here.</p>\n",
        );
    }
    out.push_str("</main>\n");
    let _ = writeln!(
        out,
        "<footer>Exported by DuckLocal {} from {}.</footer>",
        escape(report.version),
        escape(report.entry)
    );
    out.push_str("</body>\n</html>\n");
    out
}

fn write_query(
    out: &mut String,
    index: usize,
    sql: &str,
    limit: usize,
    outcome: &Result<CliResult, String>,
    runs: usize,
) {
    let _ = writeln!(out, "<section class=\"statement\">");
    let heading = format!("Statement {index}");
    match outcome {
        Ok(result) => {
            let rows = result.row_count;
            let _ = writeln!(
                out,
                "<h2>{heading}<span class=\"count\">{rows} {}</span></h2>",
                if rows == 1 { "row" } else { "rows" }
            );
        }
        Err(_) => {
            let _ = writeln!(
                out,
                "<h2>{heading}<span class=\"count failed\">failed</span></h2>"
            );
        }
    }
    let _ = writeln!(out, "<pre class=\"sql\">{}</pre>", escape(sql));
    match outcome {
        Ok(result) => {
            let columns: Vec<String> = result
                .columns
                .iter()
                .map(|column| format!("{} ({})", column.name, column.arrow_type))
                .collect();
            let mut meta = vec![
                format!("{} ms", result.elapsed_ms),
                format!("limit {limit}"),
                format!("columns: {}", escape(&columns.join(", "))),
            ];
            if runs > 1 {
                meta.push(format!("run {runs} times, the last result shown"));
            }
            let _ = writeln!(out, "<p class=\"meta\">{}</p>", meta.join(" · "));
            if result.truncated {
                out.push_str(
                    "<p class=\"warning\">Truncated: more rows were available, so this is not the \
                     whole answer. Aggregate in SQL for a complete result.</p>\n",
                );
            }
            if result.rows.is_empty() {
                out.push_str("<p class=\"note\">No rows.</p>\n");
            } else {
                if let Some(chart) = bar_chart(result) {
                    out.push_str(&chart);
                }
                write_table(out, result);
            }
        }
        Err(error) => {
            let _ = writeln!(
                out,
                "<p class=\"warning\">This statement failed: {}</p>",
                escape(error)
            );
            let _ = writeln!(out, "<p class=\"meta\">limit {limit}</p>");
        }
    }
    out.push_str("</section>\n");
}

fn write_table(out: &mut String, result: &CliResult) {
    out.push_str("<table>\n<thead>\n<tr>");
    for column in &result.columns {
        let _ = write!(
            out,
            "<th title=\"{}\">{}</th>",
            escape(&column.arrow_type),
            escape(&column.name)
        );
    }
    out.push_str("</tr>\n</thead>\n<tbody>\n");
    for row in &result.rows {
        out.push_str("<tr>");
        for cell in row {
            let class = if cell.is_null() {
                " class=\"null\""
            } else {
                ""
            };
            let _ = write!(out, "<td{class}>{}</td>", escape(&elide(plain_text(cell))));
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody>\n</table>\n");
}

/// A bar chart, when the result has the shape one describes: a name and a
/// number per row, few enough rows to compare by eye, and no negative value —
/// a bar drawn from a baseline it does not have would misstate the value.
fn bar_chart(result: &CliResult) -> Option<String> {
    if result.columns.len() != 2 || result.rows.is_empty() || result.rows.len() > CHART_ROWS {
        return None;
    }
    let mut values = Vec::with_capacity(result.rows.len());
    let mut labels = Vec::with_capacity(result.rows.len());
    for row in &result.rows {
        if row.len() != 2 {
            return None;
        }
        labels.push(elide(plain_text(&row[0])));
        let value = numeric_text(&row[1])?;
        if !value.is_finite() || value < 0.0 {
            return None;
        }
        values.push(value);
    }
    let largest = values.iter().cloned().fold(0.0_f64, f64::max);
    if largest <= 0.0 {
        return None;
    }

    let row_height = 22.0;
    let label_width = 170.0;
    let plot_width = 330.0;
    let value_width = 150.0;
    let width = label_width + plot_width + value_width;
    let height = row_height * values.len() as f64 + 8.0;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<figure><svg viewBox=\"0 0 {width} {height}\" width=\"100%\" role=\"img\" \
         aria-label=\"{} by {}\">",
        escape(&result.columns[1].name),
        escape(&result.columns[0].name)
    );
    for (index, (label, value)) in labels.iter().zip(&values).enumerate() {
        let y = index as f64 * row_height + 4.0;
        let bar = (value / largest) * plot_width;
        let text_y = y + row_height / 2.0 + 4.0;
        let _ = writeln!(
            out,
            "<text class=\"label\" x=\"0\" y=\"{text_y}\">{}</text>",
            escape(label)
        );
        let _ = writeln!(
            out,
            "<rect x=\"{label_width}\" y=\"{y}\" width=\"{bar:.2}\" height=\"{}\" \
             rx=\"2\"></rect>",
            row_height - 8.0
        );
        let _ = writeln!(
            out,
            "<text class=\"value\" x=\"{}\" y=\"{text_y}\">{}</text>",
            label_width + bar + 6.0,
            escape(&format_number(*value))
        );
    }
    out.push_str("</svg>\n");
    let _ = writeln!(
        out,
        "<figcaption>{} by {}, as the statement returned it.</figcaption></figure>",
        escape(&result.columns[1].name),
        escape(&result.columns[0].name)
    );
    Some(out)
}

fn write_catalog(out: &mut String, tables: &[Table], runs: usize) {
    out.push_str("<section class=\"statement catalog\">\n");
    let _ = writeln!(
        out,
        "<h2>Catalog<span class=\"count\">{} {}</span></h2>",
        tables.len(),
        if tables.len() == 1 {
            "entry"
        } else {
            "entries"
        }
    );
    let _ = writeln!(
        out,
        "<p class=\"meta\">catalog() · run {runs} times, the last result shown</p>"
    );
    if tables.is_empty() {
        out.push_str("<p class=\"note\">The database held no table or view.</p>\n</section>\n");
        return;
    }
    for table in tables {
        let label = if table.schema.is_empty() {
            table.name.clone()
        } else {
            format!("{}.{}", table.schema, table.name)
        };
        let rows = match table.estimated_rows {
            Some(rows) => format!(" · about {rows} rows"),
            None => String::new(),
        };
        let comment = match &table.comment {
            Some(comment) => format!(" · {}", escape(comment)),
            None => String::new(),
        };
        let _ = writeln!(
            out,
            "<h3>{} <span class=\"count\">{} in {}{rows}</span></h3>",
            escape(&label),
            table.kind,
            escape(&table.database)
        );
        if !comment.is_empty() {
            let _ = writeln!(out, "<p class=\"meta\">{comment}</p>");
        }
        out.push_str(
            "<table>\n<thead>\n<tr><th>column</th><th>type</th></tr>\n</thead>\n<tbody>\n",
        );
        for (name, data_type) in &table.columns {
            let _ = writeln!(
                out,
                "<tr><td>{}</td><td>{}</td></tr>",
                escape(name),
                escape(data_type)
            );
        }
        out.push_str("</tbody>\n</table>\n");
    }
    out.push_str("</section>\n");
}

/// A cell that reads as one number, for the chart. Encoded values are read the
/// same way they are printed; a value that is not a number refuses the chart
/// rather than being drawn as zero.
fn numeric_text(cell: &serde_json::Value) -> Option<f64> {
    use serde_json::Value;
    match cell {
        Value::Number(number) => number.as_f64(),
        Value::Object(fields) => match fields.get("encoding").and_then(Value::as_str) {
            Some("integer" | "decimal" | "float") => fields.get("value")?.as_str()?.parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

fn format_number(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

fn elide(text: String) -> String {
    if text.chars().count() <= CELL_CAP {
        return text;
    }
    let mut out: String = text.chars().take(CELL_CAP).collect();
    out.push('…');
    out
}

/// Escapes text for HTML, including the quotes an attribute sits in.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

const STYLE: &str = "<style>
:root { color-scheme: light dark; --fg: #1c1c1e; --muted: #6b6b70; --line: #d8d8dd;
        --bg: #ffffff; --panel: #f6f6f8; --bar: #4a6cf7; --warn: #a03030; --warn-bg: #fdf1f1; }
@media (prefers-color-scheme: dark) {
  :root { --fg: #ececf1; --muted: #9a9aa2; --line: #3a3a40; --bg: #16161a; --panel: #1e1e24;
          --bar: #7d95ff; --warn: #ff9c9c; --warn-bg: #2a1a1a; } }
* { box-sizing: border-box; }
body { margin: 0 auto; padding: 2rem 1.5rem 4rem; max-width: 1100px; background: var(--bg);
       color: var(--fg); font: 14px/1.5 -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; }
header { border-bottom: 1px solid var(--line); padding-bottom: 1rem; margin-bottom: 2rem; }
h1 { font-size: 1.6rem; margin: 0 0 0.25rem; }
h2 { font-size: 1.05rem; margin: 0 0 0.5rem; display: flex; align-items: baseline; gap: 0.6rem; }
h3 { font-size: 0.95rem; margin: 1.2rem 0 0.3rem; }
.where { color: var(--muted); margin: 0 0 1rem; word-break: break-all; }
.meta { color: var(--muted); font-size: 0.85rem; margin: 0.4rem 0; }
dl.meta { display: flex; flex-wrap: wrap; gap: 0.5rem 2rem; margin: 0; }
dl.meta div { display: flex; gap: 0.4rem; }
dl.meta dt { color: var(--muted); }
dl.meta dd { margin: 0; }
.note { color: var(--muted); font-size: 0.9rem; }
.count { color: var(--muted); font-size: 0.85rem; font-weight: 400; }
.count.failed { color: var(--warn); }
.warning { color: var(--warn); background: var(--warn-bg); border-radius: 6px;
           padding: 0.5rem 0.7rem; font-size: 0.9rem; }
section.statement { margin-bottom: 2.5rem; }
pre { background: var(--panel); border-radius: 6px; padding: 0.7rem 0.9rem; overflow-x: auto;
      font: 12px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre-wrap;
      word-break: break-word; margin: 0.5rem 0; }
table { border-collapse: collapse; width: 100%; font-size: 0.88rem; }
th, td { text-align: left; padding: 0.35rem 0.6rem; border-bottom: 1px solid var(--line);
         vertical-align: top; word-break: break-word; }
th { color: var(--muted); font-weight: 600; }
td.null { color: var(--muted); font-style: italic; }
figure { margin: 0.5rem 0 1rem; }
figure text { font: 12px -apple-system, BlinkMacSystemFont, sans-serif; fill: var(--fg); }
figure text.value { fill: var(--muted); }
figure rect { fill: var(--bar); }
figcaption { color: var(--muted); font-size: 0.82rem; }
footer { color: var(--muted); font-size: 0.82rem; border-top: 1px solid var(--line);
         margin-top: 3rem; padding-top: 1rem; }
</style>
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dash::capture::Statement;
    use crate::query::{CliColumn, CliResult};
    use serde_json::json;

    fn result(columns: &[&str], rows: Vec<Vec<serde_json::Value>>) -> CliResult {
        CliResult {
            columns: columns
                .iter()
                .map(|name| CliColumn {
                    name: (*name).to_string(),
                    arrow_type: "Utf8".to_string(),
                })
                .collect(),
            row_count: rows.len(),
            rows,
            truncated: false,
            elapsed_ms: 3,
        }
    }

    fn query(sql: &str, outcome: Result<CliResult, String>) -> Capture {
        Capture {
            statement: Statement::Query {
                sql: sql.to_string(),
                limit: 1000,
                outcome,
            },
            runs: 1,
        }
    }

    fn document(captures: &[Capture]) -> String {
        build(&Report {
            panel: Path::new("/panels/sales"),
            entry: "main.js",
            database: "in-memory",
            exported_at: "2026-09-19 08:00:00",
            version: "0.1.0",
            captures,
            panel_error: None,
            reported: &[],
            stop_reason: "settled",
        })
    }

    #[test]
    fn the_report_carries_the_statements_and_their_rows() {
        let captures = [query(
            "SELECT city, total FROM sales ORDER BY total DESC",
            Ok(result(
                &["city", "total"],
                vec![vec![json!("北京"), json!(20)], vec![json!(null), json!(10)]],
            )),
        )];
        let html = document(&captures);
        assert!(html.contains("SELECT city, total FROM sales ORDER BY total DESC"));
        assert!(html.contains("<td>北京</td>"));
        assert!(html.contains("<td>20</td>"));
        // A null reads as a null, and is marked as one for the stylesheet.
        assert!(html.contains("<td class=\"null\">NULL</td>"), "{html}");
        assert!(html.contains("Statement 1"));
        assert!(html.contains("2 rows"));
    }

    /// A file that needs the network, or runs code, is not one you can hand to
    /// someone else and be sure what they will see.
    #[test]
    fn the_report_is_self_contained() {
        let html = document(&[query(
            "SELECT 1 AS n",
            Ok(result(&["n"], vec![vec![json!(1)]])),
        )]);
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("http://"), "{html}");
        assert!(!html.contains("https://"), "{html}");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.trim_end().ends_with("</html>"));
    }

    /// Data is data: markup in a cell is shown, not obeyed.
    #[test]
    fn a_cell_cannot_write_the_document_it_is_in() {
        let captures = [query(
            "SELECT note FROM t",
            Ok(result(
                &["note"],
                vec![vec![json!("</td></table><script>alert(1)</script>")]],
            )),
        )];
        let html = document(&captures);
        assert!(
            html.contains("&lt;/td&gt;&lt;/table&gt;&lt;script&gt;"),
            "{html}"
        );
        assert!(!html.contains("<script>alert(1)</script>"), "{html}");
    }

    #[test]
    fn a_truncated_result_says_it_is_not_the_whole_answer() {
        let mut truncated = result(&["n"], vec![vec![json!(1)]]);
        truncated.truncated = true;
        let html = document(&[query("SELECT n FROM big", Ok(truncated))]);
        assert!(html.contains("Truncated"), "{html}");

        let empty = document(&[query(
            "SELECT n FROM t WHERE false",
            Ok(result(&["n"], vec![])),
        )]);
        assert!(empty.contains("No rows."), "{empty}");
    }

    #[test]
    fn a_failed_statement_is_shown_with_its_reason() {
        let html = document(&[query(
            "SELECT * FROM absent",
            Err("Table with name absent does not exist".to_string()),
        )]);
        assert!(html.contains("This statement failed:"));
        assert!(html.contains("Table with name absent does not exist"));
        assert!(html.contains("failed"));
    }

    /// A chart only where one describes the result: a name and a number per
    /// row. Anything else is a table, and a table is not a worse answer.
    #[test]
    fn a_chart_appears_only_for_a_shape_a_chart_describes() {
        let counted = document(&[query(
            "SELECT city, count(*) FROM t GROUP BY 1",
            Ok(result(
                &["city", "count"],
                vec![
                    vec![json!("北京"), json!(20)],
                    vec![json!("上海"), json!(5)],
                ],
            )),
        )]);
        assert!(counted.contains("<svg"), "{counted}");

        // A second column that is not a number: no bars to draw.
        let text = document(&[query(
            "SELECT city, note FROM t",
            Ok(result(
                &["city", "note"],
                vec![vec![json!("北京"), json!("fine")]],
            )),
        )]);
        assert!(!text.contains("<svg"), "{text}");

        // A negative value has no zero baseline here, so it is not drawn.
        let negative = document(&[query(
            "SELECT city, delta FROM t",
            Ok(result(
                &["city", "delta"],
                vec![vec![json!("北京"), json!(-4)]],
            )),
        )]);
        assert!(!negative.contains("<svg"), "{negative}");

        // Too many rows to compare by eye.
        let rows: Vec<Vec<serde_json::Value>> = (0..CHART_ROWS + 1)
            .map(|n| vec![json!(format!("r{n}")), json!(n)])
            .collect();
        let many = document(&[query(
            "SELECT name, n FROM t",
            Ok(result(&["name", "n"], rows)),
        )]);
        assert!(!many.contains("<svg"), "{many}");
    }

    #[test]
    fn a_catalog_read_becomes_an_appendix_not_a_statement() {
        let captures = [Capture {
            statement: Statement::Catalog {
                tables: vec![Table {
                    database: "memory".to_string(),
                    schema: "main".to_string(),
                    name: "orders".to_string(),
                    kind: "table",
                    estimated_rows: Some(12),
                    comment: Some("one row per order".to_string()),
                    columns: vec![("id".to_string(), "INTEGER".to_string())],
                }],
            },
            runs: 1,
        }];
        let html = document(&captures);
        assert!(html.contains("main.orders"), "{html}");
        assert!(html.contains("about 12 rows"), "{html}");
        assert!(html.contains("one row per order"), "{html}");
        assert!(html.contains("<td>id</td><td>INTEGER</td>"), "{html}");
        assert!(!html.contains("Statement 1"), "{html}");
    }

    #[test]
    fn a_panel_that_failed_says_so_above_what_it_managed() {
        let captures = [query(
            "SELECT 1 AS n",
            Ok(result(&["n"], vec![vec![json!(1)]])),
        )];
        let html = build(&Report {
            panel: Path::new("/panels/sales"),
            entry: "main.js",
            database: "in-memory",
            exported_at: "2026-09-19 08:00:00",
            version: "0.1.0",
            captures: &captures,
            panel_error: Some("ReferenceError: columns is not defined"),
            reported: &[],
            stop_reason: "settled",
        });
        let error = html.find("The panel did not finish").unwrap();
        assert!(error < html.find("Statement 1").unwrap());
        assert!(html.contains("ReferenceError: columns is not defined"));
    }

    #[test]
    fn a_deadline_capture_warns_that_the_panel_may_not_be_done() {
        let captures = [query(
            "SELECT 1 AS n",
            Ok(result(&["n"], vec![vec![json!(1)]])),
        )];
        let settled = document(&captures);
        assert!(!settled.contains("time limit"), "{settled}");
        let html = build(&Report {
            panel: Path::new("/panels/sales"),
            entry: "main.js",
            database: "in-memory",
            exported_at: "2026-09-19 08:00:00",
            version: "0.1.0",
            captures: &captures,
            panel_error: None,
            reported: &[],
            stop_reason: "deadline",
        });
        assert!(html.contains("The capture hit the time limit"), "{html}");
    }

    #[test]
    fn a_panel_that_asked_nothing_says_that_too() {
        let html = document(&[]);
        assert!(
            html.contains("ran no statement against the database"),
            "{html}"
        );
        assert!(html.contains("<dd>0</dd>"), "{html}");
    }

    #[test]
    fn a_long_list_cell_is_elided_rather_than_allowed_to_swallow_the_table() {
        let long: Vec<serde_json::Value> = (0..200).map(|n| json!(n)).collect();
        let html = document(&[query(
            "SELECT xs FROM t",
            Ok(result(&["xs"], vec![vec![json!(long)]])),
        )]);
        assert!(html.contains('…'), "{html}");
        assert!(html.len() < 20_000, "{} bytes", html.len());
    }
}
