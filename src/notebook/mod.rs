//! Notebooks: a `.sql` file read as a run of cells — SQL, Markdown, plots and
//! dashboards.
//!
//! A notebook is plain SQL with cell markers, the "percent" format Jupytext
//! and VS Code read: a line starting `-- %%` begins a cell, `-- %% [markdown]`
//! a Markdown one, `-- %% [plot]` a plot and `-- %% [dash]` a dashboard, whose
//! lines are SQL comments.
//!
//! ```text
//! -- %% [markdown]
//! -- # Orders review
//!
//! -- %% name=monthly
//! SELECT date_trunc('month', ts) AS month, sum(amount) AS revenue
//! FROM 'orders.parquet' GROUP BY 1 ORDER BY 1;
//!
//! -- %% [plot]
//! -- type = "line"
//! -- data = monthly
//! -- x = "month"
//! -- y = "revenue"
//! ```
//!
//! A plot cell holds the attributes of a `.dash` plot block, checked by the
//! same rules (see `plot`), with `data` naming the SQL cell it draws — by the
//! `name=` on its marker — or, left out, the nearest SQL cell above it. A dash
//! cell holds any `.dash` blocks — sources, queries, plots, filters — and is
//! drawn as a dashboard, its relative paths starting beside the notebook.
//!
//! The file stays a script: `duckdb < notebook.sql` runs it, Markdown, plots
//! and all, and it diffs like any other text. Results are not kept in it — a
//! result can be hundreds of megabytes, and rerunning is one keystroke.
//!
//! Any `.sql` file opens as a notebook. One with no markers is a single SQL
//! cell, and is written back without any for as long as it stays one, so
//! opening a plain script and saving it changes nothing but the edits.

use std::path::{Path, PathBuf};

use crate::i18n::trf;
use crate::spec::tabs::OpenDocument;

pub mod cli;
pub mod plot;

/// The extension a notebook carries.
pub const EXTENSION: &str = "sql";

/// The `settings` key the open notebooks are remembered under.
pub const SETTING: &str = "notebook_tabs";

/// What starts a cell.
const MARKER: &str = "-- %%";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Sql,
    Markdown,
    /// A chart of a SQL cell's result: `.dash` plot attributes, one a line.
    Plot,
    /// A dashboard of its own: any `.dash` blocks — sources, queries,
    /// plots, filters — drawn in place as the dashboard view draws a file.
    Dash,
}

impl CellKind {
    /// Whether the cell's lines are SQL comments in the file.
    fn commented(self) -> bool {
        matches!(self, CellKind::Markdown | CellKind::Plot | CellKind::Dash)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub kind: CellKind,
    /// The cell's text: SQL as written, or Markdown or plot attributes with
    /// the comment prefixes taken off.
    pub source: String,
    /// What a plot's `data` calls this cell: `name=` on its marker.
    pub name: Option<String>,
}

impl Cell {
    pub fn sql(source: impl Into<String>) -> Self {
        Self {
            kind: CellKind::Sql,
            source: source.into(),
            name: None,
        }
    }

    #[cfg(test)]
    pub fn markdown(source: impl Into<String>) -> Self {
        Self {
            kind: CellKind::Markdown,
            source: source.into(),
            name: None,
        }
    }

    #[cfg(test)]
    pub fn plot(source: impl Into<String>) -> Self {
        Self {
            kind: CellKind::Plot,
            source: source.into(),
            name: None,
        }
    }

    #[cfg(test)]
    pub fn named(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }
}

/// A notebook's cells, and whether the file it came from had no markers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub cells: Vec<Cell>,
    /// Read from a script without markers: written back the same way while
    /// it is still one SQL cell.
    pub plain: bool,
}

/// What a marker line says: the kind of cell it starts, and its name.
struct Marker {
    kind: CellKind,
    name: Option<String>,
}

/// Whether `text` can name a cell: a SQL identifier, as a `.dash` name is.
pub fn is_cell_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Read a marker line, or `None` when `line` is not one. Past `-- %%` comes
/// an optional title, `[kind]`, and `key=value` pairs; `name` is the one
/// pair read, and a title is ignored, as Jupytext's own reader does.
fn marker(line: &str) -> Option<Marker> {
    let rest = line.trim_start().strip_prefix(MARKER)?;
    // `-- %%%` is a comment that happens to start the same way.
    if rest.starts_with('%') {
        return None;
    }
    let tag = rest
        .find('[')
        .and_then(|open| Some(&rest[open + 1..open + rest[open..].find(']')?]))
        .map(|tag| tag.trim().to_ascii_lowercase());
    let kind = match tag.as_deref() {
        Some("markdown" | "md") => CellKind::Markdown,
        Some("plot") => CellKind::Plot,
        Some("dash") => CellKind::Dash,
        _ => CellKind::Sql,
    };
    let name = rest.split_whitespace().find_map(|word| {
        let value = word.strip_prefix("name=")?.trim_matches('"');
        is_cell_name(value).then(|| value.to_string())
    });
    Some(Marker { kind, name })
}

/// A commented line without its comment prefix: `-- text` is `text`, and a
/// bare `--` is a blank line.
fn uncomment(line: &str) -> &str {
    let trimmed = line.trim_start();
    match trimmed.strip_prefix("--") {
        Some(rest) => rest.strip_prefix(' ').unwrap_or(rest),
        None => line,
    }
}

/// Blank lines at either end of a cell are the space between cells, not
/// part of it.
fn trim_blank_lines(lines: &[&str]) -> String {
    let start = lines.iter().position(|line| !line.trim().is_empty());
    let end = lines.iter().rposition(|line| !line.trim().is_empty());
    match (start, end) {
        (Some(start), Some(end)) => lines[start..=end].join("\n"),
        _ => String::new(),
    }
}

/// Read a notebook's text.
pub fn parse(text: &str) -> Document {
    parse_located(text).0
}

/// `parse`, with the line (1-based) each cell's text starts on: its first
/// line that is not blank, or its marker's line when it has none. What
/// `ducklocal notebook` points an error at.
pub fn parse_located(text: &str) -> (Document, Vec<usize>) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut cells = Vec::new();
    let mut starts = Vec::new();
    // Text before the first marker is a SQL cell of its own, when there is
    // any: a header comment, or the whole of a plain script.
    let mut current = Marker {
        kind: CellKind::Sql,
        name: None,
    };
    let mut marker_line = 1;
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut plain = true;
    let mut flush =
        |marker: Marker, marker_line: usize, lines: &mut Vec<(usize, &str)>, leading: bool| {
            let text: Vec<&str> = lines
                .iter()
                .map(|(_, line)| {
                    if marker.kind.commented() {
                        uncomment(line)
                    } else {
                        line
                    }
                })
                .collect();
            let source = trim_blank_lines(&text);
            // Blank text before the first marker is spacing, not a cell; an
            // empty cell after a marker is one someone added.
            if !(leading && source.is_empty()) {
                let start = lines
                    .iter()
                    .find(|(_, line)| !line.trim().is_empty())
                    .map_or(marker_line, |(number, _)| *number);
                cells.push(Cell {
                    kind: marker.kind,
                    source,
                    // Only a SQL cell has a result to be named for.
                    name: marker.name.filter(|_| marker.kind == CellKind::Sql),
                });
                starts.push(start);
            }
            lines.clear();
        };
    let mut leading = true;
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if let Some(next) = marker(line) {
            let done = std::mem::replace(&mut current, next);
            flush(done, marker_line, &mut lines, leading);
            leading = false;
            plain = false;
            marker_line = number;
        } else {
            lines.push((number, line));
        }
    }
    flush(current, marker_line, &mut lines, leading);
    if cells.is_empty() {
        cells.push(Cell::sql(""));
        starts.push(1);
    }
    (Document { cells, plain }, starts)
}

/// Write a notebook's text.
pub fn serialize(doc: &Document) -> String {
    if let [cell] = doc.cells.as_slice() {
        if doc.plain && cell.kind == CellKind::Sql && cell.name.is_none() {
            return terminated(cell.source.clone());
        }
    }
    let mut out = String::new();
    for (ix, cell) in doc.cells.iter().enumerate() {
        if ix > 0 {
            out.push('\n');
        }
        out.push_str(MARKER);
        match cell.kind {
            CellKind::Sql => {}
            CellKind::Markdown => out.push_str(" [markdown]"),
            CellKind::Plot => out.push_str(" [plot]"),
            CellKind::Dash => out.push_str(" [dash]"),
        }
        if let Some(name) = cell.name.as_deref().filter(|name| is_cell_name(name)) {
            out.push_str(" name=");
            out.push_str(name);
        }
        out.push('\n');
        if cell.kind.commented() {
            for line in cell.source.trim_end().lines() {
                if line.is_empty() {
                    out.push_str("--\n");
                } else {
                    out.push_str("-- ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        } else if !cell.source.trim().is_empty() {
            out.push_str(cell.source.trim_end());
            out.push('\n');
        }
    }
    out
}

/// The SQL cell a plot at `plot_ix` draws: the one its `data` names, or the
/// nearest SQL cell above it. `Err` says why there is none.
pub fn data_cell(cells: &[Cell], plot_ix: usize, data: Option<&str>) -> Result<usize, String> {
    match data {
        Some(name) => cells
            .iter()
            .position(|cell| cell.kind == CellKind::Sql && cell.name.as_deref() == Some(name))
            .ok_or_else(|| crate::i18n::trf("notebook.plot.no_such_cell", &[name, name])),
        None => cells[..plot_ix.min(cells.len())]
            .iter()
            .rposition(|cell| cell.kind == CellKind::Sql)
            .ok_or_else(|| crate::i18n::tr("notebook.plot.no_cell_above").to_string()),
    }
}

fn terminated(mut text: String) -> String {
    let trimmed = text.trim_end().len();
    text.truncate(trimmed);
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// A path names a notebook when it is a file with the notebook's extension.
pub fn is_notebook(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(EXTENSION))
}

/// Pull the notebooks out of a path list, leaving the rest in order.
pub fn split(paths: Vec<String>) -> (Vec<PathBuf>, Vec<String>) {
    let mut notebooks = Vec::new();
    let mut rest = Vec::new();
    for path in paths {
        let expanded = PathBuf::from(crate::db::expand_tilde(&path));
        if is_notebook(&expanded) {
            notebooks.push(expanded);
        } else {
            rest.push(path);
        }
    }
    (notebooks, rest)
}

/// The default title of a notebook tab: the file's stem.
pub fn title_for(path: &Path) -> String {
    crate::spec::tabs::title_for(path)
}

/// The open notebooks still on disk, and one message per one that is not.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Restored {
    pub notebooks: Vec<OpenDocument>,
    pub problems: Vec<String>,
}

/// Write the open notebooks, replacing what was there.
pub fn remember(notebooks: &[OpenDocument]) {
    let json = crate::spec::tabs::to_json(notebooks);
    if let Err(error) = crate::history::set_setting(SETTING, &json) {
        tracing::warn!("Could not remember the open notebooks: {error}");
    }
}

/// The notebooks to reopen at launch, and what happened to the ones that
/// could not be.
pub fn restore() -> Restored {
    let saved = match crate::history::get_setting(SETTING) {
        Ok(Some(json)) => crate::spec::tabs::from_json(&json),
        Ok(None) => return Restored::default(),
        Err(error) => {
            tracing::warn!("Could not read the open notebooks: {error}");
            return Restored::default();
        }
    };
    let mut restored = Restored::default();
    for notebook in saved {
        if is_notebook(Path::new(&notebook.path)) {
            restored.notebooks.push(notebook);
        } else {
            restored.problems.push(trf(
                "notebook.restore.gone",
                &[&notebook.path, &notebook.title],
            ));
        }
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_script_is_one_cell_and_writes_back_unchanged() {
        let text = "SELECT 1;\n\nSELECT 2;\n";
        let doc = parse(text);
        assert_eq!(doc.cells, vec![Cell::sql("SELECT 1;\n\nSELECT 2;")]);
        assert!(doc.plain);
        assert_eq!(serialize(&doc), text);
    }

    #[test]
    fn markers_split_cells_and_markdown_loses_its_comment_prefix() {
        let text = "\
-- %% [markdown]
-- # Orders
--
-- Revenue by month.

-- %%
SELECT 1;

-- %% Totals
SELECT 2;
";
        let doc = parse(text);
        assert!(!doc.plain);
        assert_eq!(
            doc.cells,
            vec![
                Cell::markdown("# Orders\n\nRevenue by month."),
                Cell::sql("SELECT 1;"),
                Cell::sql("SELECT 2;"),
            ]
        );
    }

    #[test]
    fn a_notebook_round_trips() {
        let doc = Document {
            cells: vec![
                Cell::markdown("# Title\n\nSome *text*."),
                Cell::sql("SELECT 1;"),
                Cell::sql(""),
                Cell::sql("SELECT\n  2;"),
            ],
            plain: false,
        };
        let text = serialize(&doc);
        assert_eq!(
            text,
            "-- %% [markdown]\n-- # Title\n--\n-- Some *text*.\n\n-- %%\nSELECT 1;\n\n-- %%\n\n-- %%\nSELECT\n  2;\n"
        );
        assert_eq!(parse(&text), doc);
    }

    #[test]
    fn text_before_the_first_marker_is_a_cell_and_blank_text_is_not() {
        let doc = parse("-- header\n-- %%\nSELECT 1;\n");
        assert_eq!(
            doc.cells,
            vec![Cell::sql("-- header"), Cell::sql("SELECT 1;")]
        );
        let doc = parse("\n\n-- %%\nSELECT 1;\n");
        assert_eq!(doc.cells, vec![Cell::sql("SELECT 1;")]);
    }

    #[test]
    fn an_empty_file_is_one_empty_cell() {
        let doc = parse("");
        assert_eq!(doc.cells, vec![Cell::sql("")]);
        assert_eq!(serialize(&doc), "");
    }

    #[test]
    fn only_a_marker_line_starts_a_cell() {
        let kind = |line: &str| marker(line).map(|marker| marker.kind);
        assert_eq!(kind("-- %%"), Some(CellKind::Sql));
        assert_eq!(kind("  -- %% [md]"), Some(CellKind::Markdown));
        assert_eq!(kind("-- %% [Markdown] notes"), Some(CellKind::Markdown));
        assert_eq!(kind("-- %% [plot]"), Some(CellKind::Plot));
        assert_eq!(kind("-- %%% not a cell"), None);
        assert_eq!(kind("-- % not a cell"), None);
        assert_eq!(kind("SELECT '-- %%'"), None);
    }

    #[test]
    fn a_marker_names_its_cell_and_a_bad_name_is_none() {
        let name = |line: &str| marker(line).and_then(|marker| marker.name);
        assert_eq!(name("-- %% name=sales").as_deref(), Some("sales"));
        assert_eq!(
            name("-- %% Totals name=\"by_day\"").as_deref(),
            Some("by_day")
        );
        assert_eq!(name("-- %% name=1st"), None);
        assert_eq!(name("-- %% name=a-b"), None);
        assert_eq!(name("-- %%"), None);
    }

    #[test]
    fn plot_cells_and_names_round_trip() {
        let text = "\
-- %% name=sales
SELECT channel, revenue FROM t;

-- %% [plot]
-- type = \"bar\"
-- data = sales
-- x = \"channel\"
-- y = \"revenue\"
";
        let doc = parse(text);
        assert_eq!(
            doc.cells,
            vec![
                Cell::sql("SELECT channel, revenue FROM t;").named("sales"),
                Cell::plot("type = \"bar\"\ndata = sales\nx = \"channel\"\ny = \"revenue\""),
            ]
        );
        assert_eq!(serialize(&doc), text);
    }

    #[test]
    fn a_plot_draws_the_named_cell_or_the_nearest_sql_cell_above() {
        let cells = vec![
            Cell::sql("SELECT 1").named("first"),
            Cell::markdown("Between"),
            Cell::sql("SELECT 2"),
            Cell::plot(""),
        ];
        assert_eq!(data_cell(&cells, 3, None), Ok(2));
        assert_eq!(data_cell(&cells, 3, Some("first")), Ok(0));
        assert!(data_cell(&cells, 3, Some("missing")).is_err());
        assert!(data_cell(&cells, 0, None).is_err());
    }

    #[test]
    fn a_dash_cell_keeps_its_heredoc_and_indentation() {
        let text = "\
-- %% [dash]
-- source \"orders\" { path = \"orders.csv\" }
--
-- query \"q\" {
--   sql = <<SQL
--     SELECT 1 AS n -- one
--   SQL
-- }
";
        let doc = parse(text);
        assert_eq!(doc.cells.len(), 1);
        assert_eq!(doc.cells[0].kind, CellKind::Dash);
        assert_eq!(
            doc.cells[0].source,
            "source \"orders\" { path = \"orders.csv\" }\n\nquery \"q\" {\n  sql = <<SQL\n    SELECT 1 AS n -- one\n  SQL\n}"
        );
        assert_eq!(serialize(&doc), text);
    }

    #[test]
    fn a_named_single_cell_keeps_its_marker() {
        let mut doc = parse("SELECT 1;\n");
        doc.cells[0].name = Some("one".into());
        assert_eq!(serialize(&doc), "-- %% name=one\nSELECT 1;\n");
    }

    #[test]
    fn a_plain_script_that_grows_a_cell_gains_markers() {
        let mut doc = parse("SELECT 1;\n");
        doc.cells.push(Cell::markdown("Note"));
        assert_eq!(
            serialize(&doc),
            "-- %%\nSELECT 1;\n\n-- %% [markdown]\n-- Note\n"
        );
    }
}
