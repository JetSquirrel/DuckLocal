//! Completion candidates for `.dash` source, computed from the text alone.
//!
//! This is the shared half of spec completion: the GUI wraps it in gpui's
//! `CompletionProvider` (see `view.rs`), and the language server wraps it in
//! LSP `CompletionItem`s. Neither dependency is here — a candidate is plain
//! data, and the caller owns prefix filtering and the replace range.
//!
//! Positions follow the rest of the spec crate: `line` and `col` are 1-based,
//! columns count characters. Candidates come back unfiltered, in the order
//! they should rank; matching the typed prefix is the caller's job, because
//! the GUI filters against the editor rope and an LSP client filters itself.

use super::model;

/// One thing that could be inserted at the cursor.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Completion {
    /// What the menu shows and what prefix matching runs against.
    pub label: String,
    pub kind: CompletionKind,
    /// A short note beside the label, in the spirit of the SQL provider's
    /// `schema · database` — static, and intentionally not localized: these
    /// read as vocabulary, not sentences.
    pub detail: Option<String>,
    /// What accepting the candidate inserts in place of the typed prefix.
    pub insert_text: String,
}

/// What a candidate is, so each frontend can pick its own icon: gpui's
/// `CompletionItemKind` for the GUI, the LSP's for the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionKind {
    /// `query` / `plot` at the top level.
    Block,
    /// An attribute name inside a block.
    Attribute,
    /// One of the plot types after `type =`.
    Value,
    /// A query block's name after `query = query.`.
    Reference,
}

/// The candidates the cursor's context allows, unfiltered.
///
/// Four contexts, checked in the order a more specific one wins: inside a
/// heredoc there is nothing to say; after `query = query.` the query names;
/// after `type =` the plot types; otherwise the attribute names of the
/// enclosing block, or the block types at the top level.
///
/// A file that parses answers from its tree; a file being edited often does
/// not parse, and completion matters most exactly then — so a syntax error
/// falls back to scanning lines, and query names to reading `query "name"`
/// openers out of the raw text.
pub(crate) fn complete(source: &str, line: usize, col: usize) -> Vec<Completion> {
    let lines: Vec<&str> = source.lines().collect();
    // Past the last line the cursor sits at the top level of the file.
    let cursor_line = lines.get(line.saturating_sub(1)).copied().unwrap_or("");
    let before: String = cursor_line
        .chars()
        .take(col.saturating_sub(1))
        .collect();

    if inside_heredoc(&lines, line) {
        return Vec::new();
    }

    if after_query_dot(&before) {
        return query_names(source)
            .into_iter()
            .map(|name| Completion {
                label: name.clone(),
                kind: CompletionKind::Reference,
                detail: Some("query block".to_string()),
                insert_text: name,
            })
            .collect();
    }

    if let Some(quoted) = after_type_equals(&before) {
        return model::PLOT_TYPES
            .iter()
            .map(|ty| Completion {
                label: ty.to_string(),
                kind: CompletionKind::Value,
                detail: Some("plot type".to_string()),
                // Inside an open quote the bare word; outside one, bring the
                // quotes — the value is a string either way.
                insert_text: if quoted {
                    ty.to_string()
                } else {
                    format!("\"{ty}\"")
                },
            })
            .collect();
    }

    match block_at(source, &lines, line, &before) {
        Some(kind) if kind == "plot" => ["type", "query", "x", "y", "series", "title"]
            .into_iter()
            .map(|name| attribute(name, "plot"))
            .collect(),
        Some(kind) if kind == "query" => vec![attribute("sql", "query")],
        // An unknown block kind has no known attributes to offer.
        Some(_) => Vec::new(),
        None => ["query", "plot"]
            .into_iter()
            .map(|name| Completion {
                label: name.to_string(),
                kind: CompletionKind::Block,
                detail: Some("dashboard block".to_string()),
                insert_text: name.to_string(),
            })
            .collect(),
    }
}

fn attribute(name: &str, block: &str) -> Completion {
    Completion {
        label: name.to_string(),
        kind: CompletionKind::Attribute,
        detail: Some(format!("{block} attribute")),
        insert_text: format!("{name} = "),
    }
}

/// The cursor sits in a heredoc body: between a `<<DELIM` opener and the line
/// that is exactly `DELIM`.
fn inside_heredoc(lines: &[&str], line: usize) -> bool {
    let mut delimiter: Option<String> = None;
    for raw in lines.iter().take(line.saturating_sub(1)) {
        if delimiter.as_deref() == Some(raw.trim()) {
            delimiter = None;
        } else if delimiter.is_none() {
            if let Some(opened) = heredoc_delimiter(raw) {
                delimiter = Some(opened);
            }
        }
    }
    delimiter.is_some()
}

/// The delimiter a `<<SQL`-style opener introduces, if the line has one and
/// nothing but whitespace follows it (anything else is a syntax error, not an
/// opener).
fn heredoc_delimiter(line: &str) -> Option<String> {
    let start = line.find("<<")? + 2;
    let rest = &line[start..];
    let delimiter: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if delimiter.is_empty() || !rest[delimiter.len()..].trim().is_empty() {
        return None;
    }
    Some(delimiter)
}

/// The cursor follows `query = query.`, possibly part-way into the name.
fn after_query_dot(before: &str) -> bool {
    let before = before.trim_end();
    let ident_len = trailing_ident_len(before);
    let head = &before[..before.len() - ident_len];
    let Some(head) = head.strip_suffix("query.") else {
        return false;
    };
    let Some(head) = head.trim_end().strip_suffix('=') else {
        return false;
    };
    head.trim() == "query"
}

/// The cursor follows `type =`, optionally inside an opened quote. The answer
/// is whether the quote is there, so the insert text knows to bring its own.
fn after_type_equals(before: &str) -> Option<bool> {
    let before = before.trim_end();
    let ident_len = trailing_ident_len(before);
    let head = &before[..before.len() - ident_len];
    let (head, quoted) = match head.strip_suffix('"') {
        Some(head) => (head, true),
        None => (head, false),
    };
    let head = head.trim_end().strip_suffix('=')?;
    (head.trim() == "type").then_some(quoted)
}

/// Length of the ASCII identifier a line ends in — the partial word being
/// typed. Identifiers are ASCII by the grammar, so byte arithmetic is safe.
fn trailing_ident_len(text: &str) -> usize {
    text.len()
        - text
            .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
            .len()
}

/// The kind of block enclosing `line`, or `None` at the top level. A parsed
/// file answers exactly, from the blocks' line extents; a broken one falls
/// back to a forward scan that tracks openers, closers and heredocs up to the
/// cursor — the states are the same three, only the precision differs.
fn block_at(source: &str, lines: &[&str], line: usize, before: &str) -> Option<String> {
    if let Ok(file) = model::parse(source) {
        return file
            .blocks
            .iter()
            .find(|block| block.line <= line && line <= block.end_line)
            .map(|block| block.kind.clone());
    }

    let mut kind: Option<String> = None;
    let mut delimiter: Option<String> = None;
    for raw in lines.iter().take(line.saturating_sub(1)) {
        let trimmed = raw.trim();
        if let Some(open) = &delimiter {
            if trimmed == open {
                delimiter = None;
            }
            continue;
        }
        if let Some(opened) = heredoc_delimiter(raw) {
            delimiter = Some(opened);
        }
        if trimmed.starts_with('}') {
            kind = None;
        }
        if let Some(opened) = block_opener(trimmed) {
            kind = Some(opened);
        }
    }
    // The cursor's own line up to the cursor: a one-line block opens there.
    // Whatever follows the line's last `}` is the state that counts.
    let effective = before.rsplit('}').next().unwrap_or(before);
    if let Some(opened) = block_opener(effective.trim_start()) {
        return Some(opened);
    }
    if before.contains('}') {
        return None;
    }
    kind
}

/// `query "name"` or `plot "name"` at the start of a trimmed line: the kind,
/// if a quoted name follows it.
fn block_opener(trimmed: &str) -> Option<String> {
    let ident_len = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .count();
    if ident_len == 0 {
        return None;
    }
    trimmed[ident_len..]
        .trim_start()
        .starts_with('"')
        .then(|| trimmed[..ident_len].to_string())
}

/// The query block names the file defines: from the tree when it parses, from
/// a scan of `query "name"` openers when it does not — mid-edit is exactly
/// when the reference is being typed.
fn query_names(source: &str) -> Vec<String> {
    if let Ok(file) = model::parse(source) {
        return file
            .blocks
            .iter()
            .filter(|block| block.kind == "query")
            .map(|block| block.name.clone())
            .collect();
    }
    let mut names = Vec::new();
    for line in source.lines() {
        if let Some(name) = scan_query_name(line.trim_start()) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// The name of a `query "name"` opener, with a word boundary after `query` so
/// an attribute like `query = "..."` is not one.
fn scan_query_name(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("query")?;
    if rest
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(candidates: &[Completion]) -> Vec<&str> {
        candidates.iter().map(|c| c.label.as_str()).collect()
    }

    const SPEC: &str = "query \"revenue\" {\n  sql = <<SQL\n    SELECT 1\n  SQL\n}\n\nplot \"p\" {\n  type = \"bar\"\n  query = query.revenue\n  x = channel\n}\n";

    #[test]
    fn block_types_at_the_top_level() {
        let candidates = complete("", 1, 1);
        assert_eq!(labels(&candidates), ["query", "plot"]);
        assert!(candidates
            .iter()
            .all(|c| c.kind == CompletionKind::Block));

        // Between two blocks, and past the last one.
        assert_eq!(labels(&complete(SPEC, 6, 1)), ["query", "plot"]);
        assert_eq!(labels(&complete(SPEC, 12, 1)), ["query", "plot"]);
    }

    #[test]
    fn attribute_names_follow_the_enclosing_block() {
        // Line 8 of SPEC is `  type = "bar"`, inside the plot block; a fresh
        // line inside it gets every plot attribute.
        let candidates = complete("plot \"p\" {\n  \n}", 2, 3);
        assert_eq!(
            labels(&candidates),
            ["type", "query", "x", "y", "series", "title"]
        );
        assert!(candidates
            .iter()
            .all(|c| c.kind == CompletionKind::Attribute));
        assert_eq!(candidates[0].insert_text, "type = ");

        let candidates = complete("query \"q\" {\n  s\n}", 2, 3);
        assert_eq!(labels(&candidates), ["sql"]);
    }

    #[test]
    fn plot_types_after_type_equals() {
        // Inside an opened quote the insert text is the bare word.
        let candidates = complete("plot \"p\" {\n  type = \"li\n}", 2, 12);
        assert_eq!(labels(&candidates), ["line", "bar", "area", "scatter", "table"]);
        assert!(candidates
            .iter()
            .all(|c| c.kind == CompletionKind::Value));
        assert_eq!(candidates[0].insert_text, "line");

        // Without a quote the insert text brings its own.
        let candidates = complete("plot \"p\" {\n  type = \n}", 2, 11);
        assert_eq!(candidates[0].insert_text, "\"line\"");
    }

    #[test]
    fn query_names_after_query_dot() {
        let source = "query \"revenue\" {\n  sql = \"SELECT 1\"\n}\nquery \"cost\" {\n  sql = \"SELECT 2\"\n}\nplot \"p\" {\n  query = query.\n}\n";
        let candidates = complete(source, 8, 17);
        assert_eq!(labels(&candidates), ["revenue", "cost"]);
        assert!(candidates
            .iter()
            .all(|c| c.kind == CompletionKind::Reference));

        // Part-way into a name the context still holds; filtering is the
        // caller's job, so both names still come back.
        let partial = source.replace("query = query.\n", "query = query.re\n");
        let candidates = complete(&partial, 8, 19);
        assert_eq!(labels(&candidates), ["revenue", "cost"]);
    }

    #[test]
    fn a_broken_file_still_completes() {
        // Both blocks unclosed: parse fails, and the answers come from the
        // text scan instead.
        let source = "query \"q1\" {\n  sql = \"SELECT 1\"\n\nplot \"p\" {\n  query = query.\n";
        let candidates = complete(source, 5, 17);
        assert_eq!(labels(&candidates), ["q1"]);

        // The enclosing block is found by scanning backwards past the
        // unclosed query block to the plot opener.
        let source = "query \"q1\" {\n  sql = \"SELECT 1\"\n\nplot \"p\" {\n  t\n";
        assert_eq!(
            labels(&complete(source, 5, 3)),
            ["type", "query", "x", "y", "series", "title"]
        );
    }

    #[test]
    fn inside_a_heredoc_there_is_nothing_to_offer() {
        // Line 3 of SPEC is the SQL body.
        assert_eq!(complete(SPEC, 3, 5), Vec::new());
        // The attribute context returns on the line after the delimiter.
        let candidates = complete("query \"q\" {\n  sql = <<SQL\nx\nSQL\n  \n}", 5, 3);
        assert_eq!(labels(&candidates), ["sql"]);
    }

    #[test]
    fn an_attribute_named_query_is_not_a_query_block() {
        // The fallback scan must not read `query = "..."` as a block opener.
        let source = "query \"q1\" {\n  sql = \"SELECT 1\"\n}\nplot \"p\" {\n  query = query.\n";
        assert_eq!(labels(&complete(source, 5, 17)), ["q1"]);
    }
}
