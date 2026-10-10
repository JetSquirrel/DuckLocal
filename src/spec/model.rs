//! What the tree means: three block kinds, a fixed set of attributes, and
//! references that must resolve.
//!
//! The rules a `.dash` file lives by:
//!
//! * a `source` block names data files: one `path` (a file or a glob, relative
//!   to the `.dash` file), and its name is a relation every query may read;
//! * a `query` block holds one `sql` attribute — one statement, nothing else;
//! * a `plot` block holds `type`, `query`, `x` and `y`, plus an optional
//!   `series` and `title`; a `table` shows its query's every column and
//!   needs neither `x` nor `y`;
//! * a `pie` takes `x` (the slices) and `y` (their sizes), and no `series`;
//! * a `map` takes `lat` and `lng` instead of `x` and `y` — either may be left
//!   out when a column's name says what it is (`geo_lat`, `longitude`) — and
//!   optionally `color`, `size` (with `size_scale`, `"sqrt"` or `"log"`) and
//!   `tooltip`, a list of the columns its tooltip shows;
//! * a `card` shows one number: its `value` column (the first column when left
//!   out) from the query's first row, and takes no `x`, `y` or `series`;
//! * `width` is how many of the dashboard's twelve columns a plot spans —
//!   plots fill a row left to right and wrap; 12 unless a card, which is 3;
//! * a `filter` block names a plot a click picks a value on — a `bar` (its
//!   bars) or a `table` (its rows) — and optionally the `column` whose value
//!   is picked (a bar's `x` when left out; a table must name one); a query
//!   reads it as `$name`, a predicate (see `filter.rs`);
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
/// x/y for the continuous ones and `pie`, anything tabular for `table`,
/// coordinates for `map`.
pub(crate) const PLOT_TYPES: &[&str] =
    &["line", "bar", "area", "scatter", "pie", "map", "table", "card"];

/// The attributes only a `map` takes.
const MAP_ATTRS: &[&str] = &["lat", "lng", "color", "size", "size_scale", "tooltip"];

/// The attributes only a `card` takes.
const CARD_ATTRS: &[&str] = &["value"];

/// The dashboard's grid: a row is this many columns wide.
pub(crate) const GRID_COLUMNS: u8 = 12;

/// How a map's `size` may scale its points.
pub(crate) const SIZE_SCALES: &[&str] = &["sqrt", "log"];

/// The plot types a filter can pick a value on: a bar by its band, a table
/// by its row.
pub(crate) const FILTER_PLOTS: &[&str] = &["bar", "table"];

#[derive(Debug, Clone)]
pub(crate) struct Spec {
    pub sources: Vec<Source>,
    pub queries: Vec<Query>,
    pub plots: Vec<Plot>,
    pub filters: Vec<Filter>,
    /// Where every block and attribute sits in the source, in source order,
    /// for `Spec::locate`.
    #[allow(dead_code)] // read by locate(), which the editor phases wire up
    sites: Vec<Site>,
}

/// Data files under a name the queries read: `source "orders" { path = ... }`.
#[derive(Debug, Clone)]
pub(crate) struct Source {
    pub name: String,
    /// As written: a file or a glob, relative to the `.dash` file's folder
    /// unless absolute or `~`-led. `source::resolve` makes it a real path.
    pub path: String,
    pub line: usize,
    /// The quoted name's span, quotes included: the definition target.
    pub name_span: (usize, usize),
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
    /// A `map`'s coordinate columns; `None` finds them by name.
    pub lat: Option<String>,
    pub lng: Option<String>,
    /// The column a `map` colors its points by; `None` picks one.
    pub color: Option<String>,
    /// The numeric column a `map` sizes its points by, and the scale:
    /// `"sqrt"` (the default, area follows value) or `"log"`.
    pub size: Option<String>,
    pub size_scale: Option<String>,
    /// The columns a `map`'s tooltip lists; `None` picks the numbers.
    pub tooltip: Option<Vec<String>>,
    /// The column a `card` shows; `None` shows the first.
    pub value: Option<String>,
    /// Grid columns spanned as written; `Plot::width` applies the default.
    pub width: Option<u8>,
    /// Line and column of the block's kind keyword.
    pub line: usize,
    pub col: usize,
    /// The kind keyword's span: where the block starts.
    pub span: (usize, usize),
    /// The quoted name's span, quotes included: the definition target.
    pub name_span: (usize, usize),
}

/// A value a click on one plot picks, for the queries that read `$name`.
#[derive(Debug, Clone)]
pub(crate) struct Filter {
    pub name: String,
    /// The plot block the value is picked on.
    pub plot: String,
    /// The column picked and compared: as written, or the bar's `x`. Empty
    /// only while the file has diagnostics.
    pub column: String,
    pub line: usize,
    pub col: usize,
    pub span: (usize, usize),
    pub name_span: (usize, usize),
}

impl Spec {
    /// The filters a query reads, by name, in the order its SQL names them.
    pub(crate) fn filters_of(&self, query: &Query) -> Vec<&Filter> {
        let mut used: Vec<&Filter> = Vec::new();
        for placeholder in super::filter::placeholders(&query.sql) {
            if let Some(filter) = self.filters.iter().find(|f| f.name == placeholder.name) {
                if !used.iter().any(|f| f.name == filter.name) {
                    used.push(filter);
                }
            }
        }
        used
    }
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

impl Plot {
    /// How many of the grid's columns this plot spans: as written, or a
    /// quarter row for a card and the whole row for everything else.
    pub(crate) fn width(&self) -> u8 {
        self.width
            .unwrap_or(if self.kind == "card" { 3 } else { GRID_COLUMNS })
    }
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
    let mut sources = Vec::new();
    let mut sites = Vec::new();
    let mut query_names: HashSet<String> = HashSet::new();
    let mut plot_names: HashSet<String> = HashSet::new();
    let mut source_names: HashSet<String> = HashSet::new();
    let mut filters = Vec::new();
    let mut filter_names: HashSet<String> = HashSet::new();

    for block in &file.blocks {
        match block.kind.as_str() {
            "source" => {
                // SQL names are case-insensitive, so `Orders` and `orders`
                // would be one relation.
                if !source_names.insert(block.name.to_ascii_lowercase()) {
                    diagnostics.push(Diagnostic::at_block(
                        block,
                        format!("Duplicate source name: {:?}", block.name),
                    ));
                }
                if let Some(s) = source(block, &mut diagnostics) {
                    sites.push(site(block, s.name_span));
                    sources.push(s);
                }
            }
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
            "filter" => {
                if !filter_names.insert(block.name.clone()) {
                    diagnostics.push(Diagnostic::at_block(
                        block,
                        format!("Duplicate filter name: {:?}", block.name),
                    ));
                }
                let f = filter(block, &mut diagnostics);
                sites.push(site(block, f.name_span));
                filters.push((f, block));
            }
            other => diagnostics.push(Diagnostic::at_block(
                block,
                format!(
                    "Unknown block: {other:?}; the file holds source, query, plot and filter blocks"
                ),
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

    // A filter picks on a plot that exists and can be picked on; a bar's
    // column defaults to its x, a table has no such default.
    for (filter, block) in filters.iter_mut() {
        let at_plot = |message: String| match block.attrs.iter().find(|a| a.name == "plot") {
            Some(attr) => Diagnostic::at_attr(attr, message),
            None => Diagnostic::at_block(block, message),
        };
        if filter.plot.is_empty() {
            continue; // its own diagnostic already says so
        }
        let Some(plot) = plots.iter().find(|p| p.name == filter.plot) else {
            diagnostics.push(at_plot(format!(
                "filter {:?} picks on plot {:?}, which the file does not define",
                filter.name, filter.plot
            )));
            continue;
        };
        if !plot.kind.is_empty() && !FILTER_PLOTS.contains(&plot.kind.as_str()) {
            diagnostics.push(at_plot(format!(
                "filter {:?} picks on plot {:?}, a {}; a value is picked on a {} plot",
                filter.name,
                plot.name,
                plot.kind,
                FILTER_PLOTS.join(" or ")
            )));
            continue;
        }
        if filter.column.is_empty() {
            if plot.kind == "bar" {
                filter.column = plot.x.clone();
            } else if plot.kind == "table" {
                diagnostics.push(Diagnostic::at_block(
                    block,
                    format!(
                        "filter {:?} picks on table {:?}: name the column a row gives, column = some_column",
                        filter.name, plot.name
                    ),
                ));
            }
        }
    }
    let filters: Vec<Filter> = filters.into_iter().map(|(f, _)| f).collect();

    // Every `$name` a query reads is a filter the file defines.
    for query in &queries {
        for placeholder in super::filter::placeholders(&query.sql) {
            if !filters.iter().any(|f| f.name == placeholder.name) {
                let block = file
                    .blocks
                    .iter()
                    .find(|b| b.kind == "query" && b.name == query.name);
                let message = format!(
                    "query {:?} reads ${}, but no filter block is named {:?}",
                    query.name, placeholder.name, placeholder.name
                );
                diagnostics.push(
                    match block.and_then(|b| b.attrs.iter().find(|a| a.name == "sql")) {
                        Some(attr) => Diagnostic::at_attr(attr, message),
                        None => Diagnostic {
                            line: query.line,
                            col: query.col,
                            span: query.span,
                            message,
                        },
                    },
                );
            }
        }
    }

    if diagnostics.is_empty() {
        Ok(Spec {
            sources,
            queries,
            plots,
            filters,
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

fn source(block: &Block, diagnostics: &mut Vec<Diagnostic>) -> Option<Source> {
    // The name is written into SQL bare, so it must read as one identifier.
    let mut chars = block.name.chars();
    let identifier = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !identifier {
        diagnostics.push(Diagnostic::at_block(
            block,
            format!(
                "source {:?}: a source's name is how SQL reads it, so it is letters, digits and _, not starting with a digit",
                block.name
            ),
        ));
    }
    let mut path = None;
    let mut seen = false;
    for attr in &block.attrs {
        match attr.name.as_str() {
            "path" if seen => diagnostics.push(Diagnostic::at_attr(
                attr,
                "Duplicate attribute: path",
            )),
            "path" => {
                seen = true;
                match string(attr, diagnostics) {
                    Some(text) if text.trim().is_empty() => diagnostics.push(
                        Diagnostic::at_attr(attr, "path names a file or a glob; it is empty"),
                    ),
                    Some(text) => {
                        if let Some(message) = super::source::unreadable(&text) {
                            diagnostics.push(Diagnostic::at_attr(attr, message));
                        }
                        path = Some(text);
                    }
                    None => {}
                }
            }
            other => diagnostics.push(Diagnostic::at_attr(
                attr,
                format!("A source block holds only path; unknown attribute: {other}"),
            )),
        }
    }
    if !seen {
        diagnostics.push(Diagnostic::at_block(
            block,
            format!("source block {:?} requires path", block.name),
        ));
    }
    Some(Source {
        name: block.name.clone(),
        path: path.unwrap_or_default(),
        line: block.line,
        name_span: block.name_span,
    })
}

fn filter(block: &Block, diagnostics: &mut Vec<Diagnostic>) -> Filter {
    // `$name` in SQL ends at the first character that is not a letter, digit
    // or underscore, so the name must be made of nothing else.
    let mut chars = block.name.chars();
    let identifier = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !identifier {
        diagnostics.push(Diagnostic::at_block(
            block,
            format!(
                "filter {:?}: a query reads a filter as $name, so its name is letters, digits and _, not starting with a digit",
                block.name
            ),
        ));
    }
    let mut plot = None;
    let mut column_name = None;
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
            "plot" => match &attr.value {
                Value::Ref(segments) if segments.len() == 2 && segments[0] == "plot" => {
                    plot = Some(segments[1].clone())
                }
                _ => diagnostics.push(Diagnostic::at_attr(
                    attr,
                    "plot names the plot a value is picked on: plot = plot.some_name",
                )),
            },
            "column" => column_name = column(attr, diagnostics),
            other => diagnostics.push(Diagnostic::at_attr(
                attr,
                format!("A filter block holds plot and column; unknown attribute: {other}"),
            )),
        }
    }
    if !seen.contains("plot") {
        diagnostics.push(Diagnostic::at_block(
            block,
            format!("filter block {:?} requires plot", block.name),
        ));
    }
    Filter {
        name: block.name.clone(),
        plot: plot.unwrap_or_default(),
        column: column_name.unwrap_or_default(),
        line: block.line,
        col: block.col,
        span: block.kind_span,
        name_span: block.name_span,
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
    let mut lat = None;
    let mut lng = None;
    let mut color = None;
    let mut size = None;
    let mut size_scale = None;
    let mut tooltip = None;
    let mut value = None;
    let mut width = None;
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
            "lat" => lat = column(attr, diagnostics),
            "lng" => lng = column(attr, diagnostics),
            "color" => color = column(attr, diagnostics),
            "size" => size = column(attr, diagnostics),
            "size_scale" => match string(attr, diagnostics) {
                Some(text) if SIZE_SCALES.contains(&text.as_str()) => size_scale = Some(text),
                Some(text) => diagnostics.push(Diagnostic::at_attr(
                    attr,
                    format!(
                        "Unknown size_scale: {text:?}; one of {}",
                        SIZE_SCALES.join(", ")
                    ),
                )),
                None => {}
            },
            "tooltip" => tooltip = columns(attr, diagnostics),
            "value" => value = column(attr, diagnostics),
            "width" => width = grid_width(attr, diagnostics),
            other => diagnostics.push(Diagnostic::at_attr(
                attr,
                format!(
                    "A plot block holds type, query, x, y, series, title, width, for a map lat, lng, color, size, size_scale, tooltip, and for a card value; unknown attribute: {other}"
                ),
            )),
        }
    }
    let kind_name = kind.clone().unwrap_or_default();
    let is_map = kind_name == "map";
    let is_card = kind_name == "card";
    // Attributes the type has no use for are mistakes, not no-ops: a map's
    // `x` or a line's `lat` says the author expects something that will not
    // happen.
    for attr in &block.attrs {
        let name = attr.name.as_str();
        let misplaced = if is_map {
            matches!(name, "x" | "y" | "series")
                .then(|| format!("A map plot places points by lat and lng; it takes no {name}"))
        } else if is_card && (matches!(name, "x" | "y" | "series") || MAP_ATTRS.contains(&name)) {
            Some(format!("A card shows one value; it takes no {name} (name its column with value)"))
        } else if !kind_name.is_empty() && MAP_ATTRS.contains(&name) {
            Some(format!("{name} is for map plots; this plot is a {kind_name}"))
        } else if !kind_name.is_empty() && !is_card && CARD_ATTRS.contains(&name) {
            Some(format!("{name} is for card plots; this plot is a {kind_name}"))
        } else if kind_name == "pie" && name == "series" {
            Some("A pie draws one series; it takes no series".to_string())
        } else {
            None
        };
        if let Some(message) = misplaced {
            diagnostics.push(Diagnostic::at_attr(attr, message));
        }
        if name == "size_scale" && size.is_none() && is_map {
            diagnostics.push(Diagnostic::at_attr(
                attr,
                "size_scale says how size scales the points; set size to a numeric column too",
            ));
        }
    }
    // A table shows every column its query returns: it has no axis to name.
    let required: &[&str] = if is_map || is_card || kind_name == "table" {
        &["type", "query"]
    } else {
        &["type", "query", "x"]
    };
    for &name in required {
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
    if !kind.is_empty() && !matches!(kind.as_str(), "table" | "map" | "card") && y.is_none() {
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
        lat,
        lng,
        color,
        size,
        size_scale,
        tooltip,
        value,
        width,
        line: block.line,
        col: block.col,
        span: block.kind_span,
        name_span: block.name_span,
    })
}

/// A plot's `width`: a whole number of grid columns, 1 to 12.
fn grid_width(attr: &Attr, diagnostics: &mut Vec<Diagnostic>) -> Option<u8> {
    let parsed = match &attr.value {
        Value::Number(text) => text.parse::<u8>().ok(),
        _ => None,
    };
    match parsed {
        Some(n) if (1..=GRID_COLUMNS).contains(&n) => Some(n),
        _ => {
            diagnostics.push(Diagnostic::at_attr(
                attr,
                format!(
                    "width is how many of the dashboard's {GRID_COLUMNS} columns the plot spans: a whole number from 1 to {GRID_COLUMNS}"
                ),
            ));
            None
        }
    }
}

/// A list of column names: `[place, requests, "Unique IPs"]`, not empty.
fn columns(attr: &Attr, diagnostics: &mut Vec<Diagnostic>) -> Option<Vec<String>> {
    let Value::List(items) = &attr.value else {
        diagnostics.push(Diagnostic::at_attr(
            attr,
            format!("{} is a list of columns: [a, b, \"C d\"]", attr.name),
        ));
        return None;
    };
    let mut names = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::Ref(segments) if segments.len() == 1 => names.push(segments[0].clone()),
            Value::Str(text) => names.push(text.clone()),
            _ => {
                diagnostics.push(Diagnostic::at_attr(
                    attr,
                    format!("{} lists columns: identifiers or strings", attr.name),
                ));
                return None;
            }
        }
    }
    if names.is_empty() {
        diagnostics.push(Diagnostic::at_attr(
            attr,
            format!("{} lists at least one column; leave it out for the default", attr.name),
        ));
        return None;
    }
    Some(names)
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
    // A filter picks its column's value from the row the click lands on, so
    // the picked plot's query must return it.
    for filter in &spec.filters {
        let Some(plot) = spec.plots.iter().find(|p| p.name == filter.plot) else {
            continue;
        };
        let Ok(available) = columns(&plot.query) else {
            continue; // the plot's own check reports the query
        };
        if !available
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(&filter.column))
        {
            diagnostics.push(Diagnostic {
                line: filter.line,
                col: filter.col,
                span: filter.span,
                message: format!(
                    "filter {:?} picks column {:?}, which query {:?} (plot {:?}) does not return; it returns: {}",
                    filter.name,
                    filter.column,
                    plot.query,
                    plot.name,
                    available
                        .iter()
                        .map(|(name, _)| name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
    }
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
        let returns = || {
            available
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        // A map may leave its coordinates to the column names; if the names
        // do not say, the plot has nothing to place.
        let (mut lat, mut lng) = (plot.lat.clone(), plot.lng.clone());
        if plot.kind == "map" && (lat.is_none() || lng.is_none()) {
            let (guess_lat, guess_lng) = crate::ui::geo::guess_coordinate_columns(
                available.iter().map(|(name, _)| name.as_str()),
            );
            let guessed = |ix: Option<usize>| ix.map(|ix| available[ix].0.clone());
            lat = lat.or_else(|| guessed(guess_lat));
            lng = lng.or_else(|| guessed(guess_lng));
            for (name, found) in [("lat", &lat), ("lng", &lng)] {
                if found.is_none() {
                    diagnostics.push(Diagnostic {
                        line: plot.line,
                        col: plot.col,
                        span: plot.span,
                        message: format!(
                            "map plot {:?} names no {name}, and no column of query {:?} looks like one; set {name} to one of: {}",
                            plot.name,
                            query.name,
                            returns()
                        ),
                    });
                }
            }
        }
        let x = !matches!(plot.kind.as_str(), "map" | "card") && !plot.x.is_empty();
        let x = x.then_some(plot.x.as_str());
        for (name, column) in [
            ("x", x),
            ("y", plot.y.as_deref()),
            ("series", plot.series.as_deref()),
            ("lat", plot.lat.as_deref()),
            ("lng", plot.lng.as_deref()),
            ("color", plot.color.as_deref()),
            ("size", plot.size.as_deref()),
            ("value", plot.value.as_deref()),
        ]
        .into_iter()
        .chain(
            plot.tooltip
                .iter()
                .flatten()
                .map(|column| ("tooltip", Some(column.as_str()))),
        )
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
        // Coordinates that are not numbers place nothing, and a size that is
        // not one sizes nothing.
        for (name, column) in [("lat", &lat), ("lng", &lng), ("size", &plot.size)] {
            let Some(column) = column else { continue };
            if let Some((_, ty)) = available
                .iter()
                .find(|(c, _)| c.eq_ignore_ascii_case(column))
            {
                if !is_numeric(ty) {
                    diagnostics.push(Diagnostic {
                        line: plot.line,
                        col: plot.col,
                        span: plot.span,
                        message: format!(
                            "map plot {:?} uses {name} = {column:?} ({ty}), which is not numeric",
                            plot.name
                        ),
                    });
                }
            }
        }
        // A plotted y that is not a number draws nothing worth looking at.
        if plot.kind == "card" && available.is_empty() {
            diagnostics.push(Diagnostic {
                line: plot.line,
                col: plot.col,
                span: plot.span,
                message: format!(
                    "card {:?}: query {:?} returns no columns to show",
                    plot.name, query.name
                ),
            });
        }
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

    const FILTERED: &str = r#"
query "by_channel" { sql = "SELECT channel, sum(revenue) AS revenue FROM orders GROUP BY 1" }
query "daily" { sql = "SELECT date, sum(orders) AS orders FROM orders WHERE $channel GROUP BY 1" }
query "rows" { sql = "SELECT id, channel FROM orders" }
plot "channels" {
  type = "bar"
  query = query.by_channel
  x = channel
  y = revenue
}
plot "list" {
  type = "table"
  query = query.rows
}
plot "daily" {
  type = "line"
  query = query.daily
  x = date
  y = orders
}
filter "channel" { plot = plot.channels }
filter "picked" {
  plot = plot.list
  column = id
}
"#;

    #[test]
    fn a_filter_picks_on_a_bar_or_a_table_and_queries_read_it() {
        let spec = validate_ok(FILTERED);
        assert_eq!(spec.filters.len(), 2);
        // A bar's column defaults to its x; a table's is as written.
        assert_eq!(spec.filters[0].column, "channel");
        assert_eq!(spec.filters[1].column, "id");
        let daily = spec.queries.iter().find(|q| q.name == "daily").unwrap();
        let used: Vec<&str> = spec.filters_of(daily).iter().map(|f| f.name.as_str()).collect();
        assert_eq!(used, ["channel"]);
    }

    #[test]
    fn filter_mistakes_are_named() {
        let unknown = FILTERED.replace("WHERE $channel", "WHERE $chanel");
        let messages: Vec<String> = validate_err(&unknown).into_iter().map(|d| d.message).collect();
        assert!(
            messages.iter().any(|m| m.contains("$chanel") && m.contains("no filter block")),
            "{messages:?}"
        );

        let on_a_line = FILTERED.replace("plot = plot.channels", "plot = plot.daily");
        let messages: Vec<String> = validate_err(&on_a_line).into_iter().map(|d| d.message).collect();
        assert!(messages.iter().any(|m| m.contains("a line")), "{messages:?}");

        let missing = FILTERED.replace("plot = plot.channels", "plot = plot.nope");
        let messages: Vec<String> = validate_err(&missing).into_iter().map(|d| d.message).collect();
        assert!(messages.iter().any(|m| m.contains("does not define")), "{messages:?}");

        let table_without_column = FILTERED.replace("  column = id\n", "");
        let messages: Vec<String> =
            validate_err(&table_without_column).into_iter().map(|d| d.message).collect();
        assert!(messages.iter().any(|m| m.contains("name the column")), "{messages:?}");

        let bad_name = FILTERED.replace("filter \"channel\"", "filter \"sales-channel\"");
        let messages: Vec<String> = validate_err(&bad_name).into_iter().map(|d| d.message).collect();
        assert!(messages.iter().any(|m| m.contains("letters, digits")), "{messages:?}");
    }

    #[test]
    fn a_filter_column_the_picked_query_lacks_is_caught_with_a_database() {
        let spec = validate_ok(&FILTERED.replace("column = id", "column = nope"));
        let lookup = |name: &str| -> Result<Vec<(String, String)>, String> {
            Ok(match name {
                "by_channel" => vec![("channel".into(), "VARCHAR".into()), ("revenue".into(), "DOUBLE".into())],
                "rows" => vec![("id".into(), "BIGINT".into()), ("channel".into(), "VARCHAR".into())],
                _ => vec![("date".into(), "DATE".into()), ("orders".into(), "BIGINT".into())],
            })
        };
        let messages: Vec<String> =
            check_columns(&spec, &lookup).into_iter().map(|d| d.message).collect();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].contains("\"nope\""), "{messages:?}");
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
  type  = "donut"
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
        assert!(messages.iter().any(|m| m.contains("\"donut\"")), "{messages:?}");
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
            .find(|d| d.message.contains("\"donut\""))
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

    #[test]
    fn pies_and_maps_validate() {
        let spec = validate_ok(
            r#"
query "q" { sql = "SELECT 1" }
plot "share" { type = "pie" query = query.q x = channel y = total }
plot "where" {
  type  = "map"
  query = query.q
  lat   = geo_lat
  lng   = geo_lng
  color = type
}
plot "guessed" { type = "map" query = query.q }
"#,
        );
        assert_eq!(spec.plots[0].kind, "pie");
        let map = &spec.plots[1];
        assert_eq!(map.lat.as_deref(), Some("geo_lat"));
        assert_eq!(map.lng.as_deref(), Some("geo_lng"));
        assert_eq!(map.color.as_deref(), Some("type"));
        // A map needs neither x nor y, and may leave its coordinates to the
        // column names.
        assert_eq!(spec.plots[2].lat, None);
    }

    #[test]
    fn attributes_a_type_has_no_use_for_are_mistakes() {
        let diagnostics = validate_err(
            r#"
query "q" { sql = "SELECT 1" }
plot "a" { type = "map" query = query.q x = lng y = lat }
plot "b" { type = "line" query = query.q x = t y = v lat = geo_lat }
plot "c" { type = "pie" query = query.q x = t y = v series = s }
"#,
        );
        let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("takes no x")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("takes no y")), "{messages:?}");
        assert!(
            messages.iter().any(|m| m.contains("lat is for map plots")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("takes no series")),
            "{messages:?}"
        );
        // Each points at the attribute, on its own line.
        let lat = diagnostics
            .iter()
            .find(|d| d.message.contains("lat is for map plots"))
            .unwrap();
        assert_eq!(lat.line, 4);
    }

    #[test]
    fn a_map_checks_its_coordinates_against_the_query() {
        let spec = validate_ok(
            r#"
query "q" { sql = "SELECT 1" }
plot "named" { type = "map" query = query.q lat = geo_lat lng = code }
plot "guessed" { type = "map" query = query.q }
"#,
        );
        let stations = |_: &str| -> Result<Vec<(String, String)>, String> {
            Ok(vec![
                ("code".into(), "VARCHAR".into()),
                ("geo_lat".into(), "DOUBLE".into()),
                ("geo_lng".into(), "DOUBLE".into()),
            ])
        };
        let diagnostics = check_columns(&spec, &stations);
        // A text longitude places nothing; the guessed map finds both.
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].message.contains("lng = \"code\""), "{diagnostics:?}");

        let no_coordinates = |_: &str| -> Result<Vec<(String, String)>, String> {
            Ok(vec![("code".into(), "VARCHAR".into())])
        };
        let diagnostics = check_columns(&spec, &no_coordinates);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.message.contains("names no lat") && d.message.contains("guessed")),
            "{diagnostics:?}"
        );
        assert!(
            diagnostics.iter().any(|d| d.message.contains("\"geo_lat\"")),
            "a named column the query lacks is reported: {diagnostics:?}"
        );
    }

    #[test]
    fn a_map_takes_a_size_its_scale_and_tooltip_columns() {
        let spec = validate_ok(
            r#"
query "q" { sql = "SELECT 1" }
plot "m" {
  type = "map"
  query = query.q
  size = requests
  size_scale = "log"
  tooltip = [place, requests, "Unique IPs"]
}
"#,
        );
        let map = &spec.plots[0];
        assert_eq!(map.size.as_deref(), Some("requests"));
        assert_eq!(map.size_scale.as_deref(), Some("log"));
        assert_eq!(
            map.tooltip.as_deref(),
            Some(&["place".to_string(), "requests".into(), "Unique IPs".into()][..])
        );
    }

    #[test]
    fn size_tooltip_and_their_scale_are_checked() {
        let diagnostics = validate_err(
            r#"
query "q" { sql = "SELECT 1" }
plot "a" { type = "map" query = query.q size = n size_scale = "cubic" }
plot "b" { type = "map" query = query.q size_scale = "log" }
plot "c" { type = "map" query = query.q tooltip = [] }
plot "d" { type = "map" query = query.q tooltip = place }
plot "e" { type = "bar" query = query.q x = t y = v size = n }
"#,
        );
        let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
        let has = |needle: &str| messages.iter().any(|m| m.contains(needle));
        assert!(has("Unknown size_scale: \"cubic\""), "{messages:?}");
        assert!(has("set size to a numeric column too"), "{messages:?}");
        assert!(has("lists at least one column"), "{messages:?}");
        assert!(has("tooltip is a list of columns"), "{messages:?}");
        assert!(has("size is for map plots"), "{messages:?}");
    }

    #[test]
    fn size_and_tooltip_columns_must_exist_and_size_must_be_a_number() {
        let spec = validate_ok(
            r#"
query "q" { sql = "SELECT 1" }
plot "m" {
  type = "map"
  query = query.q
  lat = lat
  lng = lng
  size = place
  tooltip = [place, nope]
}
"#,
        );
        let columns = |_: &str| {
            Ok(vec![
                ("lat".to_string(), "DOUBLE".to_string()),
                ("lng".to_string(), "DOUBLE".to_string()),
                ("place".to_string(), "VARCHAR".to_string()),
            ])
        };
        let diagnostics = check_columns(&spec, &columns);
        let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert!(
            messages.iter().any(|m| m.contains("size = \"place\" (VARCHAR), which is not numeric")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("tooltip = \"nope\"")),
            "{messages:?}"
        );
    }

    #[test]
    fn sources_cards_and_widths_validate() {
        let spec = validate_ok(
            r#"
source "orders" { path = "exports/*.parquet" }
query "q" { sql = "SELECT count(*) AS n FROM orders" }
plot "n" { type = "card" query = query.q value = n title = "Orders" }
plot "first" { type = "card" query = query.q }
plot "half" { type = "bar" query = query.q x = n y = n width = 6 }
"#,
        );
        assert_eq!(spec.sources[0].name, "orders");
        assert_eq!(spec.sources[0].path, "exports/*.parquet");
        assert_eq!(spec.plots[0].value.as_deref(), Some("n"));
        assert_eq!(spec.plots[1].value, None, "a card may leave its column out");
        let widths: Vec<u8> = spec.plots.iter().map(Plot::width).collect();
        assert_eq!(widths, [3, 3, 6]);
    }

    #[test]
    fn source_card_and_width_mistakes_are_said() {
        let diagnostics = validate_err(
            r#"
source "orders" { path = "o.csv" }
source "ORDERS" { path = "p.csv" }
source "has space" { path = "o.csv" }
source "nopath" { glob = "o.csv" }
source "book" { path = "b.xlsx" }
query "q" { sql = "SELECT 1" }
plot "a" { type = "card" query = query.q y = n series = s }
plot "b" { type = "line" query = query.q x = t y = v value = v }
plot "c" { type = "line" query = query.q x = t y = v width = 0 }
plot "d" { type = "line" query = query.q x = t y = v width = "6" }
"#,
        );
        let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
        let has = |needle: &str| messages.iter().any(|m| m.contains(needle));
        assert!(has("Duplicate source name: \"ORDERS\""), "{messages:?}");
        assert!(has("source \"has space\""), "{messages:?}");
        assert!(has("unknown attribute: glob"), "{messages:?}");
        assert!(has("requires path"), "{messages:?}");
        assert!(has("A workbook cannot be a source"), "{messages:?}");
        assert!(has("A card shows one value; it takes no y"), "{messages:?}");
        assert!(has("A card shows one value; it takes no series"), "{messages:?}");
        assert!(has("value is for card plots"), "{messages:?}");
        assert_eq!(
            messages.iter().filter(|m| m.starts_with("width is")).count(),
            2,
            "{messages:?}"
        );
        // A card needs no x or y.
        assert!(!has("requires x") && !has("requires y"), "{messages:?}");
    }
}
