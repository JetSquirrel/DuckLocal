//! The `.dash` text as tokens and as a tree.
//!
//! The grammar is deliberately much smaller than the HCL it borrows from:
//! blocks, `name = value` attributes, heredocs for SQL, and references — no
//! functions, no conditionals, no interpolation. What is not here cannot be
//! misused, and a parser this size can be read in one sitting, which is the
//! point of the format.
//!
//! ```text
//! file    ::= block*
//! block   ::= IDENT STRING "{" attr* "}"        query "name" { ... }
//! attr    ::= IDENT "=" value
//! value   ::= STRING | HEREDOC | NUMBER | BOOL | "[" value ("," value)* "]" | ref
//! ref     ::= IDENT ("." IDENT)?                query.latency, timestamp
//! heredoc ::= "<<" IDENT "\n" ... "\n" IDENT    indented SQL, dedented
//! ```
//!
//! Newlines are whitespace outside heredocs; attributes need no separator
//! because every value form is self-delimiting. `#` and `//` comment to the
//! end of the line.
//!
//! Every token and node carries where it was written: a 1-based line and
//! column (counting characters) and a byte `span` (half-open, for slicing the
//! source). Lines stay the diagnostics contract; columns and spans are what
//! an editor needs to point.

/// One lexeme with the line and column it started on and its byte span, so
/// every complaint can name a line and every editor feature can point.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokenKind {
    Ident(String),
    Str(String),
    Number(String),
    Heredoc(String),
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Equals,
    Comma,
    Dot,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub line: usize,
    /// 1-based, in characters — columns count what a reader counts.
    pub col: usize,
    /// Byte offsets, half-open: `source[span.0..span.1]` is the lexeme.
    pub span: (usize, usize),
}

/// A parse failure: the line, column, and span of what was wrong. Syntax
/// errors fail fast — there is no tree to keep validating past the first one.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SyntaxError {
    pub line: usize,
    pub col: usize,
    pub span: (usize, usize),
    pub message: String,
}

impl SyntaxError {
    fn new(line: usize, col: usize, span: (usize, usize), message: impl Into<String>) -> Self {
        Self {
            line,
            col,
            span,
            message: message.into(),
        }
    }

    /// At a token the parser could not use.
    fn at(token: &Token, message: impl Into<String>) -> Self {
        Self::new(token.line, token.col, token.span, message)
    }

    /// At the end of input, where another token was expected: a zero-width
    /// span just past the last one.
    fn at_end(tokens: &[Token], message: impl Into<String>) -> Self {
        match tokens.last() {
            Some(last) => Self::new(last.line, last.col, (last.span.1, last.span.1), message),
            None => Self::new(1, 1, (0, 0), message),
        }
    }
}

/// The 1-based column of a byte offset: characters back to the last newline.
/// Columns count characters, not bytes, so a `数据库` wide name still lines up
/// with what an editor shows. Files are small; scanning the line is cheap.
fn col_at(source: &str, offset: usize) -> usize {
    source[..offset]
        .chars()
        .rev()
        .take_while(|&c| c != '\n')
        .count()
        + 1
}

pub(crate) fn lex(source: &str) -> Result<Vec<Token>, SyntaxError> {
    let mut tokens = Vec::new();
    let mut chars = source.char_indices().peekable();
    let mut line = 1;
    while let Some(&(start, ch)) = chars.peek() {
        match ch {
            _ if ch.is_whitespace() => {
                if ch == '\n' {
                    line += 1;
                }
                chars.next();
            }
            '#' => {
                for (_, c) in chars.by_ref() {
                    if c == '\n' {
                        line += 1;
                        break;
                    }
                }
            }
            '/' if matches!(chars.clone().nth(1), Some((_, '/'))) => {
                chars.next();
                chars.next();
                for (_, c) in chars.by_ref() {
                    if c == '\n' {
                        line += 1;
                        break;
                    }
                }
            }
            '{' => {
                tokens.push(Token {
                    kind: TokenKind::LBrace,
                    line,
                    col: col_at(source, start),
                    span: (start, start + 1),
                });
                chars.next();
            }
            '}' => {
                tokens.push(Token {
                    kind: TokenKind::RBrace,
                    line,
                    col: col_at(source, start),
                    span: (start, start + 1),
                });
                chars.next();
            }
            '[' => {
                tokens.push(Token {
                    kind: TokenKind::LBracket,
                    line,
                    col: col_at(source, start),
                    span: (start, start + 1),
                });
                chars.next();
            }
            ']' => {
                tokens.push(Token {
                    kind: TokenKind::RBracket,
                    line,
                    col: col_at(source, start),
                    span: (start, start + 1),
                });
                chars.next();
            }
            '=' => {
                tokens.push(Token {
                    kind: TokenKind::Equals,
                    line,
                    col: col_at(source, start),
                    span: (start, start + 1),
                });
                chars.next();
            }
            ',' => {
                tokens.push(Token {
                    kind: TokenKind::Comma,
                    line,
                    col: col_at(source, start),
                    span: (start, start + 1),
                });
                chars.next();
            }
            '.' => {
                tokens.push(Token {
                    kind: TokenKind::Dot,
                    line,
                    col: col_at(source, start),
                    span: (start, start + 1),
                });
                chars.next();
            }
            '"' => {
                let start_line = line;
                let col = col_at(source, start);
                chars.next();
                let mut text = String::new();
                let mut closed = false;
                while let Some((offset, c)) = chars.next() {
                    match c {
                        '"' => {
                            closed = true;
                            break;
                        }
                        '\n' => {
                            return Err(SyntaxError::new(
                                start_line,
                                col,
                                (start, offset),
                                "A string ends at its line; use a heredoc for long text",
                            ));
                        }
                        '\\' => match chars.next() {
                            Some((_, 'n')) => text.push('\n'),
                            Some((_, 't')) => text.push('\t'),
                            Some((_, '"')) => text.push('"'),
                            Some((_, '\\')) => text.push('\\'),
                            Some((escape, other)) => {
                                return Err(SyntaxError::new(
                                    start_line,
                                    col,
                                    (start, escape + other.len_utf8()),
                                    format!("Unknown escape: \\{other}"),
                                ));
                            }
                            None => break,
                        },
                        _ => text.push(c),
                    }
                }
                if !closed {
                    return Err(SyntaxError::new(
                        start_line,
                        col,
                        (start, source.len()),
                        "Unterminated string",
                    ));
                }
                let end = chars.peek().map(|&(i, _)| i).unwrap_or(source.len());
                tokens.push(Token {
                    kind: TokenKind::Str(text),
                    line: start_line,
                    col,
                    span: (start, end),
                });
            }
            '<' if matches!(chars.clone().nth(1), Some((_, '<'))) => {
                let start_line = line;
                let col = col_at(source, start);
                chars.next();
                chars.next();
                let mut delimiter = String::new();
                while let Some(&(_, c)) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        delimiter.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if delimiter.is_empty() {
                    return Err(SyntaxError::new(
                        start_line,
                        col,
                        (start, start + 2),
                        "A heredoc needs a delimiter: <<SQL",
                    ));
                }
                // The rest of the opening line is not content.
                for (offset, c) in chars.by_ref() {
                    if c == '\n' {
                        line += 1;
                        break;
                    }
                    if !c.is_whitespace() {
                        return Err(SyntaxError::new(
                            start_line,
                            col,
                            (start, offset + c.len_utf8()),
                            "Nothing may follow a heredoc delimiter on its line",
                        ));
                    }
                }
                let mut lines: Vec<String> = Vec::new();
                let mut current = String::new();
                let mut closing_indent = None;
                let mut end = None;
                for (offset, c) in chars.by_ref() {
                    if c == '\n' {
                        line += 1;
                        if current.trim() == delimiter {
                            closing_indent =
                                Some(current.len() - current.trim_start().len());
                            end = Some(offset);
                            break;
                        }
                        lines.push(std::mem::take(&mut current));
                    } else {
                        current.push(c);
                    }
                }
                // A delimiter on the file's last line needs no newline after it.
                if closing_indent.is_none() && current.trim() == delimiter {
                    closing_indent = Some(current.len() - current.trim_start().len());
                    end = Some(source.len());
                }
                let Some(indent) = closing_indent else {
                    return Err(SyntaxError::new(
                        start_line,
                        col,
                        (start, source.len()),
                        format!("Unterminated heredoc: no line is exactly {delimiter}"),
                    ));
                };
                // HCL's `<<-` rule as the only rule: the closing line's
                // indentation comes off every content line that carries it, so
                // SQL can sit at the block's indent without owning it.
                let text = lines
                    .iter()
                    .map(|l| {
                        if indent == 0 {
                            l.clone()
                        } else if l.len() >= indent
                            && l.bytes().take(indent).all(|b| b == b' ')
                        {
                            l[indent..].to_string()
                        } else {
                            l.trim_start().to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                tokens.push(Token {
                    kind: TokenKind::Heredoc(text),
                    line: start_line,
                    col,
                    span: (start, end.unwrap_or(source.len())),
                });
            }
            _ if ch.is_ascii_alphabetic() || ch == '_' => {
                let start_line = line;
                let mut word = String::new();
                while let Some(&(_, c)) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        word.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let end = chars.peek().map(|&(i, _)| i).unwrap_or(source.len());
                tokens.push(Token {
                    kind: TokenKind::Ident(word),
                    line: start_line,
                    col: col_at(source, start),
                    span: (start, end),
                });
            }
            _ if ch.is_ascii_digit() || (ch == '-' && matches!(chars.clone().nth(1), Some((_, c)) if c.is_ascii_digit())) => {
                let start_line = line;
                let mut word = String::new();
                chars.next();
                word.push(ch);
                while let Some(&(_, c)) = chars.peek() {
                    if c.is_ascii_digit() || c == '.' {
                        word.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let end = chars.peek().map(|&(i, _)| i).unwrap_or(source.len());
                tokens.push(Token {
                    kind: TokenKind::Number(word),
                    line: start_line,
                    col: col_at(source, start),
                    span: (start, end),
                });
            }
            other => {
                return Err(SyntaxError::new(
                    line,
                    col_at(source, start),
                    (start, start + other.len_utf8()),
                    format!("Unexpected character: {other:?}"),
                ));
            }
        }
    }
    Ok(tokens)
}

/// The tree: a file is blocks, a block is attributes, an attribute is a value.
/// Everything keeps the line it came from for the diagnostics that follow,
/// plus the columns and spans an editor points with.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct File {
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Block {
    pub kind: String,
    pub name: String,
    /// Line and column of the kind keyword — where the block starts.
    pub line: usize,
    pub col: usize,
    /// The kind keyword's span: the block's start, for hover and for
    /// diagnostics that blame the block as a whole.
    pub kind_span: (usize, usize),
    /// The quoted name's span, quotes included: the definition target.
    pub name_span: (usize, usize),
    /// The line of the closing brace: with `line`, the block's extent.
    pub end_line: usize,
    pub attrs: Vec<Attr>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Attr {
    pub name: String,
    /// Line, column, and byte span of the attribute's name.
    pub line: usize,
    pub col: usize,
    pub name_span: (usize, usize),
    pub value: Value,
    /// References the value holds, in source order, each segment with its own
    /// span — `query.latency` resolves at the segment under the cursor.
    pub refs: Vec<RefSite>,
}

/// One segment of a reference, with where it was written. Segments are
/// identifiers, so a column range is `col .. col + name.len()`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RefSegment {
    pub name: String,
    pub line: usize,
    pub col: usize,
    pub span: (usize, usize),
}

/// A reference inside a value: `query.latency` is two segments, a bare
/// `timestamp` is one.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RefSite {
    pub segments: Vec<RefSegment>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Str(String),
    Heredoc(String),
    Number(String),
    Bool(bool),
    List(Vec<Value>),
    /// `query.latency` is two segments; a bare `timestamp` is one. Which is
    /// legal depends on the attribute, and that is the model's call, not the
    /// parser's.
    Ref(Vec<String>),
}

pub(crate) fn parse(source: &str) -> Result<File, SyntaxError> {
    let tokens = lex(source)?;
    let mut pos = 0;
    let mut blocks = Vec::new();
    while pos < tokens.len() {
        let (block, next) = parse_block(&tokens, pos)?;
        blocks.push(block);
        pos = next;
    }
    Ok(File { blocks })
}

fn parse_block(tokens: &[Token], pos: usize) -> Result<(Block, usize), SyntaxError> {
    let kind = expect_ident(tokens, pos, "A file is blocks: query \"name\" { ... }")?;
    let name_token = tokens.get(pos + 1).ok_or_else(|| {
        SyntaxError::new(
            kind.line,
            kind.col,
            kind.span,
            format!("Block `{}` needs a name in quotes", kind.name),
        )
    })?;
    let TokenKind::Str(name) = &name_token.kind else {
        return Err(SyntaxError::at(
            name_token,
            format!("Block `{}` needs a name in quotes", kind.name),
        ));
    };
    let name_span = name_token.span;
    let mut pos = expect(tokens, pos + 2, TokenKind::LBrace, "{")?;
    let mut attrs = Vec::new();
    loop {
        let token = tokens.get(pos).ok_or_else(|| {
            SyntaxError::new(
                kind.line,
                kind.col,
                kind.span,
                format!("Block `{}` is never closed", kind.name),
            )
        })?;
        if token.kind == TokenKind::RBrace {
            return Ok((
                Block {
                    kind: kind.name,
                    name: name.clone(),
                    line: kind.line,
                    col: kind.col,
                    kind_span: kind.span,
                    name_span,
                    end_line: token.line,
                    attrs,
                },
                pos + 1,
            ));
        }
        let attr = expect_ident(tokens, pos, "An attribute is a name, `=`, and a value")?;
        pos = expect(tokens, pos + 1, TokenKind::Equals, "=")?;
        let mut refs = Vec::new();
        let (value, next) = parse_value(tokens, pos, &mut refs)?;
        attrs.push(Attr {
            name: attr.name,
            line: attr.line,
            col: attr.col,
            name_span: attr.span,
            value,
            refs,
        });
        pos = next;
    }
}

struct FoundIdent {
    name: String,
    line: usize,
    col: usize,
    span: (usize, usize),
}

fn expect_ident(tokens: &[Token], pos: usize, hint: &str) -> Result<FoundIdent, SyntaxError> {
    match tokens.get(pos) {
        Some(Token {
            kind: TokenKind::Ident(name),
            line,
            col,
            span,
        }) => Ok(FoundIdent {
            name: name.clone(),
            line: *line,
            col: *col,
            span: *span,
        }),
        Some(token) => Err(SyntaxError::at(token, hint)),
        None => Err(SyntaxError::at_end(tokens, hint)),
    }
}

fn expect(
    tokens: &[Token],
    pos: usize,
    kind: TokenKind,
    what: &str,
) -> Result<usize, SyntaxError> {
    match tokens.get(pos) {
        Some(token) if token.kind == kind => Ok(pos + 1),
        Some(token) => Err(SyntaxError::at(token, format!("Expected {what}"))),
        None => Err(SyntaxError::at_end(tokens, format!("Expected {what}"))),
    }
}

fn parse_value(
    tokens: &[Token],
    pos: usize,
    refs: &mut Vec<RefSite>,
) -> Result<(Value, usize), SyntaxError> {
    let token = tokens.get(pos).ok_or_else(|| {
        SyntaxError::at_end(tokens, "An attribute needs a value after `=`")
    })?;
    match &token.kind {
        TokenKind::Str(text) => Ok((Value::Str(text.clone()), pos + 1)),
        TokenKind::Heredoc(text) => Ok((Value::Heredoc(text.clone()), pos + 1)),
        TokenKind::Number(text) => Ok((Value::Number(text.clone()), pos + 1)),
        TokenKind::Ident(word) if word == "true" => Ok((Value::Bool(true), pos + 1)),
        TokenKind::Ident(word) if word == "false" => Ok((Value::Bool(false), pos + 1)),
        TokenKind::Ident(word) => {
            let mut site = RefSite {
                segments: vec![RefSegment {
                    name: word.clone(),
                    line: token.line,
                    col: token.col,
                    span: token.span,
                }],
            };
            let mut next = pos + 1;
            while tokens.get(next).map(|t| &t.kind) == Some(&TokenKind::Dot) {
                let segment = expect_ident(
                    tokens,
                    next + 1,
                    "A reference is segments joined by dots: query.latency",
                )?;
                site.segments.push(RefSegment {
                    name: segment.name,
                    line: segment.line,
                    col: segment.col,
                    span: segment.span,
                });
                next += 2;
            }
            let segments = site.segments.iter().map(|s| s.name.clone()).collect();
            refs.push(site);
            Ok((Value::Ref(segments), next))
        }
        TokenKind::LBracket => {
            let mut values = Vec::new();
            let mut next = pos + 1;
            loop {
                let token = tokens.get(next).ok_or_else(|| {
                    SyntaxError::at(token, "A list is never closed")
                })?;
                if token.kind == TokenKind::RBracket {
                    return Ok((Value::List(values), next + 1));
                }
                if !values.is_empty() {
                    next = expect(tokens, next, TokenKind::Comma, "`,` between list values")?;
                }
                let (value, after) = parse_value(tokens, next, refs)?;
                values.push(value);
                next = after;
            }
        }
        other => Err(SyntaxError::at(
            token,
            format!("Not a value: {other:?}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(source: &str) -> File {
        parse(source).unwrap_or_else(|e| panic!("line {}: {}", e.line, e.message))
    }

    #[test]
    fn tokens_carry_columns_and_spans() {
        let source = "query \"q\" {\n  sql = <<SQL\n  SELECT 1\n  SQL\n}";
        let tokens = lex(source).unwrap();
        let token = |kind: &TokenKind| {
            tokens
                .iter()
                .find(|t| &t.kind == kind)
                .unwrap_or_else(|| panic!("no token {kind:?}"))
        };
        // Every span slices its own lexeme back out of the source.
        let ident = token(&TokenKind::Ident("query".into()));
        assert_eq!((ident.line, ident.col), (1, 1));
        assert_eq!(&source[ident.span.0..ident.span.1], "query");
        let name = token(&TokenKind::Str("q".into()));
        assert_eq!((name.line, name.col, name.span), (1, 7, (6, 9)));
        assert_eq!(&source[name.span.0..name.span.1], "\"q\"");

        // A heredoc's span runs from `<<` through its closing delimiter,
        // excluding the newline that follows; its line and column are the
        // opening line's.
        let heredoc = token(&TokenKind::Heredoc("SELECT 1".into()));
        assert_eq!((heredoc.line, heredoc.col), (2, 9));
        assert_eq!(heredoc.span, (20, 42));
        assert_eq!(&source[heredoc.span.0..heredoc.span.1], "<<SQL\n  SELECT 1\n  SQL");

        let close = token(&TokenKind::RBrace);
        assert_eq!((close.line, close.col, close.span), (5, 1, (43, 44)));
    }

    #[test]
    fn a_minimal_dashboard_parses() {
        let source = r#"
// the day's latency, by service
query "latency" {
  sql = <<SQL
    SELECT timestamp, service, avg(latency) AS latency
    FROM logs
    GROUP BY timestamp, service
  SQL
}

plot "latency" {
  type   = "line"
  query  = query.latency
  x      = timestamp
  y      = latency
  series = service
}
"#;
        let file = parse_ok(source);
        assert_eq!(file.blocks.len(), 2);
        let query = &file.blocks[0];
        assert_eq!(query.kind, "query");
        assert_eq!(query.name, "latency");
        let Value::Heredoc(sql) = &query.attrs[0].value else {
            panic!("sql is a heredoc: {:?}", query.attrs[0]);
        };
        // The closing delimiter's two-space indent came off; the SQL keeps the
        // indentation it owns beyond that.
        assert!(sql.starts_with("  SELECT timestamp"), "{sql}");
        assert!(sql.contains("\n  GROUP BY"), "{sql}");

        // The block points at its kind keyword, its quoted name, and its
        // closing brace; the attribute points at its name.
        assert_eq!((query.line, query.col), (3, 1));
        assert_eq!(&source[query.kind_span.0..query.kind_span.1], "query");
        assert_eq!(query.name_span, (40, 49));
        assert_eq!(&source[query.name_span.0..query.name_span.1], "\"latency\"");
        assert_eq!(query.end_line, 9);
        assert_eq!((query.attrs[0].line, query.attrs[0].col), (4, 3));
        assert_eq!(&source[query.attrs[0].name_span.0..query.attrs[0].name_span.1], "sql");

        let plot = &file.blocks[1];
        assert_eq!(plot.kind, "plot");
        assert_eq!(
            plot.attrs[1].value,
            Value::Ref(vec!["query".into(), "latency".into()])
        );
        assert_eq!(plot.attrs[2].value, Value::Ref(vec!["timestamp".into()]));

        // The reference `query.latency` resolves per segment: each carries its
        // own line, column, and span.
        assert_eq!(plot.attrs[1].refs.len(), 1);
        let site = &plot.attrs[1].refs[0];
        assert_eq!(site.segments.len(), 2);
        assert_eq!((site.segments[0].line, site.segments[0].col), (13, 12));
        assert_eq!(&source[site.segments[0].span.0..site.segments[0].span.1], "query");
        assert_eq!((site.segments[1].line, site.segments[1].col), (13, 18));
        assert_eq!(&source[site.segments[1].span.0..site.segments[1].span.1], "latency");
    }

    #[test]
    fn value_forms_round_trip() {
        let file = parse_ok(
            r#"
plot "p" {
  s = "text \"quoted\""
  h = <<H
raw
  text
H
  n = -12.5
  b = true
  l = ["a", 1, false]
}
"#,
        );
        let attrs = &file.blocks[0].attrs;
        assert_eq!(attrs[0].value, Value::Str("text \"quoted\"".into()));
        assert_eq!(attrs[1].value, Value::Heredoc("raw\n  text".into()));
        assert_eq!(attrs[2].value, Value::Number("-12.5".into()));
        assert_eq!(attrs[3].value, Value::Bool(true));
        assert_eq!(
            attrs[4].value,
            Value::List(vec![
                Value::Str("a".into()),
                Value::Number("1".into()),
                Value::Bool(false),
            ])
        );
        // References inside a list are collected too.
        let file = parse_ok("plot \"p\" { l = [a, b.c] }");
        let refs = &file.blocks[0].attrs[0].refs;
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].segments[0].name, "a");
        assert_eq!(
            refs[1].segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["b", "c"]
        );
    }

    #[test]
    fn newlines_are_not_separators_but_everything_still_lines_up() {
        let file = parse_ok("plot \"p\" { x = a\ny = b }");
        let attrs = &file.blocks[0].attrs;
        assert_eq!(attrs[0].name, "x");
        assert_eq!(attrs[1].name, "y");
        assert_eq!(attrs[1].line, 2);
        assert_eq!((attrs[0].line, attrs[0].col, attrs[0].name_span), (1, 12, (11, 12)));
        assert_eq!((attrs[1].line, attrs[1].col, attrs[1].name_span), (2, 1, (17, 18)));
    }

    #[test]
    fn errors_name_their_line() {
        let error = parse("plot {\n").unwrap_err();
        assert_eq!(error.line, 1);
        assert!(error.message.contains("name"), "{}", error.message);
        // The `{` where the quoted name should be.
        assert_eq!((error.col, error.span), (6, (5, 6)));

        let error = parse("plot \"p\" {\n  x = \"unterminated\n}").unwrap_err();
        assert_eq!(error.line, 2);
        // The string's opening quote, on its own line.
        assert_eq!(error.col, 7);

        let source = "query \"q\" {\n  sql = <<SQL\n  SELECT 1\n}";
        let error = parse(source).unwrap_err();
        assert_eq!(error.line, 2);
        assert!(error.message.contains("SQL"), "{}", error.message);
        // The heredoc's `<<`, spanning to the end of input.
        assert_eq!(error.col, 9);
        assert_eq!(error.span, (20, source.len()));

        let error = parse("plot \"p\" { x = @ }").unwrap_err();
        assert_eq!(error.line, 1);
        assert_eq!((error.col, error.span), (16, (15, 16)));
    }
}
