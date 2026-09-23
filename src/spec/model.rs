//! What the tree means: two block kinds, a fixed set of attributes, and
//! references that must resolve.
//!
//! The rules a `.dash` file lives by:
//!
//! * a `query` block holds one `sql` attribute — one statement, nothing else;
//! * a `plot` block holds `type`, `query`, `x`, and (unless a table) `y`,
//!   plus an optional `series` and `title`;
//! * `query = query.latency` names a query block that exists;
//! * `x`, `y`, `series` name result columns — as bare identifiers when the
//!   column allows it, as strings when it does not (`"Revenue (USD)"`).
//!
//! Everything here is checked without a database. What a column name resolves
//! *to* — whether the query returns it, whether `y` is numeric — needs a
//! connection, and that half lives in `mod.rs` behind `--database`.

use std::collections::HashSet;

use super::syntax::{self, Attr, Block, File, RefSite, Value};

/// The plot types the format knows. Each is a promise the renderer can keep:
/// x/y for the continuous ones, anything tabular for `table`.
pub(crate) const PLOT_TYPES: &[&str] = &["line", "bar", "area", "scatter", "table"];

#[derive(Debug, Clone)]
pub(crate) struct Spec {
    pub queries: Vec<Query>,
    pub plots: Vec<Plot>,
    /// Where every block and attribute sits in the source, in source order,
    /// for `Spec::locate`.
    #[allow(dead_code)] // read by locate(), which the editor phases wire up
    sites: Vec<Site>,
}

#[derive(Debug, Clone)]
pub(crate) struct Query {
    pub name: String,
    pub sql: String,
    /// Line and column of the block's kind keyword.
    pub line: usize,
    pub col: usize,
    /// The kind keyword's span: where the block starts.
    pub span: (usize, usize),
    /// The quoted name's span, quotes included: the definition target.
    pub name_span: (usize, usize),
}

#[derive(Debug, Clone)]
pub(crate) struct Plot {
    pub name: String,
    pub kind: String,
    /// The name of the query block this plot draws.
    pub query: String,
    pub x: String,
    pub y: Option<String>,
    pub series: Option<String>,
    pub title: Option<String>,
    /// Line and column of the block's kind keyword.
    pub line: usize,
    pub col: usize,
    /// The kind keyword's span: where the block starts.
    pub span: (usize, usize),
    /// The quoted name's span, quotes included: the definition target.
    pub name_span: (usize, usize),
}

/// One thing wrong with the file, with its line, column, and span. Semantic
/// checking collects rather than fails fast: a dashboard being written has
/// all its mistakes worth saying in one pass.
#[derive(Debug, Clone)]
pub(crate) struct Diagnostic {
    pub line: usize,
    #[allow(dead_code)] // carried for the editor phases; text output uses line
    pub col: usize,
    #[allow(dead_code)] // carried for the editor phases; text output uses line
    pub span: (usize, usize),
    pub message: String,
}

impl Diagnostic {
    /// At an attribute's name: the precise thing that is wrong.
    fn at_attr(attr: &Attr, message: impl Into<String>) -> Self {
        Self {
            line: attr.line,
            col: attr.col,
            span: attr.name_span,
            message: message.into(),
        }
    }

    /// At a block's kind keyword: the fallback when the block as a whole is
    /// to blame and nothing inside it can be pointed at.
    fn at_block(block: &Block, message: impl Into<String>) -> Self {
        Self {
            line: block.line,
            col: block.col,
            span: block.kind_span,
            message: message.into(),
        }
    }
}

/// Where one block and its attributes sit in the source: the data
/// `Spec::locate` scans. Built from the syntax tree, kept in source order.
#[derive(Debug, Clone)]
#[allow(dead_code)] // read by locate(), which the editor phases wire up
struct Site {
    kind: String,
    name: String,
    name_span: (usize, usize),
    /// Line and column of the kind keyword.
    line: usize,
    col: usize,
    /// Line of the closing brace: with `line`, the block's extent.
    end_line: usize,
    attrs: Vec<AttrSite>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // read by locate(), which the editor phases wire up
struct AttrSite {
    name: String,
    /// Line and column of the attribute's name: its column range is
    /// `col .. col + name.len()` (names are ASCII identifiers).
    line: usize,
    col: usize,
    refs: Vec<RefSite>,
}

/// What a position in the source points at: the innermost named thing under
/// the cursor, for the editor features (hover, definition, completion) that
/// build on this.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // produced by locate(), which the editor phases wire up
pub(crate) enum Hit {
    /// On one segment of a reference like `query.latency`: the block and
    /// attribute holding it, every segment's name, which segment the cursor
    /// is on, and that segment's byte span.
    Ref {
        block: String,
        attr: String,
        segments: Vec<String>,
        segment: usize,
        span: (usize, usize),
    },
    /// On an attribute's name.
    Attr { block: String, name: String },
    /// Anywhere else inside a block — header, values, heredocs, whitespace.
    /// The name span is the block's definition target.
    Block {
        kind: String,
        name: String,
        name_span: (usize, usize),
    },
}

impl Spec {
    /// The innermost named thing at `(line, col)` — both 1-based, columns in
    /// characters — or `None` outside every block. A linear scan; spec files
    /// are small. Usage:
    ///
    /// ```text
    /// // cursor on `latency` of `query = query.latency` inside plot "p":
    /// spec.locate(11, 20)
    /// // → Some(Hit::Ref { block: "p", attr: "query",
    /// //      segments: ["query", "latency"], segment: 1, span })
    /// // cursor on an attribute name:        Some(Hit::Attr { .. })
    /// // elsewhere inside a block:           Some(Hit::Block { .. })
    /// // between or outside blocks:          None
    /// ```
    #[allow(dead_code)] // the editor phases (GUI editing, LSP) call this
    pub(crate) fn locate(&self, line: usize, col: usize) -> Option<Hit> {
        let site = self
            .sites
            .iter()
            .find(|s| (s.line, s.col) <= (line, col) && line <= s.end_line)?;
        for attr in &site.attrs {
            if attr.line == line && col >= attr.col && col < attr.col + attr.name.len() {
                return Some(Hit::Attr {
                    block: site.name.clone(),
                    name: attr.name.clone(),
                });
            }
            for reference in &attr.refs {
                for (index, segment) in reference.segments.iter().enumerate() {
                    if segment.line == line
                        && col >= segment.col
                        && col < segment.col + segment.name.len()
                    {
                        return Some(Hit::Ref {
                            block: site.name.clone(),
                            attr: attr.name.clone(),
                            segments: reference
                                .segments
                                .iter()
                                .map(|s| s.name.clone())
                                .collect(),
                            segment: index,
                            span: segment.span,
                        });
                    }
                }
            }
        }
        Some(Hit::Block {
            kind: site.kind.clone(),
            name: site.name.clone(),
            name_span: site.name_span,
        })
    }
}

pub(crate) fn validate(file: &File) -> Result<Spec, Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    let mut queries = Vec::new();
    let mut plots = Vec::new();
    let mut sites = Vec::new();
    let mut query_names: HashSet<String> = HashSet::new();
    let mut plot_names: HashSet<String> = HashSet::new();

    for block in &file.blocks {
        match block.kind.as_str() {
            "query" => {
                if !query_names.insert(block.name.clone()) {
                    diagnostics.push(Diagnostic::at_block(
                        block,
                        format!("Duplicate query name: {:?}", block.name),
                    ));
                }
                if let Some(q) = query(block, &mut diagnostics) {
                    sites.push(site(block, q.name_span));
                    queries.push(q);
                }
            }
            "plot" => {
                if !plot_names.insert(block.name.clone()) {
                    diagnostics.push(Diagnostic::at_block(
                        block,
                        format!("Duplicate plot name: {:?}", block.name),
                    ));
                }
                if let Some(p) = plot(block, &mut diagnostics) {
                    sites.push(site(block, p.name_span));
                    plots.push(p);
                }
            }
            other => diagnostics.push(Diagnostic::at_block(
                block,
                format!("Unknown block: {other:?}; the file holds query and plot blocks"),
            )),
        }
    }

    // References resolve against the names collected above — including a query
    // that was itself flawed, so one mistake does not cascade into another.
    // Plots were built in block order, one per plot block: walking the blocks
    // again pairs each plot with its own `query` attribute to point at.
    let mut plot_iter = plots.iter();
    for block in &file.blocks {
        if block.kind != "plot" {
            continue;
        }
        let plot = plot_iter.next().expect("one plot per plot block");
        if !plot.query.is_empty() && !query_names.contains(&plot.query) {
            let message = format!(
                "plot {:?} draws query {:?}, which the file does not define",
                plot.name, plot.query
            );
            let attr = block.attrs.iter().find(|a| a.name == "query");
            diagnostics.push(match attr {
                Some(attr) => Diagnostic::at_attr(attr, message),
                None => Diagnostic {
                    line: plot.line,
                    col: plot.col,
                    span: plot.span,
                    message,
                },
            });
        }
    }

    if diagnostics.is_empty() {
        Ok(Spec {
            queries,
            plots,
            sites,
        })
    } else {
        Err(diagnostics)
    }
}

fn site(block: &Block, name_span: (usize, usize)) -> Site {
    Site {
        kind: block.kind.clone(),
        name: block.name.clone(),
        name_span,
        line: block.line,
        col: block.col,
        end_line: block.end_line,
        attrs: block
            .attrs
            .iter()
            .map(|attr| AttrSite {
                name: attr.name.clone(),
                line: attr.line,
                col: attr.col,
                refs: attr.refs.clone(),
            })
            .collect(),
    }
}

fn query(block: &Block, diagnostics: &mut Vec<Diagnostic>) -> Option<Query> {
    let mut sql = None;
    let mut seen = false;
    for attr in &block.attrs {
        match attr.name.as_str() {
            "sql" => {
                seen = true;
                match &attr.value {
                    Value::Heredoc(text) | Value::Str(text) => sql = Some(text.clone()),
                    _ => diagnostics.push(Diagnostic::at_attr(
                        attr,
                        "sql is a heredoc or a string",
                    )),
                }
            }
            other => diagnostics.push(Diagnostic::at_attr(
                attr,
                format!("A query block holds only sql; unknown attribute: {other}"),
            )),
        }
    }
    if !seen {
        diagnostics.push(Diagnostic::at_block(
            block,
            format!("query block {:?} requires sql", block.name),
        ));
    }
    // A flawed block still joins the model — with an empty sql — so that one
    // mistake does not cascade into every plot that references it. Nothing
    // runs while diagnostics are pending.
    Some(Query {
        name: block.name.clone(),
        sql: sql.unwrap_or_default(),
        line: block.line,
        col: block.col,
        span: block.kind_span,
        name_span: block.name_span,
    })
}

fn plot(block: &Block, diagnostics: &mut Vec<Diagnostic>) -> Option<Plot> {
    let mut kind = None;
    let mut query = None;
    let mut x = None;
    let mut y = None;
    let mut series = None;
    let mut title = None;
    let mut seen: HashSet<&str> = HashSet::new();
    for attr in &block.attrs {
        if !seen.insert(attr.name.as_str()) {
            diagnostics.push(Diagnostic::at_attr(
                attr,
                format!("Duplicate attribute: {}", attr.name),
            ));
            continue;
        }
        match attr.name.as_str() {
            "type" => match string(attr, diagnostics) {
                Some(text) if PLOT_TYPES.contains(&text.as_str()) => kind = Some(text),
                Some(text) => diagnostics.push(Diagnostic::at_attr(
                    attr,
                    format!(
                        "Unknown plot type: {text:?}; one of {}",
                        PLOT_TYPES.join(", ")
                    ),
                )),
                None => {}
            },
            "query" => match &attr.value {
                Value::Ref(segments)
                    if segments.len() == 2 && segments[0] == "query" =>
                {
                    query = Some(segments[1].clone())
                }
                _ => diagnostics.push(Diagnostic::at_attr(
                    attr,
                    "query names a query block: query = query.some_name",
                )),
            },
            "x" => x = column(attr, diagnostics),
            "y" => y = column(attr, diagnostics),
            "series" => series = column(attr, diagnostics),
            "title" => title = string(attr, diagnostics),
            other => diagnostics.push(Diagnostic::at_attr(
                attr,
                format!(
                    "A plot block holds type, query, x, y, series, title; unknown attribute: {other}"
                ),
            )),
        }
    }
    for name in ["type", "query", "x"] {
        if !seen.contains(name) {
            diagnostics.push(Diagnostic::at_block(
                block,
                format!("plot block {:?} requires {name}", block.name),
            ));
        }
    }
    // A present-but-invalid type already earned its own diagnostic; "requires"
    // is only for attributes never written. y is required once the type is
    // known to need one.
    let kind = kind.unwrap_or_default();
    if !kind.is_empty() && kind != "table" && y.is_none() {
        diagnostics.push(Diagnostic::at_block(
            block,
            format!("plot block {:?} requires y", block.name),
        ));
    }
    Some(Plot {
        name: block.name.clone(),
        kind,
        query: query.unwrap_or_default(),
        x: x.unwrap_or_default(),
        y,
        series,
        title,
        line: block.line,
        col: block.col,
        span: block.kind_span,
        name_span: block.name_span,
    })
}

/// A column name, written as a bare identifier or — for the names SQL quoting
/// exists for — a string.
fn column(attr: &Attr, diagnostics: &mut Vec<Diagnostic>) -> Option<String> {
    match &attr.value {
        Value::Ref(segments) if segments.len() == 1 => Some(segments[0].clone()),
        Value::Ref(_) => {
            diagnostics.push(Diagnostic::at_attr(
                attr,
                format!("{} names a column of the plot's query, not a reference", attr.name),
            ));
            None
        }
        Value::Str(text) => Some(text.clone()),
        _ => {
            diagnostics.push(Diagnostic::at_attr(
                attr,
                format!("{} names a column: an identifier or a string", attr.name),
            ));
            None
        }
    }
}

fn string(attr: &Attr, diagnostics: &mut Vec<Diagnostic>) -> Option<String> {
    match &attr.value {
        Value::Str(text) => Some(text.clone()),
        _ => {
            diagnostics.push(Diagnostic::at_attr(
                attr,
                format!("{} is a string in quotes", attr.name),
            ));
            None
        }
    }
}

/// A query's result columns as DuckDB describes them: `(name, type)` per
/// column, or why the describe failed.
pub(crate) type ColumnLookup<'a> = dyn Fn(&str) -> Result<Vec<(String, String)>, String> + 'a;

/// Column names DuckDB reports for a query, checked against what the plots
/// ask for. This is the half of checking that needs a live connection.
pub(crate) fn check_columns(spec: &Spec, columns: &ColumnLookup) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for plot in &spec.plots {
        let Some(query) = spec.queries.iter().find(|q| q.name == plot.query) else {
            continue; // already reported by validate()
        };
        let available = match columns(&query.name) {
            Ok(columns) => columns,
            Err(message) => {
                diagnostics.push(Diagnostic {
                    line: query.line,
                    col: query.col,
                    span: query.span,
                    message: format!("query {:?}: {message}", query.name),
                });
                continue;
            }
        };
        let has = |name: &str| {
            available
                .iter()
                .any(|(column, _)| column.eq_ignore_ascii_case(name))
        };
        for (name, column) in [
            ("x", Some(plot.x.as_str())),
            ("y", plot.y.as_deref()),
            ("series", plot.series.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, column)| column.map(|c| (name, c)))
        {
            if !has(column) {
                diagnostics.push(Diagnostic {
                    line: plot.line,
                    col: plot.col,
                    span: plot.span,
                    message: format!(
                        "plot {:?} uses {name} = {column:?}, which query {:?} does not return; it returns: {}",
                        plot.name,
                        query.name,
                        available
                            .iter()
                            .map(|(name, _)| name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
        }
        // A plotted y that is not a number draws nothing worth looking at.
        if plot.kind != "table" {
            if let Some(y) = &plot.y {
                if let Some((_, ty)) = available
                    .iter()
                    .find(|(column, _)| column.eq_ignore_ascii_case(y))
                {
                    if !is_numeric(ty) {
                        diagnostics.push(Diagnostic {
                            line: plot.line,
                            col: plot.col,
                            span: plot.span,
                            message: format!(
                                "plot {:?} draws y = {y:?} ({ty}), which is not numeric",
                                plot.name
                            ),
                        });
                    }
                }
            }
        }
    }
    diagnostics
}

fn is_numeric(ty: &str) -> bool {
    let ty = ty.to_ascii_uppercase();
    [
        "TINYINT", "SMALLINT", "INTEGER", "BIGINT", "HUGEINT", "UTINYINT", "USMALLINT",
        "UINTEGER", "UBIGINT", "UHUGEINT", "FLOAT", "DOUBLE", "DECIMAL",
    ]
    .iter()
    .any(|prefix| ty.starts_with(prefix))
}

/// Keep the syntax module's types reachable as one tree for callers.
pub(crate) fn parse(source: &str) -> Result<File, syntax::SyntaxError> {
    syntax::parse(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate_ok(source: &str) -> Spec {
        let file = syntax::parse(source).unwrap();
        match validate(&file) {
            Ok(spec) => spec,
            Err(diagnostics) => panic!(
                "{}",
                diagnostics
                    .iter()
                    .map(|d| format!("line {}: {}", d.line, d.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        }
    }

    fn validate_err(source: &str) -> Vec<Diagnostic> {
        let file = syntax::parse(source).unwrap();
        validate(&file).expect_err("the spec should not validate")
    }

    const GOOD: &str = r#"
query "latency" {
  sql = <<SQL
    SELECT timestamp, service, avg(latency) AS latency FROM logs
    GROUP BY timestamp, service
  SQL
}

plot "latency" {
  type   = "line"
  query  = query.latency
  x      = timestamp
  y      = latency
  series = service
  title  = "Latency by service"
}

plot "raw" {
  type  = "table"
  query = query.latency
  x     = timestamp
}
"#;

    #[test]
    fn a_good_spec_validates() {
        let spec = validate_ok(GOOD);
        assert_eq!(spec.queries.len(), 1);
        assert_eq!(spec.plots.len(), 2);
        assert_eq!(spec.plots[0].kind, "line");
        assert_eq!(spec.plots[0].query, "latency");
        assert_eq!(spec.plots[1].y, None, "a table needs no y");

        // Blocks carry their kind keyword's position and their name's span.
        let query = &spec.queries[0];
        assert_eq!((query.line, query.col), (2, 1));
        assert_eq!(&GOOD[query.span.0..query.span.1], "query");
        assert_eq!(&GOOD[query.name_span.0..query.name_span.1], "\"latency\"");
        let raw = &spec.plots[1];
        assert_eq!(&GOOD[raw.span.0..raw.span.1], "plot");
        assert_eq!(&GOOD[raw.name_span.0..raw.name_span.1], "\"raw\"");
    }

    #[test]
    fn every_mistake_is_said_in_one_pass() {
        let source = r#"
query "a" { sql = "SELECT 1" }
query "a" { sql = "SELECT 2" }
plot "p" {
  type  = "pie"
  query = query.missing
  x     = timestamp
}
plot "q" {
  type  = "line"
  query = query.a
}
wat "huh" {}
"#;
        let diagnostics = validate_err(source);
        let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("Duplicate query")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("\"pie\"")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("does not define")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("requires x")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("requires y")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("Unknown block")), "{messages:?}");

        // Each diagnostic points: attribute mistakes at the attribute's name,
        // block mistakes at the kind keyword.
        let duplicate = diagnostics
            .iter()
            .find(|d| d.message.contains("Duplicate query"))
            .unwrap();
        assert_eq!((duplicate.line, duplicate.col), (3, 1));
        assert_eq!(&source[duplicate.span.0..duplicate.span.1], "query");

        let pie = diagnostics
            .iter()
            .find(|d| d.message.contains("\"pie\""))
            .unwrap();
        assert_eq!((pie.line, pie.col), (5, 3));
        assert_eq!(&source[pie.span.0..pie.span.1], "type");

        let missing = diagnostics
            .iter()
            .find(|d| d.message.contains("does not define"))
            .unwrap();
        assert_eq!((missing.line, missing.col), (6, 3));
        assert_eq!(&source[missing.span.0..missing.span.1], "query");

        let unknown = diagnostics
            .iter()
            .find(|d| d.message.contains("Unknown block"))
            .unwrap();
        assert_eq!((unknown.line, unknown.col), (13, 1));
        assert_eq!(&source[unknown.span.0..unknown.span.1], "wat");
    }

    #[test]
    fn diagnostics_carry_columns_and_spans_with_heredocs() {
        let source = r#"
query "q" {
  sql = <<SQL
    SELECT 1
  SQL
  bogus = 1
}
plot "p" { type = "line" query = query.q x = timestamp }
"#;
        let diagnostics = validate_err(source);
        // The unknown attribute below the heredoc points at its own name.
        let bogus = diagnostics
            .iter()
            .find(|d| d.message.contains("unknown attribute: bogus"))
            .unwrap();
        assert_eq!((bogus.line, bogus.col), (6, 3));
        assert_eq!(&source[bogus.span.0..bogus.span.1], "bogus");
        // A whole-block complaint points at the kind keyword.
        let requires_y = diagnostics
            .iter()
            .find(|d| d.message.contains("requires y"))
            .unwrap();
        assert_eq!((requires_y.line, requires_y.col), (8, 1));
        assert_eq!(&source[requires_y.span.0..requires_y.span.1], "plot");
    }

    #[test]
    fn locate_finds_blocks_attrs_and_refs() {
        // GOOD's layout, 1-based: the query block spans lines 2-7 (heredoc
        // included), plot "latency" lines 9-16, plot "raw" lines 18-22.
        let spec = validate_ok(GOOD);

        // On `latency` of `query  = query.latency` (line 11): the second
        // segment of the reference in plot "latency"'s query attribute.
        let Some(Hit::Ref {
            block,
            attr,
            segments,
            segment,
            span,
        }) = spec.locate(11, 20)
        else {
            panic!("a reference segment: {:?}", spec.locate(11, 20));
        };
        assert_eq!((block.as_str(), attr.as_str(), segment), ("latency", "query", 1));
        assert_eq!(segments, ["query", "latency"]);
        assert_eq!(&GOOD[span.0..span.1], "latency");

        // On the first segment, and on a bare single-segment reference.
        assert!(matches!(
            spec.locate(11, 14),
            Some(Hit::Ref { segment: 0, .. })
        ));
        assert!(matches!(
            spec.locate(12, 15),
            Some(Hit::Ref {
                attr,
                segment: 0,
                ..
            }) if attr == "x"
        ));

        // On an attribute name.
        assert_eq!(
            spec.locate(14, 4),
            Some(Hit::Attr {
                block: "latency".into(),
                name: "series".into(),
            })
        );

        // Inside the query's heredoc, and on the block header: the block.
        assert_eq!(
            spec.locate(4, 10),
            Some(Hit::Block {
                kind: "query".into(),
                name: "latency".into(),
                name_span: spec.queries[0].name_span,
            })
        );
        assert!(matches!(
            spec.locate(2, 3),
            Some(Hit::Block { .. })
        ));

        // Between blocks and past the last one: nothing.
        assert_eq!(spec.locate(8, 1), None);
        assert_eq!(spec.locate(23, 1), None);
    }

    #[test]
    fn columns_check_against_a_live_query() {
        let spec = validate_ok(GOOD);
        let columns = |_: &str| -> Result<Vec<(String, String)>, String> {
            Ok(vec![
                ("timestamp".into(), "TIMESTAMP".into()),
                ("service".into(), "VARCHAR".into()),
                ("latency".into(), "DOUBLE".into()),
            ])
        };
        assert!(check_columns(&spec, &columns).is_empty());

        let bad = validate_ok(
            r#"
query "q" { sql = "SELECT 1" }
plot "p" { type = "bar" query = query.q x = nope y = service }
"#,
        );
        let diagnostics = check_columns(&bad, &columns);
        assert!(
            diagnostics.iter().any(|d| d.message.contains("\"nope\"")),
            "{diagnostics:?}"
        );
        assert!(
            diagnostics.iter().any(|d| d.message.contains("not numeric")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn quoted_column_names_work_where_identifiers_cannot() {
        let spec = validate_ok(
            r#"
query "q" { sql = "SELECT 1" }
plot "p" { type = "line" query = query.q x = "Order Date" y = revenue }
"#,
        );
        assert_eq!(spec.plots[0].x, "Order Date");
    }
}
