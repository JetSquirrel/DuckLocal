//! What Finder shows when Space is pressed on a data file: its columns and
//! first rows, rendered as one self-contained HTML page.
//!
//! The Swift preview extension (`quicklook/extension`) calls
//! [`ducklocal_preview_html`] and hands the page to Quick Look. Everything
//! runs in the extension's own sandboxed process, on an in-memory DuckDB:
//! the file is read, never written, and a `.duckdb` file is attached
//! read-only.
//!
//! A preview has to come back quickly — Quick Look gives up on a slow one —
//! so nothing here scans a whole file: rows are a `LIMIT`, and a row count
//! is shown only where the format keeps one (Parquet's footer, a database's
//! catalog estimate).

use std::ffi::{c_char, CStr, CString};
use std::path::Path;

use duckdb::Connection;

/// Rows the preview shows.
const PREVIEW_ROWS: usize = 50;
/// A longer cell is cut, so one wide JSON value cannot make the page huge.
const MAX_CELL_CHARS: usize = 200;

/// Render the preview of the file at `path` (UTF-8, NUL-terminated) as HTML.
/// Never NULL for a valid `path`: a file that cannot be read gets a page
/// saying why. The caller frees the result with [`ducklocal_preview_free`].
///
/// # Safety
/// `path` must be a valid NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn ducklocal_preview_html(path: *const c_char) -> *mut c_char {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    let path = CStr::from_ptr(path).to_string_lossy().into_owned();
    // A panic must not unwind into Swift.
    let html = std::panic::catch_unwind(|| preview_html(&path))
        .unwrap_or_else(|_| error_page(&path, "The preview failed unexpectedly."));
    CString::new(html.replace('\0', ""))
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// Free a page returned by [`ducklocal_preview_html`].
///
/// # Safety
/// `html` must come from [`ducklocal_preview_html`] and be freed once.
#[no_mangle]
pub unsafe extern "C" fn ducklocal_preview_free(html: *mut c_char) {
    if !html.is_null() {
        drop(CString::from_raw(html));
    }
}

/// The page for `path`, or an error page.
pub fn preview_html(path: &str) -> String {
    match build(path) {
        Ok(preview) => preview.html(),
        Err(e) => error_page(path, &e.to_string()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Parquet,
    Csv,
    Json,
    Database,
}

impl Format {
    fn of(path: &str) -> Option<Self> {
        let ext = Path::new(path)
            .extension()?
            .to_string_lossy()
            .to_lowercase();
        Some(match ext.as_str() {
            "parquet" => Format::Parquet,
            "csv" | "tsv" | "txt" => Format::Csv,
            "json" | "jsonl" | "ndjson" => Format::Json,
            "duckdb" | "ddb" => Format::Database,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            Format::Parquet => "Parquet",
            Format::Csv => "CSV",
            Format::Json => "JSON",
            Format::Database => "DuckDB",
        }
    }
}

struct Table {
    /// A database table's name; `None` for a data file.
    name: Option<String>,
    columns: Vec<(String, String)>,
    rows: Vec<Vec<Option<String>>>,
    /// Known without a scan, or not shown.
    row_count: Option<i64>,
}

struct Preview {
    file_name: String,
    format: Format,
    file_size: u64,
    /// Facts from the format's own metadata, as label/value pairs.
    facts: Vec<(String, String)>,
    tables: Vec<Table>,
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn build(path: &str) -> Result<Preview> {
    let format = Format::of(path).ok_or("DuckLocal does not preview this kind of file.")?;
    let file_size = std::fs::metadata(path)?.len();
    let conn = Connection::open_in_memory()?;
    // The sandbox has no network, and a preview has no business downloading.
    conn.execute_batch(
        "SET autoinstall_known_extensions = false; SET autoload_known_extensions = false;",
    )?;
    let mut facts = Vec::new();
    let tables = match format {
        Format::Parquet => {
            let (rows, groups, created_by): (i64, i64, Option<String>) = conn.query_row(
                &format!(
                    "SELECT num_rows, num_row_groups, created_by \
                     FROM parquet_file_metadata({})",
                    literal(path)
                ),
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            facts.push(("Row groups".into(), group_digits(groups)));
            let compression: Option<String> = conn
                .query_row(
                    &format!(
                        "SELECT string_agg(DISTINCT compression, ', ') \
                         FROM parquet_metadata({})",
                        literal(path)
                    ),
                    [],
                    |r| r.get(0),
                )
                .ok()
                .flatten();
            if let Some(compression) = compression {
                facts.push(("Compression".into(), compression));
            }
            if let Some(created_by) = created_by {
                facts.push(("Written by".into(), created_by));
            }
            let mut table = read_table(&conn, &format!("read_parquet({})", literal(path)), None)?;
            table.row_count = Some(rows);
            vec![table]
        }
        Format::Csv => vec![read_table(
            &conn,
            &format!("read_csv_auto({})", literal(path)),
            None,
        )?],
        Format::Json => vec![read_table(
            &conn,
            &format!("read_json_auto({})", literal(path)),
            None,
        )?],
        Format::Database => {
            conn.execute_batch(&format!("ATTACH {} AS f (READ_ONLY)", literal(path)))?;
            let version: Option<String> = conn
                .query_row(
                    "SELECT tags['storage_version'] FROM duckdb_databases() \
                     WHERE database_name = 'f'",
                    [],
                    |r| r.get(0),
                )
                .ok()
                .flatten();
            if let Some(version) = version {
                facts.push(("Readable by".into(), format!("DuckDB {version}")));
            }
            let mut stmt = conn.prepare(
                "SELECT schema_name, table_name, estimated_size FROM duckdb_tables() \
                 WHERE database_name = 'f' ORDER BY estimated_size DESC, table_name",
            )?;
            let listed: Vec<(String, String, Option<i64>)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<std::result::Result<_, _>>()?;
            facts.push(("Tables".into(), listed.len().to_string()));
            // The largest few, each with its first rows: what a glance at a
            // database is for.
            listed
                .into_iter()
                .take(5)
                .map(|(schema, name, estimated)| {
                    let relation = format!("f.{}.{}", quote(&schema), quote(&name));
                    let shown = if schema == "main" {
                        name
                    } else {
                        format!("{schema}.{name}")
                    };
                    let mut table = read_table(&conn, &relation, Some(shown))?;
                    table.row_count = estimated;
                    Ok(table)
                })
                .collect::<Result<Vec<_>>>()?
        }
    };
    Ok(Preview {
        file_name: Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string()),
        format,
        file_size,
        facts,
        tables,
    })
}

/// The columns and first rows of `relation`, every value as text.
fn read_table(conn: &Connection, relation: &str, name: Option<String>) -> Result<Table> {
    let mut describe = conn.prepare(&format!("DESCRIBE SELECT * FROM {relation}"))?;
    let columns: Vec<(String, String)> = describe
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut rows = Vec::new();
    if !columns.is_empty() {
        let select = columns
            .iter()
            .map(|(name, _)| format!("CAST({} AS VARCHAR)", quote(name)))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt =
            conn.prepare(&format!("SELECT {select} FROM {relation} LIMIT {PREVIEW_ROWS}"))?;
        let mut result = stmt.query([])?;
        while let Some(row) = result.next()? {
            rows.push(
                (0..columns.len())
                    .map(|ix| row.get::<_, Option<String>>(ix))
                    .collect::<std::result::Result<_, _>>()?,
            );
        }
    }
    Ok(Table {
        name,
        columns,
        rows,
        row_count: None,
    })
}

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

fn group_digits(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (ix, ch) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// Light and dark both: Quick Look renders the page in the Finder's
/// appearance.
const STYLE: &str = "
:root { color-scheme: light dark; --fg: #1d1d1f; --muted: #6e6e73; --line: #e5e5ea;
  --head: #f5f5f7; --null: #aeaeb2; }
@media (prefers-color-scheme: dark) { :root { --fg: #f5f5f7; --muted: #98989d;
  --line: #3a3a3c; --head: #2c2c2e; --null: #636366; } }
body { margin: 0; padding: 16px 20px; font: 13px -apple-system, BlinkMacSystemFont, sans-serif;
  color: var(--fg); }
h1 { font-size: 17px; margin: 0 0 4px; }
.meta { color: var(--muted); margin-bottom: 14px; }
.meta span + span::before { content: ' · '; }
h2 { font-size: 14px; margin: 18px 0 6px; }
.scroll { overflow-x: auto; border: 1px solid var(--line); border-radius: 6px; }
table { border-collapse: collapse; font: 12px ui-monospace, SFMono-Regular, Menlo, monospace;
  white-space: nowrap; }
th, td { padding: 4px 10px; border-bottom: 1px solid var(--line); text-align: left; }
th { background: var(--head); position: sticky; top: 0; font-weight: 600; }
th small { display: block; color: var(--muted); font-weight: 400; }
td.n { color: var(--muted); text-align: right; }
td.null { color: var(--null); font-style: italic; }
.note { color: var(--muted); margin-top: 6px; }
.error { color: #d70015; }
";

impl Preview {
    fn html(&self) -> String {
        let mut meta = vec![
            self.format.label().to_string(),
            human_size(self.file_size),
        ];
        if let [table] = self.tables.as_slice() {
            if let Some(rows) = table.row_count {
                meta.push(format!("{} rows", group_digits(rows)));
            }
            meta.push(format!("{} columns", table.columns.len()));
        }
        meta.extend(self.facts.iter().map(|(k, v)| format!("{k}: {v}")));

        let mut body = format!(
            "<h1>{}</h1><div class=\"meta\">{}</div>",
            escape(&self.file_name),
            meta.iter()
                .map(|m| format!("<span>{}</span>", escape(m)))
                .collect::<String>()
        );
        if self.tables.is_empty() {
            body.push_str("<p class=\"note\">No tables.</p>");
        }
        for table in &self.tables {
            if let Some(name) = &table.name {
                let count = table
                    .row_count
                    .map(|n| format!(" <small>≈ {} rows</small>", group_digits(n)))
                    .unwrap_or_default();
                body.push_str(&format!("<h2>{}{count}</h2>", escape(name)));
            }
            body.push_str(&table_html(table));
        }
        page(&escape(&self.file_name), &body)
    }
}

fn table_html(table: &Table) -> String {
    let mut out = String::from("<div class=\"scroll\"><table><thead><tr><th></th>");
    for (name, data_type) in &table.columns {
        out.push_str(&format!(
            "<th>{}<small>{}</small></th>",
            escape(name),
            escape(data_type)
        ));
    }
    out.push_str("</tr></thead><tbody>");
    for (ix, row) in table.rows.iter().enumerate() {
        out.push_str(&format!("<tr><td class=\"n\">{}</td>", ix + 1));
        for cell in row {
            match cell {
                None => out.push_str("<td class=\"null\">NULL</td>"),
                Some(value) => {
                    let shown: String = value.chars().take(MAX_CELL_CHARS).collect();
                    let cut = if shown.len() < value.len() { "…" } else { "" };
                    out.push_str(&format!("<td>{}{cut}</td>", escape(&shown)));
                }
            }
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
    let more = table
        .row_count
        .is_none_or(|n| n > table.rows.len() as i64);
    if table.rows.len() == PREVIEW_ROWS && more {
        out.push_str(&format!(
            "<p class=\"note\">First {PREVIEW_ROWS} rows. Open in DuckLocal for the rest.</p>"
        ));
    }
    out
}

fn error_page(path: &str, message: &str) -> String {
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    page(
        &escape(&name),
        &format!(
            "<h1>{}</h1><p class=\"error\">{}</p>",
            escape(&name),
            escape(message)
        ),
    )
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
         <style>{STYLE}</style></head><body>{body}</body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("ducklocal_quicklook_tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn previews_a_parquet_file_with_its_footer_facts() {
        let path = dir().join("orders.parquet");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT range AS id, 'a<b' AS note, NULL::INT AS gone FROM range(120)) \
             TO {} (FORMAT parquet)",
            literal(path.to_str().unwrap())
        ))
        .unwrap();
        let html = preview_html(path.to_str().unwrap());
        assert!(html.contains("orders.parquet"), "{html}");
        assert!(html.contains("120 rows"), "{html}");
        assert!(html.contains("Row groups"), "{html}");
        // Escaped, never raw markup from the data.
        assert!(html.contains("a&lt;b") && !html.contains("a<b"), "{html}");
        assert!(html.contains("class=\"null\""));
        assert!(html.contains("First 50 rows"));
        assert_eq!(html.matches("<tr><td class=\"n\">").count(), PREVIEW_ROWS);
    }

    #[test]
    fn previews_csv_json_and_a_database_read_only() {
        let csv = dir().join("people.csv");
        std::fs::write(&csv, "name,age\nAda,36\nGrace,45\n").unwrap();
        let html = preview_html(csv.to_str().unwrap());
        assert!(html.contains("Grace") && html.contains("BIGINT"), "{html}");
        assert!(!html.contains("First 50 rows"));

        let json = dir().join("events.jsonl");
        std::fs::write(&json, "{\"k\":1}\n{\"k\":2}\n").unwrap();
        assert!(preview_html(json.to_str().unwrap()).contains("<td>2</td>"));

        let db = dir().join("shop.duckdb");
        let _ = std::fs::remove_file(&db);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE orders AS SELECT range AS id FROM range(10); \
             CREATE SCHEMA staging; CREATE TABLE staging.raw (x INT);",
        )
        .unwrap();
        drop(conn);
        let before = std::fs::read(&db).unwrap();
        let html = preview_html(db.to_str().unwrap());
        assert!(html.contains("<h2>orders") && html.contains("staging.raw"), "{html}");
        assert!(html.contains("Readable by"), "{html}");
        assert_eq!(std::fs::read(&db).unwrap(), before, "a preview never writes");
    }

    #[test]
    fn an_unreadable_file_gets_a_page_saying_why() {
        let bad = dir().join("broken.parquet");
        std::fs::write(&bad, "not parquet").unwrap();
        let html = preview_html(bad.to_str().unwrap());
        assert!(html.contains("class=\"error\""), "{html}");
        assert!(preview_html("/no/such/file.csv").contains("class=\"error\""));
    }

    #[test]
    fn the_c_entry_point_round_trips() {
        let csv = dir().join("ffi.csv");
        std::fs::write(&csv, "a\n1\n").unwrap();
        let path = CString::new(csv.to_str().unwrap()).unwrap();
        unsafe {
            let html = ducklocal_preview_html(path.as_ptr());
            assert!(!html.is_null());
            assert!(CStr::from_ptr(html).to_string_lossy().contains("<table>"));
            ducklocal_preview_free(html);
        }
    }
}
