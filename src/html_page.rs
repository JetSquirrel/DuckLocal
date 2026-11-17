//! `ducklocal query --format html`: one result as a page a person can open,
//! send or publish.
//!
//! The page is one file with nothing to fetch: its style and the few lines of
//! script that sort and filter the table are inline, so it opens the same from
//! a disk, an email attachment or a static host such as GitHub Pages. The
//! values are the ones `--format md` shows, through the same
//! [`crate::query::plain_text`].

use std::fmt::Write as _;

use crate::query::{is_numeric_arrow, plain_text, CliResult};

/// The whole page for `result`, which `sql` produced.
pub fn format_html(result: &CliResult, sql: &str) -> String {
    let mut body = String::from("<h1>Query result</h1><div class=\"meta\">");
    let rows = match (result.truncated, result.total_rows_exact) {
        (false, _) => count(result.row_count, "row"),
        (true, true) => format!(
            "{} of {}",
            group_digits(result.row_count),
            count(result.total_rows, "row")
        ),
        (true, false) => format!(
            "{} of at least {}",
            group_digits(result.row_count),
            count(result.total_rows, "row")
        ),
    };
    for fact in [
        rows,
        count(result.columns.len(), "column"),
        format!("{} ms", result.elapsed_ms),
    ] {
        let _ = write!(body, "<span>{}</span>", escape(&fact));
    }
    body.push_str("</div>");
    let _ = write!(
        body,
        "<details><summary>SQL</summary><pre>{}</pre></details>",
        escape(sql.trim())
    );

    if result.columns.is_empty() {
        body.push_str("<p class=\"note\">No columns were returned.</p>");
        return page(&body);
    }

    body.push_str(
        "<input id=\"filter\" type=\"search\" placeholder=\"Filter rows\" \
         aria-label=\"Filter rows\"><span id=\"shown\" class=\"note\"></span>\
         <div class=\"scroll\"><table><thead><tr>",
    );
    let numeric: Vec<bool> = result
        .columns
        .iter()
        .map(|c| is_numeric_arrow(&c.arrow_type))
        .collect();
    for (ix, column) in result.columns.iter().enumerate() {
        let class = if numeric[ix] { " class=\"num\"" } else { "" };
        let _ = write!(
            body,
            "<th{class} aria-sort=\"none\"><button type=\"button\">{}<small>{}</small></button></th>",
            escape(&column.name),
            escape(&column.arrow_type)
        );
    }
    body.push_str("</tr></thead><tbody>");
    for row in &result.rows {
        body.push_str("<tr>");
        for (ix, cell) in row.iter().enumerate() {
            let mut classes = Vec::new();
            if numeric.get(ix).copied().unwrap_or(false) {
                classes.push("num");
            }
            if cell.is_null() {
                classes.push("null");
            }
            if classes.is_empty() {
                body.push_str("<td>");
            } else {
                let _ = write!(body, "<td class=\"{}\">", classes.join(" "));
            }
            body.push_str(&escape(&plain_text(cell)));
            body.push_str("</td>");
        }
        body.push_str("</tr>");
    }
    body.push_str("</tbody></table></div>");
    if result.rows.is_empty() {
        body.push_str("<p class=\"note\">0 rows.</p>");
    }
    if result.truncated {
        let at_least = if result.total_rows_exact {
            ""
        } else {
            "at least "
        };
        let _ = write!(
            body,
            "<p class=\"note warn\">Truncated at {} of {at_least}{} rows, so this is not \
             the whole answer.</p>",
            group_digits(result.row_count),
            group_digits(result.total_rows)
        );
    }
    body.push_str("<script>");
    body.push_str(SCRIPT);
    body.push_str("</script>");
    page(&body)
}

fn page(body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"generator\" content=\"DuckLocal {}\">\
         <title>Query result</title><style>{STYLE}</style></head><body>{body}</body></html>",
        env!("CARGO_PKG_VERSION")
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

fn count(n: usize, noun: &str) -> String {
    let s = if n == 1 { "" } else { "s" };
    format!("{} {noun}{s}", group_digits(n))
}

fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, ch) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Light and dark both: the page follows the reader's system appearance.
const STYLE: &str = "
:root { color-scheme: light dark; --bg: #ffffff; --fg: #1d1d1f; --muted: #6e6e73;
  --line: #e5e5ea; --head: #f5f5f7; --null: #aeaeb2; --warn: #b25000; }
@media (prefers-color-scheme: dark) { :root { --bg: #1c1c1e; --fg: #f5f5f7; --muted: #98989d;
  --line: #3a3a3c; --head: #2c2c2e; --null: #636366; --warn: #ff9f0a; } }
body { margin: 0; padding: 16px; background: var(--bg); color: var(--fg);
  font: 14px system-ui, -apple-system, 'Segoe UI', sans-serif; }
h1 { font-size: 18px; margin: 0 0 4px; }
.meta { color: var(--muted); margin-bottom: 12px; }
.meta span + span::before { content: ' · '; }
details { margin-bottom: 12px; }
summary { cursor: pointer; color: var(--muted); }
pre { background: var(--head); border-radius: 6px; padding: 8px 10px; overflow-x: auto;
  font: 12px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
#filter { font: inherit; padding: 4px 8px; margin: 0 8px 8px 0; border: 1px solid var(--line);
  border-radius: 6px; background: var(--bg); color: var(--fg); max-width: 100%; }
.scroll { overflow: auto; max-height: 80vh; border: 1px solid var(--line); border-radius: 6px; }
table { border-collapse: collapse; white-space: nowrap;
  font: 12px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
th, td { padding: 4px 10px; border-bottom: 1px solid var(--line); text-align: left; }
th { background: var(--head); position: sticky; top: 0; padding: 0; }
th button { all: unset; display: block; box-sizing: border-box; width: 100%; padding: 4px 10px;
  cursor: pointer; font-weight: 600; }
th button:focus-visible { outline: 2px solid currentColor; outline-offset: -2px; }
th small { display: block; color: var(--muted); font-weight: 400; }
th[aria-sort=ascending] button::after { content: ' ▲'; }
th[aria-sort=descending] button::after { content: ' ▼'; }
.num { text-align: right; }
td.null { color: var(--null); font-style: italic; }
.note { color: var(--muted); }
.warn { color: var(--warn); }
";

/// Sort by a column (click again to reverse; NULLs stay last) and filter rows
/// by text. Numeric columns sort as numbers.
const SCRIPT: &str = r#"
(() => {
  const body = document.querySelector('tbody');
  const rows = Array.from(body.rows);
  const shown = document.getElementById('shown');
  const heads = document.querySelectorAll('th');
  const report = () => {
    const n = rows.filter(r => !r.hidden).length;
    shown.textContent = n === rows.length ? '' : n + ' of ' + rows.length + ' rows shown';
  };
  heads.forEach((th, col) => th.querySelector('button').addEventListener('click', () => {
    const dir = th.getAttribute('aria-sort') === 'ascending' ? -1 : 1;
    heads.forEach(h => h.setAttribute('aria-sort', 'none'));
    th.setAttribute('aria-sort', dir === 1 ? 'ascending' : 'descending');
    const num = th.classList.contains('num');
    const key = r => {
      const c = r.cells[col];
      if (c.classList.contains('null')) return null;
      return num ? parseFloat(c.textContent) : c.textContent;
    };
    rows.slice().sort((a, b) => {
      const x = key(a), y = key(b);
      if (x === null || y === null) return (x === null) - (y === null);
      if (num) return (x - y) * dir;
      return x.localeCompare(y, undefined, { numeric: true }) * dir;
    }).forEach(r => body.appendChild(r));
  }));
  document.getElementById('filter').addEventListener('input', e => {
    const q = e.target.value.toLowerCase();
    rows.forEach(r => { r.hidden = q !== '' && !r.textContent.toLowerCase().includes(q); });
    report();
  });
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::CliColumn;
    use serde_json::json;

    fn result(columns: &[(&str, &str)], rows: Vec<Vec<serde_json::Value>>) -> CliResult {
        CliResult {
            columns: columns
                .iter()
                .map(|(name, ty)| CliColumn {
                    name: (*name).to_string(),
                    arrow_type: (*ty).to_string(),
                })
                .collect(),
            row_count: rows.len(),
            total_rows: rows.len(),
            total_rows_exact: true,
            rows,
            truncated: false,
            elapsed_ms: 3,
        }
    }

    #[test]
    fn a_cell_cannot_become_markup() {
        let page = format_html(
            &result(
                &[("<b>", "Utf8"), ("n", "Int64")],
                vec![vec![json!("</td><script>x</script>"), json!(null)]],
            ),
            "SELECT '<b>'",
        );
        assert!(page.contains("&lt;b&gt;"), "{page}");
        assert!(
            page.contains("&lt;/td&gt;&lt;script&gt;x&lt;/script&gt;"),
            "{page}"
        );
        assert!(!page.contains("<script>x"), "{page}");
        assert!(page.contains("SELECT '&lt;b&gt;'"), "{page}");
        // A SQL NULL is marked as one, in a numeric column, so it sorts last.
        assert!(page.contains("<td class=\"num null\">NULL</td>"), "{page}");
        assert!(page.contains("<th class=\"num\""), "{page}");
    }

    #[test]
    fn the_page_needs_nothing_from_the_network() {
        let page = format_html(&result(&[("n", "Int64")], vec![vec![json!(1)]]), "SELECT 1");
        assert!(page.starts_with("<!doctype html>"));
        assert!(
            !page.contains("http://") && !page.contains("https://"),
            "{page}"
        );
        assert!(page.contains("<td class=\"num\">1</td>"));
        assert!(
            page.contains("1 row<") && page.contains("1 column<"),
            "{page}"
        );
    }

    #[test]
    fn a_cut_result_says_so() {
        let mut cut = result(&[("n", "Int64")], vec![vec![json!(1)]]);
        cut.truncated = true;
        cut.total_rows = 12_345;
        cut.total_rows_exact = false;
        let page = format_html(&cut, "SELECT 1");
        assert!(page.contains("1 of at least 12,345 rows"), "{page}");
        assert!(
            page.contains("Truncated at 1 of at least 12,345 rows"),
            "{page}"
        );

        let empty = format_html(&result(&[], vec![]), "CREATE TABLE t(a INT)");
        assert!(empty.contains("No columns were returned."), "{empty}");
        assert!(!empty.contains("<script>"), "{empty}");
    }
}
