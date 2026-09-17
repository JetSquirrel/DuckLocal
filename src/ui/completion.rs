//! SQL code completion for the query editor.
//!
//! Implements gpui-base's LSP-style `CompletionProvider`: SQL keywords and
//! DuckDB functions from static lists, table and column names from the live
//! catalog held by `AppState`.

use std::rc::Rc;

use anyhow::Result;
use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt};
use gpui_kit::{App, Entity, Task, Window};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    TextEdit,
};

use crate::schema::DatabaseInfo;
use crate::state::AppState;

/// Catalog names share the menu with the static lists; per-group floors keep
/// a wide catalog from crowding functions and keywords out entirely.
const MAX_CATALOG_ITEMS: usize = 35;
const MAX_FUNCTION_ITEMS: usize = 10;
const MAX_KEYWORD_ITEMS: usize = 5;
const MAX_ITEMS: usize = MAX_CATALOG_ITEMS + MAX_FUNCTION_ITEMS + MAX_KEYWORD_ITEMS;
/// Don't pop the menu up for a single stray keystroke context like spaces;
/// only complete once at least one identifier character exists.
const MAX_PREFIX_SCAN: usize = 64;

const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP BY",
    "ORDER BY",
    "LIMIT",
    "OFFSET",
    "JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "FULL OUTER JOIN",
    "CROSS JOIN",
    "ON",
    "AS",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IN",
    "IS",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "DISTINCT",
    "UNION",
    "UNION ALL",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "CREATE TABLE",
    "CREATE VIEW",
    "CREATE OR REPLACE VIEW",
    "DROP TABLE",
    "ALTER TABLE",
    "HAVING",
    "EXISTS",
    "CAST",
    "ASC",
    "DESC",
    "WITH",
    "COPY",
    "ATTACH",
    "DETACH",
    "USE",
    "DESCRIBE",
    "SHOW TABLES",
    "EXPLAIN",
    "PRAGMA",
    "SUMMARIZE",
    "PIVOT",
    "UNPIVOT",
    "QUALIFY",
    "OVER",
    "PARTITION BY",
    "RETURNING",
    "PRIMARY KEY",
    "DEFAULT",
    "BEGIN TRANSACTION",
    "COMMIT",
    "ROLLBACK",
    "INTERVAL",
    "DATE",
    "TIMESTAMP",
    "BOOLEAN",
    "INTEGER",
    "BIGINT",
    "DOUBLE",
    "DECIMAL",
    "VARCHAR",
    "BLOB",
    "UUID",
    "JSON",
];

const FUNCTIONS: &[&str] = &[
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "round",
    "abs",
    "ceil",
    "floor",
    "coalesce",
    "nullif",
    "ifnull",
    "upper",
    "lower",
    "length",
    "trim",
    "replace",
    "substring",
    "concat",
    "concat_ws",
    "split_part",
    "regexp_matches",
    "regexp_replace",
    "starts_with",
    "ends_with",
    "contains",
    "left",
    "right",
    "lpad",
    "rpad",
    "repeat",
    "reverse",
    "md5",
    "hash",
    "now",
    "current_date",
    "current_timestamp",
    "date_trunc",
    "date_part",
    "extract",
    "epoch",
    "strftime",
    "strptime",
    "date_diff",
    "date_add",
    "last_day",
    "monthname",
    "dayname",
    "year",
    "month",
    "day",
    "hour",
    "minute",
    "second",
    "string_agg",
    "list",
    "bool_and",
    "bool_or",
    "median",
    "quantile",
    "mode",
    "stddev",
    "variance",
    "first",
    "last",
    "arg_max",
    "arg_min",
    "approx_count_distinct",
    "histogram",
    "row_number",
    "rank",
    "dense_rank",
    "ntile",
    "lag",
    "lead",
    "first_value",
    "last_value",
    "cume_dist",
    "percent_rank",
    "power",
    "sqrt",
    "exp",
    "ln",
    "log10",
    "pi",
    "random",
    "greatest",
    "least",
    "read_parquet",
    "read_csv",
    "read_csv_auto",
    "read_json",
    "parquet_scan",
    "typeof",
    "try_cast",
    "version",
    "gen_random_uuid",
    "unnest",
    "range",
    "generate_series",
    "struct_pack",
    "struct_extract",
    "list_value",
    "list_extract",
    "json_extract",
    "json_extract_string",
    "map_extract",
];

pub struct SqlCompletionProvider {
    state: Entity<AppState>,
}

impl SqlCompletionProvider {
    pub fn new(state: Entity<AppState>) -> Rc<Self> {
        Rc::new(Self { state })
    }
}

/// A matched candidate name. Where it came from is kept as a tag so the LSP
/// item — with its `detail` and insert text — is only built for names that
/// actually made the cut.
pub struct CandidateName {
    name: String,
    /// Lowercased once, for dedup and `sort_text`.
    lower: String,
    source: CandidateSource,
}

enum CandidateSource {
    Table { schema: String, database: String },
    Column { data_type: String, table: String },
    Function,
    Keyword,
}

/// Candidate names matching `prefix_lower`, capped at `MAX_ITEMS`: catalog
/// tables and columns first (up to `MAX_CATALOG_ITEMS`), then functions, then
/// keywords, each static group guaranteed its own floor.
pub fn collect_candidates(catalog: &[DatabaseInfo], prefix_lower: &str) -> Vec<CandidateName> {
    let mut names: Vec<CandidateName> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    'catalog: for database in catalog {
        for table in &database.tables {
            push_match(
                &mut names,
                &mut seen,
                prefix_lower,
                MAX_CATALOG_ITEMS,
                &table.name,
                || CandidateSource::Table {
                    schema: table.schema.clone(),
                    database: database.name.clone(),
                },
            );
            for column in &table.columns {
                push_match(
                    &mut names,
                    &mut seen,
                    prefix_lower,
                    MAX_CATALOG_ITEMS,
                    &column.name,
                    || CandidateSource::Column {
                        data_type: column.data_type.clone(),
                        table: table.name.clone(),
                    },
                );
            }
            if names.len() >= MAX_CATALOG_ITEMS {
                break 'catalog;
            }
        }
    }

    // Each static group's cap is relative to the candidates already taken, so
    // it is a floor when the catalog is wide and grows when it is narrow.
    let function_cap = names.len() + MAX_FUNCTION_ITEMS;
    for function in FUNCTIONS {
        push_match(
            &mut names,
            &mut seen,
            prefix_lower,
            function_cap,
            function,
            || CandidateSource::Function,
        );
    }
    let keyword_cap = names.len() + MAX_KEYWORD_ITEMS;
    for keyword in KEYWORDS {
        push_match(
            &mut names,
            &mut seen,
            prefix_lower,
            keyword_cap,
            keyword,
            || CandidateSource::Keyword,
        );
    }

    debug_assert!(names.len() <= MAX_ITEMS);
    names
}

/// Record `name` if it matches the prefix, has not been seen, and `cap` —
/// the total length `names` may reach for this group — has not been hit.
/// `source` is only evaluated for a name that is kept, so describing a
/// candidate costs nothing for the many that do not match.
fn push_match(
    names: &mut Vec<CandidateName>,
    seen: &mut std::collections::HashSet<String>,
    prefix_lower: &str,
    cap: usize,
    name: &str,
    source: impl FnOnce() -> CandidateSource,
) {
    if names.len() >= cap || !starts_with_ignore_case(name, prefix_lower) {
        return;
    }
    let lower = name.to_lowercase();
    if seen.insert(lower.clone()) {
        names.push(CandidateName {
            name: name.to_string(),
            lower,
            source: source(),
        });
    }
}

/// `starts_with` against an already-lowercased prefix, without lowercasing
/// the candidate first. The ASCII path covers keywords, functions, and plain
/// identifiers; anything else falls back so case folding stays correct.
fn starts_with_ignore_case(name: &str, prefix_lower: &str) -> bool {
    if name.is_ascii() && prefix_lower.is_ascii() {
        name.len() >= prefix_lower.len()
            && name.as_bytes()[..prefix_lower.len()].eq_ignore_ascii_case(prefix_lower.as_bytes())
    } else {
        name.to_lowercase().starts_with(prefix_lower)
    }
}

/// Insert form for a catalog identifier: bare when it's a plain identifier,
/// double-quoted otherwise (DuckDB does not accept backticks).
pub(crate) fn identifier_insert(name: &str) -> String {
    let bare_safe = !name.is_empty()
        && name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if bare_safe {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

impl CompletionProvider for SqlCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        // Identifier prefix immediately before the cursor.
        let mut prefix_len = 0usize;
        while prefix_len < MAX_PREFIX_SCAN {
            let pos = offset.saturating_sub(prefix_len + 1);
            match text.get_char(pos).ok() {
                Some(c) if c.is_alphanumeric() || c == '_' => prefix_len += 1,
                _ => break,
            }
            if pos == 0 {
                break;
            }
        }
        if prefix_len == 0 {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        }
        let start = offset - prefix_len;
        let prefix = text.slice(start..offset).to_string();
        let prefix_lower = prefix.to_lowercase();

        // Candidates are matched by name before anything is built for them:
        // this runs on every keystroke, and a wide catalog offers far more
        // names than the menu will ever show.
        let names = collect_candidates(&self.state.read(cx).catalog, &prefix_lower);

        let start_pos = text.offset_to_position(start);
        let end_pos = text.offset_to_position(offset);
        let replace_range = lsp_types::Range {
            start: start_pos,
            end: end_pos,
        };

        let items: Vec<CompletionItem> = names
            .into_iter()
            .map(|candidate| {
                let (kind, detail, group, insert) = match &candidate.source {
                    CandidateSource::Table { schema, database } => (
                        CompletionItemKind::STRUCT,
                        Some(format!("{schema} · {database}")),
                        1,
                        identifier_insert(&candidate.name),
                    ),
                    CandidateSource::Column { data_type, table } => (
                        CompletionItemKind::FIELD,
                        Some(format!("{data_type} · {table}")),
                        2,
                        identifier_insert(&candidate.name),
                    ),
                    CandidateSource::Function => (
                        CompletionItemKind::FUNCTION,
                        None,
                        3,
                        format!("{}(", candidate.name),
                    ),
                    CandidateSource::Keyword => {
                        (CompletionItemKind::KEYWORD, None, 4, candidate.name.clone())
                    }
                };
                CompletionItem {
                    label: candidate.name,
                    kind: Some(kind),
                    detail,
                    sort_text: Some(format!("{}{}", group, candidate.lower)),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: replace_range,
                        new_text: insert,
                    })),
                    ..Default::default()
                }
            })
            .collect();

        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .map(|c| c.is_alphanumeric() || c == '_')
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        collect_candidates, identifier_insert, CandidateSource, MAX_CATALOG_ITEMS, MAX_ITEMS,
    };
    use crate::schema::{ColumnInfo, DatabaseInfo, NodeKind, TableInfo};

    fn catalog(table_count: usize) -> Vec<DatabaseInfo> {
        Vec::from([DatabaseInfo {
            name: "memory".into(),
            tables: (0..table_count)
                .map(|ix| TableInfo {
                    database: "memory".into(),
                    schema: "main".into(),
                    name: format!("orders_{ix}"),
                    kind: NodeKind::Table,
                    estimated_rows: None,
                    comment: None,
                    columns: Vec::from([ColumnInfo {
                        name: format!("amount_{ix}"),
                        data_type: "DOUBLE".into(),
                    }]),
                })
                .collect(),
        }])
    }

    #[test]
    fn candidates_match_prefix_case_insensitively() {
        let names: Vec<String> = collect_candidates(&catalog(2), "ord")
            .into_iter()
            .map(|c| c.name)
            .collect();
        // Catalog names first, then the statics that also match the prefix.
        assert_eq!(names, ["orders_0", "orders_1", "ORDER BY"]);

        // An uppercase prefix reaches the same lowercase catalog names.
        let upper: Vec<String> = collect_candidates(&catalog(1), "ORD")
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(upper, ["orders_0", "ORDER BY"]);
    }

    #[test]
    fn candidates_offer_catalog_before_keywords() {
        let found = collect_candidates(&catalog(1), "s");
        // "orders_0" does not start with "s", so only statics match here.
        assert!(found.iter().all(|c| matches!(
            c.source,
            CandidateSource::Function | CandidateSource::Keyword
        )));
        // Functions rank ahead of keywords.
        let first_keyword = found
            .iter()
            .position(|c| matches!(c.source, CandidateSource::Keyword));
        let last_function = found
            .iter()
            .rposition(|c| matches!(c.source, CandidateSource::Function));
        if let (Some(first_keyword), Some(last_function)) = (first_keyword, last_function) {
            assert!(last_function < first_keyword);
        }
    }

    #[test]
    fn candidates_are_capped_and_deduplicated() {
        // A wide catalog must not be walked into a menu larger than its share
        // of the cap; nothing static starts with "orders".
        let found = collect_candidates(&catalog(5_000), "orders");
        assert_eq!(found.len(), MAX_CATALOG_ITEMS);

        let mut names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "candidate names must be unique");
    }

    #[test]
    fn statics_keep_a_floor_when_the_catalog_fills_up() {
        // The catalog alone could fill the whole menu; functions and keywords
        // that match must still be offered on top of the catalog's share.
        let found = collect_candidates(&catalog(5_000), "ord");
        assert!(found.len() <= MAX_ITEMS);
        let catalog_count = found
            .iter()
            .filter(|c| {
                matches!(
                    c.source,
                    CandidateSource::Table { .. } | CandidateSource::Column { .. }
                )
            })
            .count();
        assert_eq!(catalog_count, MAX_CATALOG_ITEMS);
        assert!(
            found
                .iter()
                .any(|c| matches!(c.source, CandidateSource::Keyword) && c.name == "ORDER BY"),
            "ORDER BY must be offered even when the catalog is full"
        );

        // Same for functions: "sum" collides with nothing in this catalog.
        let found = collect_candidates(&catalog(5_000), "s");
        assert!(found
            .iter()
            .any(|c| matches!(c.source, CandidateSource::Function) && c.name == "sum"),);
    }

    #[test]
    fn plain_identifiers_stay_bare() {
        assert_eq!(identifier_insert("orders"), "orders");
        assert_eq!(identifier_insert("_dim_1"), "_dim_1");
    }

    #[test]
    fn special_identifiers_get_double_quoted() {
        assert_eq!(
            identifier_insert("amount-2026-08-12_2026-09-10"),
            "\"amount-2026-08-12_2026-09-10\""
        );
        assert_eq!(identifier_insert("2026logs"), "\"2026logs\"");
        assert_eq!(identifier_insert("my table"), "\"my table\"");
        assert_eq!(identifier_insert("数码配件"), "\"数码配件\"");
        assert_eq!(identifier_insert("a\"b"), "\"a\"\"b\"");
    }
}
