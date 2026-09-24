//! The `.dash` language server: `ducklocal lsp [--database PATH]`.
//!
//! A stdio LSP server so any editor gets what the GUI's own spec editor
//! has: diagnostics, completion, hover, and go-to-definition. The server
//! owns no spec logic — diagnostics are `syntax::parse` + `model::validate`
//! (plus each query's SQL through the real DuckDB parser when `--database`
//! is given), completion is `complete::complete`, and hover/definition are
//! `Spec::locate`. Positions are the one thing the protocol owns: LSP
//! counts columns in UTF-16 code units, the spec crate in characters, so
//! every position crosses the two helpers at the bottom of this file.
//!
//! Sync is full-document: small files, one parse per change, no incremental
//! bookkeeping to get wrong.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use lsp_server::{Connection, ErrorCode, Message, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Exit as ExitNotification, Notification as _, PublishDiagnostics,
};
use lsp_types::request::{Completion, GotoDefinition, HoverRequest, Request as _, Shutdown};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    CompletionTextEdit, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, HoverProviderCapability, Location,
    MarkupContent, MarkupKind, OneOf, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Uri as Url,
};
use serde_json::json;

use super::complete::{self, CompletionKind};
use super::model::{self, Hit, Spec};
use crate::cli::{parse_args, Arg, CliError, FlagSpec};

const SPEC: &[FlagSpec] = &[FlagSpec {
    name: "--database",
    takes_value: true,
}];

/// The command's entry point: argument errors are the CLI's JSON-on-stderr
/// contract; once the protocol starts, stdout belongs to LSP frames alone.
pub fn main(args: &[OsString]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "{}",
                json!({"error": {"kind": error.kind, "message": error.message}})
            );
            error.code
        }
    }
}

fn run(args: &[OsString]) -> Result<i32, CliError> {
    let mut database = None;
    for arg in parse_args("lsp", args, SPEC, &[])? {
        match arg {
            Arg::Flag("--database", Some(value)) => {
                database = Some(crate::cli::database_path(&value)?);
            }
            Arg::Positional(value) => {
                return Err(CliError::argument(format!(
                    "Unknown lsp option: {}",
                    value.to_string_lossy()
                )));
            }
            _ => {
                return Err(CliError::failure(
                    "internal",
                    "the argument walker produced a flag lsp does not declare",
                ));
            }
        }
    }
    serve(database)
}

fn serve(database: Option<PathBuf>) -> Result<i32, CliError> {
    // The io threads are never joined: the writer only finishes once every
    // sender drops and the reader only once the client closes stdin, and an
    // `exit` notification requires leaving before either has to happen.
    // Dropping the handles detaches them; `main` exits the process, and the
    // shutdown response is long sent by then.
    let (connection, _io_threads) = Connection::stdio();
    let capabilities = serde_json::to_value(ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        completion_provider: Some(CompletionOptions::default()),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        ..ServerCapabilities::default()
    })
    .expect("server capabilities serialize");
    connection
        .initialize(capabilities)
        .map_err(|error| CliError {
            kind: "lsp",
            message: error.to_string(),
            code: 2,
        })?;

    let mut documents: HashMap<Url, String> = HashMap::new();
    let mut shutdown = false;
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                let id = request.id.clone();
                let response = match request.method.as_str() {
                    Shutdown::METHOD => {
                        shutdown = true;
                        Response::new_ok(id, ())
                    }
                    Completion::METHOD => {
                        answer::<CompletionParams, _>(id, request.params, |_uri, position, source| {
                            complete_at(source, position)
                        }, &documents)
                    }
                    HoverRequest::METHOD => {
                        answer::<HoverParams, _>(id, request.params, |_uri, position, source| {
                            hover_at(source, position)
                        }, &documents)
                    }
                    GotoDefinition::METHOD => {
                        answer::<GotoDefinitionParams, _>(id, request.params, |uri, position, source| {
                            definition_at(source, uri, position)
                        }, &documents)
                    }
                    method => Response::new_err(
                        id,
                        ErrorCode::MethodNotFound as i32,
                        format!("ducklocal lsp does not answer {method}"),
                    ),
                };
                connection
                    .sender
                    .send(Message::Response(response))
                    .map_err(|e| CliError::failure("io", e))?;
            }
            Message::Notification(notification) => match notification.method.as_str() {
                DidOpenTextDocument::METHOD => {
                    match serde_json::from_value::<DidOpenTextDocumentParams>(notification.params)
                    {
                        Ok(params) => {
                            let document = params.text_document;
                            documents.insert(document.uri.clone(), document.text);
                            publish(&connection, &documents, &document.uri, database.as_deref());
                        }
                        Err(error) => {
                            tracing::warn!("bad didOpen params: {error}");
                        }
                    }
                }
                DidChangeTextDocument::METHOD => {
                    match serde_json::from_value::<DidChangeTextDocumentParams>(notification.params)
                    {
                        Ok(params) => {
                            // Full sync: the last change carries the whole
                            // document. An empty change list changes nothing.
                            if let Some(change) = params.content_changes.into_iter().last() {
                                documents.insert(params.text_document.uri.clone(), change.text);
                                publish(
                                    &connection,
                                    &documents,
                                    &params.text_document.uri,
                                    database.as_deref(),
                                );
                            }
                        }
                        Err(error) => {
                            tracing::warn!("bad didChange params: {error}");
                        }
                    }
                }
                DidCloseTextDocument::METHOD => {
                    match serde_json::from_value::<DidCloseTextDocumentParams>(notification.params)
                    {
                        Ok(params) => {
                            let uri = params.text_document.uri;
                            documents.remove(&uri);
                            // Clearing the diagnostics is the client's cue to
                            // drop the file's squiggles everywhere.
                            send(
                                &connection,
                                lsp_server::Notification::new(
                                    PublishDiagnostics::METHOD.to_string(),
                                    PublishDiagnosticsParams {
                                        uri,
                                        diagnostics: Vec::new(),
                                        version: None,
                                    },
                                ),
                            )?;
                        }
                        Err(error) => {
                            tracing::warn!("bad didClose params: {error}");
                        }
                    }
                }
                DidSaveTextDocument::METHOD => {}
                ExitNotification::METHOD => {
                    return Ok(if shutdown { 0 } else { 1 });
                }
                _ => {}
            },
            Message::Response(_) => {}
        }
    }
    // The client went away without an exit notification: leave quietly,
    // reporting abnormal shutdown the way the protocol words it.
    Ok(if shutdown { 0 } else { 1 })
}

fn send(connection: &Connection, notification: lsp_server::Notification) -> Result<(), CliError> {
    connection
        .sender
        .send(Message::Notification(notification))
        .map_err(|e| CliError::failure("io", e))
}

/// Publish the document's diagnostics after an open or change.
fn publish(
    connection: &Connection,
    documents: &HashMap<Url, String>,
    uri: &Url,
    database: Option<&std::path::Path>,
) {
    let Some(source) = documents.get(uri) else {
        return;
    };
    let diagnostics = diagnose(source, database);
    let _ = connection.sender.send(Message::Notification(
        lsp_server::Notification::new(
            PublishDiagnostics::METHOD.to_string(),
            PublishDiagnosticsParams {
                uri: uri.clone(),
                diagnostics,
                version: None,
            },
        ),
    ));
}

/// Every complaint about the document, in the order `check` would list them:
/// the parse error alone, else every semantic diagnostic, plus each query's
/// SQL through the real DuckDB parser when a database was named. All
/// severities are errors: every one of these is a file `check` rejects and
/// the GUI refuses to draw, not a style note.
fn diagnose(source: &str, database: Option<&std::path::Path>) -> Vec<lsp_types::Diagnostic> {
    let file = match model::parse(source) {
        Ok(file) => file,
        Err(error) => {
            return vec![diagnostic(span_range(source, error.span), error.message)];
        }
    };
    let spec = match model::validate(&file) {
        Ok(spec) => spec,
        Err(diagnostics) => {
            return diagnostics
                .into_iter()
                .map(|d| diagnostic(span_range(source, d.span), d.message))
                .collect();
        }
    };
    let mut out = Vec::new();
    if database.is_some() {
        // Point at the sql attribute's name: the query's line is `check`'s
        // contract, but an editor wants a span, and the name is the precise
        // place a broken statement can be fixed.
        for query in &spec.queries {
            if let Err(message) = super::validate_query_sql(&query.sql) {
                let span = file
                    .blocks
                    .iter()
                    .find(|block| block.kind == "query" && block.name == query.name)
                    .and_then(|block| block.attrs.iter().find(|attr| attr.name == "sql"))
                    .map(|attr| attr.name_span)
                    .unwrap_or(query.span);
                out.push(diagnostic(
                    span_range(source, span),
                    format!("query {:?}: {message}", query.name),
                ));
            }
        }
    }
    out
}

fn diagnostic(range: Range, message: String) -> lsp_types::Diagnostic {
    lsp_types::Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("ducklocal".to_string()),
        message,
        ..Default::default()
    }
}

/// The shared shape of the position-based requests: parse the params, find
/// the document, compute. A missing document or a wrong param shape is a
/// null answer, never a panic — an editor asking about a closed file is a
/// race, not a bug.
fn answer<P, R>(
    id: RequestId,
    params: serde_json::Value,
    compute: impl Fn(&Url, Position, &str) -> Option<R>,
    documents: &HashMap<Url, String>,
) -> Response
where
    P: serde::de::DeserializeOwned + PositionParams,
    R: serde::Serialize,
{
    let params: P = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(error) => {
            return Response::new_err(id, ErrorCode::InvalidParams as i32, error.to_string());
        }
    };
    let (uri, position) = params.uri_position();
    let result = documents
        .get(uri)
        .and_then(|source| compute(uri, position, source));
    Response::new_ok(id, result)
}

/// What the three position requests share, spelled once.
trait PositionParams {
    fn uri_position(&self) -> (&Url, Position);
}

impl PositionParams for CompletionParams {
    fn uri_position(&self) -> (&Url, Position) {
        (
            &self.text_document_position.text_document.uri,
            self.text_document_position.position,
        )
    }
}

impl PositionParams for HoverParams {
    fn uri_position(&self) -> (&Url, Position) {
        (
            &self.text_document_position_params.text_document.uri,
            self.text_document_position_params.position,
        )
    }
}

impl PositionParams for GotoDefinitionParams {
    fn uri_position(&self) -> (&Url, Position) {
        (
            &self.text_document_position_params.text_document.uri,
            self.text_document_position_params.position,
        )
    }
}

/// Completion at a position: the shared candidate list, each given a text
/// edit that replaces the identifier prefix being typed — the same range
/// the GUI computes from its rope. Candidates go out unfiltered; the client
/// filters against its own typed text.
fn complete_at(source: &str, position: Position) -> Option<CompletionResponse> {
    let (line, col) = line_col(source, position);
    let line_text = source
        .lines()
        .nth(position.line as usize)
        .unwrap_or("");
    let (_, prefix_units) = ident_prefix(line_text, col.saturating_sub(1));
    let range = Range {
        start: Position::new(position.line, position.character - prefix_units),
        end: position,
    };
    let candidates = complete::complete(source, line, col);
    let items = candidates
        .into_iter()
        .map(|candidate| CompletionItem {
            label: candidate.label,
            kind: Some(match candidate.kind {
                CompletionKind::Block => CompletionItemKind::KEYWORD,
                CompletionKind::Attribute => CompletionItemKind::PROPERTY,
                CompletionKind::Value => CompletionItemKind::ENUM_MEMBER,
                CompletionKind::Reference => CompletionItemKind::REFERENCE,
            }),
            detail: candidate.detail,
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range,
                new_text: candidate.insert_text,
            })),
            ..Default::default()
        })
        .collect();
    Some(CompletionResponse::Array(items))
}

/// Hover at a position: what the thing under the cursor is. A file with
/// errors has no model to locate in, and hovering over half-typed code is
/// the common case — so a broken file hovers nothing.
fn hover_at(source: &str, position: Position) -> Option<Hover> {
    let (line, col) = line_col(source, position);
    let spec = validate(source)?;
    let text = match spec.locate(line, col)? {
        Hit::Attr { name, .. } => attr_doc(&name)?.to_string(),
        Hit::Block { kind, name, .. } => match kind.as_str() {
            "query" => format!("**query \"{name}\"**\n\nA named query: one `sql` attribute holding one statement, as a heredoc or a string."),
            "plot" => format!("**plot \"{name}\"**\n\nA named plot: `type`, `query`, `x` and `y`, plus optional `series` and `title`."),
            _ => return None,
        },
        Hit::Ref { segments, .. } => {
            if segments.len() == 2 && segments[0] == "query" {
                let query = spec.queries.iter().find(|q| q.name == segments[1])?;
                format!(
                    "**query \"{}\"**\n\n```sql\n{}\n```",
                    query.name,
                    sql_summary(&query.sql)
                )
            } else {
                return None;
            }
        }
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: text,
        }),
        range: None,
    })
}

/// Go-to-definition: a `query.name` reference resolves to the query block's
/// quoted name, in this same file. Anything else has nowhere to go.
fn definition_at(source: &str, uri: &Url, position: Position) -> Option<GotoDefinitionResponse> {
    let (line, col) = line_col(source, position);
    let spec = validate(source)?;
    let Hit::Ref { segments, .. } = spec.locate(line, col)? else {
        return None;
    };
    if segments.len() != 2 || segments[0] != "query" {
        return None;
    }
    let query = spec.queries.iter().find(|q| q.name == segments[1])?;
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: uri.clone(),
        range: span_range(source, query.name_span),
    }))
}

/// The validated model, or nothing — hover and definition both answer from
/// `Spec::locate`, which only a clean file has.
fn validate(source: &str) -> Option<Spec> {
    model::validate(&model::parse(source).ok()?).ok()
}

/// One line of documentation per attribute name — the language rules of
/// <https://ducklocal.app/docs/dashboards>, not localized: like completion details, these read as
/// vocabulary, not sentences.
fn attr_doc(name: &str) -> Option<&'static str> {
    Some(match name {
        "sql" => "**sql**\n\nThe query's one statement, as a heredoc or a string. A query block holds only this.",
        "type" => "**type**\n\nThe plot's kind: one of `line`, `bar`, `area`, `scatter`, `table`.",
        "query" => "**query**\n\nThe query block this plot draws: `query = query.some_name`.",
        "x" => "**x**\n\nA column of the query's result, as a bare identifier or a quoted string.",
        "y" => "**y**\n\nThe numeric column the plot draws; optional when the type is `table`.",
        "series" => "**series**\n\nOptional: the column that splits the plot into one line or bar group per value.",
        "title" => "**title**\n\nOptional: the plot's title.",
        _ => return None,
    })
}

/// The first non-empty line of a query's SQL, cut to a readable length: the
/// hover answers "which query is this", not "what does it say in full".
fn sql_summary(sql: &str) -> String {
    let line = sql.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    const LIMIT: usize = 80;
    if line.chars().count() > LIMIT {
        format!("{}…", line.chars().take(LIMIT).collect::<String>())
    } else {
        line.to_string()
    }
}

/// The identifier prefix immediately before `char_col` (a 0-based character
/// index): how many characters it is and how many UTF-16 units that spans,
/// so the completion text edit can replace exactly it. Identifiers are
/// ASCII by the grammar, but scanning is character-based all the same.
fn ident_prefix(line: &str, char_col: usize) -> (usize, u32) {
    let mut chars = 0usize;
    let mut units = 0u32;
    for ch in line.chars().take(char_col).collect::<Vec<_>>().into_iter().rev() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            chars += 1;
            units += ch.len_utf16() as u32;
        } else {
            break;
        }
    }
    (chars, units)
}

/// An LSP position as the spec crate's 1-based `(line, col)`: the line is a
/// count either way, the column crosses from UTF-16 units to characters.
/// A column inside a surrogate pair or past the line's end clamps to the
/// nearest character boundary rather than failing — the cursor races the
/// text during edits.
fn line_col(source: &str, position: Position) -> (usize, usize) {
    let line_text = source.lines().nth(position.line as usize).unwrap_or("");
    (position.line as usize + 1, utf16_col_to_char_col(line_text, position.character) + 1)
}

/// A 0-based UTF-16 column as a 0-based character index.
fn utf16_col_to_char_col(line: &str, utf16: u32) -> usize {
    let mut units = 0u32;
    for (index, ch) in line.chars().enumerate() {
        if units >= utf16 {
            return index;
        }
        units += ch.len_utf16() as u32;
    }
    line.chars().count()
}

/// A byte offset in the source as an LSP position: 0-based line, UTF-16
/// column. Diagnostics and definition targets are byte spans on our side,
/// so every one of them crosses here twice.
fn position_of(source: &str, offset: usize) -> Position {
    let mut line = 0u32;
    let mut character = 0u32;
    for (index, ch) in source.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }
    Position::new(line, character)
}

fn span_range(source: &str, span: (usize, usize)) -> Range {
    Range {
        start: position_of(source, span.0),
        end: position_of(source, span.1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_utf16_units() {
        // ASCII: bytes, characters and UTF-16 units all agree.
        assert_eq!(position_of("plot {\n", 5), Position::new(0, 5));
        assert_eq!(position_of("plot {\n", 7), Position::new(1, 0));
        // A CJK name is one unit per character, an emoji is two.
        let source = "query \"数据库\" {\n  x = 1\n}\n";
        let offset = source.find('库').unwrap();
        assert_eq!(position_of(source, offset), Position::new(0, 9));
        let source = "# 📈 charts\nplot {\n";
        let offset = source.find('p').unwrap();
        assert_eq!(position_of(source, offset), Position::new(1, 0));
        let offset = source.find('c').unwrap();
        assert_eq!(position_of(source, offset), Position::new(0, 5));
    }

    #[test]
    fn utf16_columns_cross_back_to_characters() {
        assert_eq!(utf16_col_to_char_col("plot {", 4), 4);
        assert_eq!(utf16_col_to_char_col("数据库 x", 0), 0);
        assert_eq!(utf16_col_to_char_col("数据库 x", 3), 3);
        // Inside a surrogate pair the column clamps to the character after.
        assert_eq!(utf16_col_to_char_col("📈x", 1), 1);
        assert_eq!(utf16_col_to_char_col("📈x", 2), 1);
        // Past the end clamps to the line's length.
        assert_eq!(utf16_col_to_char_col("plot", 99), 4);
    }

    #[test]
    fn line_col_round_trips_through_positions() {
        let source = "plot \"图\" {\n  y = 值\n}\n";
        // On the `y`: line 1 (0-based), column 2 — ASCII, so both agree.
        assert_eq!(line_col(source, Position::new(1, 2)), (2, 3));
        // On `值` (column 6 in characters and units alike here).
        assert_eq!(line_col(source, Position::new(1, 6)), (2, 7));
    }

    #[test]
    fn ident_prefixes_are_replaced() {
        assert_eq!(ident_prefix("  type = \"li", 12), (2, 2));
        assert_eq!(ident_prefix("  query = query.lat", 19), (3, 3));
        assert_eq!(ident_prefix("plot {", 6), (0, 0));
    }

    #[test]
    fn sql_summaries_stay_short() {
        assert_eq!(sql_summary("\n  SELECT 1\n"), "SELECT 1");
        let long = format!("SELECT {}", "x".repeat(200));
        assert_eq!(sql_summary(&long).chars().count(), 81);
    }
}
