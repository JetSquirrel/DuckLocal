//! Syntax highlighting for `.dash` source in the dashboard's editor.
//!
//! The editor's highlighting is parser-independent: it asks an
//! [`InputHighlighter`] for styled ranges. There is no tree-sitter grammar for
//! `.dash`, and it does not need one — the language is small enough that a
//! scanner colours it, and the SQL inside heredocs goes to the same
//! tree-sitter SQL highlighter the SQL editor uses, so a query reads the same
//! in both places.
//!
//! The scanner is deliberately not [`super::syntax::lex`]. The lexer's job is
//! to refuse a malformed file at the first mistake; a highlighter runs on
//! every keystroke over text that is malformed half the time — an unclosed
//! string, a heredoc still missing its delimiter — and has to keep colouring
//! the rest. It follows the lexer's rules (`#` and `//` comments, strings that
//! end at their line, `<<DELIM` heredocs closed by a line that is exactly the
//! delimiter) and never fails.

use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::highlighter::SyntaxHighlighter;
use gpui_kit::component::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter,
    InputHighlighterFactory, Rope,
};
use gpui_kit::{Context, HighlightStyle, SharedString, Window};

/// The editor language name dashboard sources are opened with.
pub const LANGUAGE: &str = "dash";

/// The plot types, coloured as constants: they are the language's own words.
const PLOT_TYPES: &[&str] = &["line", "bar", "area", "scatter", "table"];

/// What the scanner found: `.dash` tokens by highlight name, and the byte
/// ranges of heredoc bodies, which are SQL.
#[derive(Debug, Default, PartialEq)]
struct Scan {
    runs: Vec<(Range<usize>, &'static str)>,
    sql: Vec<Range<usize>>,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The byte index where the line holding `at` ends (its `\n`, or the end).
fn line_end(bytes: &[u8], at: usize) -> usize {
    bytes[at..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |ix| at + ix)
}

/// The next byte after `at` that is not a space or tab, on the same line.
fn next_on_line(bytes: &[u8], mut at: usize) -> Option<u8> {
    while at < bytes.len() && matches!(bytes[at], b' ' | b'\t') {
        at += 1;
    }
    bytes.get(at).copied().filter(|&b| b != b'\n')
}

fn scan(source: &str) -> Scan {
    let bytes = source.as_bytes();
    let mut scan = Scan::default();
    let mut depth = 0usize;
    let mut ix = 0;
    while ix < bytes.len() {
        let b = bytes[ix];
        match b {
            b'#' => {
                let end = line_end(bytes, ix);
                scan.runs.push((ix..end, "comment"));
                ix = end;
            }
            b'/' if bytes.get(ix + 1) == Some(&b'/') => {
                let end = line_end(bytes, ix);
                scan.runs.push((ix..end, "comment"));
                ix = end;
            }
            b'"' => {
                // A string ends at its closing quote or, unclosed, at its line.
                let mut end = ix + 1;
                while end < bytes.len() && bytes[end] != b'\n' {
                    match bytes[end] {
                        b'\\' if end + 1 < bytes.len() && bytes[end + 1] != b'\n' => end += 2,
                        b'"' => {
                            end += 1;
                            break;
                        }
                        _ => end += 1,
                    }
                }
                let end = end.min(bytes.len());
                scan.runs.push((ix..end, "string"));
                ix = end;
            }
            b'<' if bytes.get(ix + 1) == Some(&b'<') => {
                let mut end = ix + 2;
                while end < bytes.len() && is_ident(bytes[end]) {
                    end += 1;
                }
                let delimiter = &source[ix + 2..end];
                scan.runs.push((ix..end, "string.special"));
                if delimiter.is_empty() {
                    ix = end;
                    continue;
                }
                // The body starts on the next line and runs to the first line
                // that is exactly the delimiter — or, still being typed, to
                // the end of the file.
                let opening_end = line_end(bytes, end);
                let body_start = (opening_end + 1).min(bytes.len());
                let mut line_start = body_start;
                let mut closed = None;
                while line_start < bytes.len() {
                    let end_of_line = line_end(bytes, line_start);
                    let line = &source[line_start..end_of_line];
                    if line.trim() == delimiter {
                        let lead = line.len() - line.trim_start().len();
                        closed = Some((line_start, line_start + lead));
                        break;
                    }
                    line_start = end_of_line + 1;
                }
                match closed {
                    Some((closing_line, delimiter_start)) => {
                        if closing_line > body_start {
                            scan.sql.push(body_start..closing_line);
                        }
                        let delimiter_end = delimiter_start + delimiter.len();
                        scan.runs.push((delimiter_start..delimiter_end, "string.special"));
                        ix = delimiter_end;
                    }
                    None => {
                        if bytes.len() > body_start {
                            scan.sql.push(body_start..bytes.len());
                        }
                        ix = bytes.len();
                    }
                }
            }
            b'{' => {
                depth += 1;
                ix += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                ix += 1;
            }
            b'-' | b'0'..=b'9'
                if b.is_ascii_digit()
                    || bytes.get(ix + 1).is_some_and(|next| next.is_ascii_digit()) =>
            {
                let mut end = ix + 1;
                while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
                    end += 1;
                }
                scan.runs.push((ix..end, "number"));
                ix = end;
            }
            _ if is_ident_start(b) => {
                let mut end = ix + 1;
                while end < bytes.len() && is_ident(bytes[end]) {
                    end += 1;
                }
                let word = &source[ix..end];
                let next = next_on_line(bytes, end);
                let name = if depth == 0 && next == Some(b'"') {
                    // `query "name" {` — a block's type.
                    "keyword"
                } else if next == Some(b'=') {
                    "property"
                } else if matches!(word, "true" | "false") {
                    "boolean"
                } else if bytes.get(end) == Some(&b'.')
                    && bytes.get(end + 1).is_some_and(|&b| is_ident_start(b))
                {
                    // `query.latency`: the block type, then the block's name.
                    let mut tail_end = end + 2;
                    while tail_end < bytes.len() && is_ident(bytes[tail_end]) {
                        tail_end += 1;
                    }
                    scan.runs.push((ix..end, "keyword"));
                    scan.runs.push((end + 1..tail_end, "variable.special"));
                    ix = tail_end;
                    continue;
                } else if PLOT_TYPES.contains(&word) {
                    "constant"
                } else {
                    // A bare column name, like `x = day`.
                    "variable.special"
                };
                scan.runs.push((ix..end, name));
                ix = end;
            }
            _ => {
                // Step a whole character, so a multi-byte one is never split.
                ix += source[ix..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    scan
}

/// A dashboard source's highlighter: the scan of the whole text, plus one SQL
/// highlighter per heredoc body. Specs are small, so each edit rescans the
/// lot; a body whose text did not change keeps its parsed SQL.
struct DashHighlighter {
    scan: Scan,
    sql: Vec<(String, SyntaxHighlighter)>,
}

impl DashHighlighter {
    fn new() -> Self {
        Self {
            scan: Scan::default(),
            sql: Vec::new(),
        }
    }
}

impl DashHighlighter {
    /// Rescan `source`, reusing the parsed SQL of every heredoc body whose
    /// text did not change.
    fn refresh(&mut self, source: &str) {
        self.scan = scan(source);

        let mut previous = std::mem::take(&mut self.sql);
        self.sql = self
            .scan
            .sql
            .iter()
            .map(|range| {
                let body = source[range.clone()].to_string();
                match previous.iter().position(|(text, _)| *text == body) {
                    Some(ix) => previous.swap_remove(ix),
                    None => {
                        let mut highlighter = SyntaxHighlighter::new("sql");
                        highlighter.update(None, &Rope::from(body.as_str()), None);
                        (body, highlighter)
                    }
                }
            })
            .collect();
    }
}

impl InputHighlighter for DashHighlighter {
    fn language(&self) -> SharedString {
        LANGUAGE.into()
    }

    fn update(
        &mut self,
        _edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        _cx: &mut Context<EditorState>,
    ) {
        self.refresh(&text.to_string());
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        // Styled pieces inside `range`, in order and disjoint: the scanner's
        // runs never enter a heredoc body, and a body's own styles stay in it.
        let mut pieces: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        for (run, name) in &self.scan.runs {
            let start = run.start.max(range.start);
            let end = run.end.min(range.end);
            if start < end {
                if let Some(style) = resolver.style(name) {
                    pieces.push((start..end, style));
                }
            }
        }
        for (body, (_, highlighter)) in self.scan.sql.iter().zip(&self.sql) {
            let start = body.start.max(range.start);
            let end = body.end.min(range.end);
            if start >= end {
                continue;
            }
            let local = start - body.start..end - body.start;
            for (piece, style) in highlighter.styles(&local, resolver) {
                if style != HighlightStyle::default() && piece.start < piece.end {
                    pieces.push((piece.start + body.start..piece.end + body.start, style));
                }
            }
        }
        pieces.sort_by_key(|(piece, _)| piece.start);

        // The editor wants the whole range covered: unstyled text in between.
        let mut runs = Vec::with_capacity(pieces.len() * 2 + 1);
        let mut at = range.start;
        for (piece, style) in pieces {
            if piece.start < at {
                continue;
            }
            if piece.start > at {
                runs.push((at..piece.start, HighlightStyle::default()));
            }
            at = piece.end;
            runs.push((piece, style));
        }
        if at < range.end {
            runs.push((at..range.end, HighlightStyle::default()));
        }
        runs
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}

/// The factory the dashboard's source editor installs: `.dash` gets this
/// highlighter, anything else none.
pub fn factory() -> InputHighlighterFactory {
    Rc::new(|language| {
        (language == LANGUAGE).then(|| Box::new(DashHighlighter::new()) as Box<dyn InputHighlighter>)
    })
}

#[cfg(test)]
mod tests {
    use super::scan;

    /// Each run as `(text, highlight name)`, for readable assertions.
    fn runs(source: &str) -> Vec<(&str, &'static str)> {
        scan(source)
            .runs
            .into_iter()
            .map(|(range, name)| (&source[range], name))
            .collect()
    }

    #[test]
    fn a_spec_is_coloured_by_what_each_word_is() {
        let source = "# latency\nplot \"p50\" {\n  type = line\n  query = query.latency\n  x = day\n  stacked = true\n  width = 2\n}\n";
        assert_eq!(
            runs(source),
            vec![
                ("# latency", "comment"),
                ("plot", "keyword"),
                ("\"p50\"", "string"),
                ("type", "property"),
                ("line", "constant"),
                ("query", "property"),
                ("query", "keyword"),
                ("latency", "variable.special"),
                ("x", "property"),
                ("day", "variable.special"),
                ("stacked", "property"),
                ("true", "boolean"),
                ("width", "property"),
                ("2", "number"),
            ]
        );
    }

    #[test]
    fn a_heredoc_body_is_sql_and_its_delimiters_are_marked() {
        let source = "query \"q\" {\n  sql = <<SQL\n    SELECT 1\n  SQL\n}\n";
        let found = scan(source);
        assert_eq!(found.sql.len(), 1);
        assert_eq!(&source[found.sql[0].clone()], "    SELECT 1\n");
        let marks: Vec<_> = found
            .runs
            .iter()
            .filter(|(_, name)| *name == "string.special")
            .map(|(range, _)| &source[range.clone()])
            .collect();
        assert_eq!(marks, vec!["<<SQL", "SQL"]);
    }

    #[test]
    fn half_typed_text_still_colours() {
        // An unclosed string stops at its line; the next line is still code.
        assert_eq!(
            runs("title = \"Daily\nx = day"),
            vec![
                ("title", "property"),
                ("\"Daily", "string"),
                ("x", "property"),
                ("day", "variable.special"),
            ]
        );
        // A heredoc still missing its delimiter is SQL to the end.
        let source = "sql = <<SQL\nSELECT";
        assert_eq!(&source[scan(source).sql[0].clone()], "SELECT");
        // Nothing to scan, and a lone `<<`, do not panic.
        assert_eq!(scan(""), Default::default());
        assert_eq!(runs("<<"), vec![("<<", "string.special")]);
    }

    #[test]
    fn a_double_slash_comments_and_non_ascii_text_is_stepped_over() {
        assert_eq!(
            runs("// 每日成本\ntitle = \"成本\""),
            vec![
                ("// 每日成本", "comment"),
                ("title", "property"),
                ("\"成本\"", "string"),
            ]
        );
        assert_eq!(runs("é = 1"), vec![("1", "number")]);
    }

    #[test]
    fn styles_cover_the_range_and_colour_the_sql_inside_a_heredoc() {
        use super::DashHighlighter;
        use gpui_kit::component::highlighter::HighlightTheme;
        use gpui_kit::component::input::{HighlightStyleResolver, InputHighlighter};

        let source = "query \"q\" {\n  sql = <<SQL\n    SELECT 1 FROM t\n  SQL\n}\n";
        let mut highlighter = DashHighlighter::new();
        highlighter.refresh(source);
        let theme = HighlightTheme::default_light();
        let range = 0..source.len();
        let styles = highlighter.styles(&range, &*theme);

        // Ordered, disjoint, and covering the whole range.
        assert_eq!(styles.first().unwrap().0.start, 0);
        assert_eq!(styles.last().unwrap().0.end, source.len());
        for pair in styles.windows(2) {
            assert_eq!(pair[0].0.end, pair[1].0.start, "{styles:?}");
        }
        // `SELECT` inside the heredoc is coloured by the SQL grammar.
        let select = source.find("SELECT").unwrap();
        let keyword = theme.style("keyword").unwrap();
        assert!(
            styles
                .iter()
                .any(|(range, style)| range.start == select && *style == keyword),
            "{styles:?}"
        );
        // A sub-range starting mid-file is covered too.
        let middle = source.find("sql").unwrap()..source.find("FROM").unwrap();
        let styles = highlighter.styles(&middle, &*theme);
        assert_eq!(styles.first().unwrap().0.start, middle.start);
        assert_eq!(styles.last().unwrap().0.end, middle.end);
    }
}
