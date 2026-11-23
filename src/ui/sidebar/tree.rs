//! Projection of the catalog, the registered local files, and the S3 browse
//! tree into the `TreeItem`s the sidebar renders, plus the metadata map each
//! row's actions read back.

use std::collections::HashMap;

use gpui_kit::component::tree::TreeItem;
use gpui_kit::SharedString;

use super::model::{
    ColumnRef, DatabaseRef, FileRef, MetricRef, SchemaNodeKind, SchemaNodeMeta, TableRef,
};
use super::s3::{s3_children_items, S3Browse};
use crate::i18n::tr;
use crate::recents::{RecentDocument, RecentKind};
use crate::schema::{DatabaseInfo, NodeKind, TableInfo};
use crate::semantic::browse::ModelEntry;
use crate::state::{format_rows, AttachedFileView};

pub(super) fn build_tree_items(
    catalog: &[DatabaseInfo],
    attached_files: &[AttachedFileView],
    recents: &[RecentDocument],
    models: &[ModelEntry],
    s3_browse: Option<&S3Browse>,
) -> (Vec<TreeItem>, HashMap<SharedString, SchemaNodeMeta>) {
    let mut meta = HashMap::new();
    let mut items = Vec::new();

    if !attached_files.is_empty() {
        let group_id: SharedString = "group:local-files".into();
        meta.insert(
            group_id.clone(),
            SchemaNodeMeta::new(
                SchemaNodeKind::LocalFilesGroup,
                Some(attached_files.len().to_string().into()),
            ),
        );
        let duplicated = crate::ui::workspace::duplicate_titles(
            attached_files.iter().map(|file| file.view_name.as_str()),
        );
        let file_items: Vec<TreeItem> = attached_files
            .iter()
            .map(|file| {
                let duplicate = duplicated.contains(&file.view_name.as_str());
                file_tree_item(file, duplicate, catalog, &mut meta)
            })
            .collect();
        items.push(
            TreeItem::new(group_id, tr("sidebar.group.local_files"))
                .expanded(true)
                .children(file_items),
        );
    }

    // One group per kind of document, each under its own heading.
    for (id, label, kind) in [
        (
            "group:dashboards",
            tr("sidebar.group.dashboards"),
            RecentKind::Dashboard,
        ),
        (
            "group:notebooks",
            tr("sidebar.group.notebooks"),
            RecentKind::Notebook,
        ),
    ] {
        let of_kind: Vec<RecentDocument> = recents
            .iter()
            .filter(|doc| doc.kind == kind)
            .cloned()
            .collect();
        if let Some(group) = recents_group(id, label, &of_kind, &mut meta) {
            items.push(group);
        }
    }

    if let Some(group) = metrics_group(models, &mut meta) {
        items.push(group);
    }

    if let Some(browse) = s3_browse {
        let s3_id: SharedString = "s3:root".into();
        meta.insert(
            s3_id.clone(),
            SchemaNodeMeta::new(
                SchemaNodeKind::S3Status,
                Some(tr("sidebar.s3.configured").into()),
            ),
        );
        let children = s3_children_items(&s3_id, &browse.children, &mut meta);
        items.push(
            TreeItem::new(s3_id, "S3")
                .expanded(browse.expanded)
                .children(children),
        );
    }

    for (ix, db) in catalog.iter().enumerate() {
        // The catalog lists the open database first. Empty, it is the
        // first-run state, which the sidebar shows as such rather than as an
        // empty `memory` row; an attached database shows even when empty.
        let is_main = ix == 0;
        if is_main && db.tables.is_empty() {
            continue;
        }
        let db_id: SharedString = format!("db:{}", db.name).into();
        let mut db_meta = SchemaNodeMeta::new(
            SchemaNodeKind::Database,
            Some(db.tables.len().to_string().into()),
        );
        db_meta.database = Some(DatabaseRef {
            name: db.name.clone(),
            is_main,
        });
        db_meta.hint = database_hint(db);
        db_meta.tooltip = db
            .path
            .as_deref()
            .map(|path| crate::db::compact_home(path).into());
        meta.insert(db_id.clone(), db_meta);

        let mut schemas: Vec<(String, Vec<&TableInfo>)> = Vec::new();
        for table in &db.tables {
            match schemas.iter_mut().find(|(name, _)| *name == table.schema) {
                Some((_, tables)) => tables.push(table),
                None => schemas.push((table.schema.clone(), vec![table])),
            }
        }

        let schema_items: Vec<TreeItem> = schemas
            .into_iter()
            .map(|(schema, tables)| {
                let schema_id: SharedString = format!("schema:{}.{schema}", db.name).into();
                meta.insert(
                    schema_id.clone(),
                    SchemaNodeMeta::new(
                        SchemaNodeKind::Schema,
                        Some(tables.len().to_string().into()),
                    ),
                );
                let table_items: Vec<TreeItem> = tables
                    .iter()
                    .map(|table| table_tree_item(table, &mut meta))
                    .collect();
                TreeItem::new(schema_id, schema)
                    .expanded(true)
                    .children(table_items)
            })
            .collect();

        items.push(
            TreeItem::new(db_id, db.name.clone())
                .expanded(true)
                .children(schema_items),
        );
    }

    (items, meta)
}

/// A recent documents group (dashboards, or notebooks): a row per remembered document,
/// in a stable order, like files the user can click back open.
fn recents_group(
    id: &str,
    label: &str,
    recents: &[RecentDocument],
    meta: &mut HashMap<SharedString, SchemaNodeMeta>,
) -> Option<TreeItem> {
    let mut documents: Vec<&RecentDocument> = recents.iter().collect();
    // By name, then path — not by recency. Opening a document bumps it to the
    // front of the stored list, and a list ordered that way reshuffles under
    // the pointer the moment one of its rows is clicked. Recency still decides
    // which documents the list keeps.
    sort_documents(&mut documents);
    if documents.is_empty() {
        return None;
    }
    let group_id: SharedString = id.into();
    meta.insert(
        group_id.clone(),
        SchemaNodeMeta::new(
            SchemaNodeKind::DashboardsGroup,
            Some(documents.len().to_string().into()),
        ),
    );
    let duplicated =
        crate::ui::workspace::duplicate_titles(documents.iter().map(|doc| doc.title.as_str()));
    let rows: Vec<TreeItem> = documents
        .iter()
        .map(|doc| {
            let row_id: SharedString = format!("{id}:{}", doc.path).into();
            // Two `dashboard.dash` files both title `dashboard`; the folder
            // each came from is what tells them apart, as on the tabs.
            let hint = duplicated
                .contains(&doc.title.as_str())
                .then(|| crate::ui::workspace::parent_name(std::path::Path::new(&doc.path)))
                .flatten()
                .map(SharedString::from);
            meta.insert(
                row_id.clone(),
                SchemaNodeMeta {
                    doc: Some((*doc).clone()),
                    hint,
                    tooltip: Some(crate::db::compact_home(&doc.path).into()),
                    ..SchemaNodeMeta::new(SchemaNodeKind::RecentDocument, None)
                },
            );
            TreeItem::new(row_id, doc.title.clone())
        })
        .collect();
    Some(TreeItem::new(group_id, label).expanded(true).children(rows))
}

/// The semantic models found beside the dashboards: a row per model, and
/// under it a row per metric, which runs the metric on click.
fn metrics_group(
    models: &[ModelEntry],
    meta: &mut HashMap<SharedString, SchemaNodeMeta>,
) -> Option<TreeItem> {
    if models.is_empty() {
        return None;
    }
    let group_id: SharedString = "group:metrics".into();
    let count: usize = models.iter().map(|model| model.metrics.len()).sum();
    meta.insert(
        group_id.clone(),
        SchemaNodeMeta::new(SchemaNodeKind::MetricsGroup, Some(count.to_string().into())),
    );
    let model_items = models
        .iter()
        .map(|model| {
            let path = model.path.to_string_lossy();
            let model_id: SharedString = format!("model:{path}").into();
            let mut tooltip = crate::db::compact_home(&path);
            if let Some(views) = &model.views {
                tooltip.push_str(&format!(
                    "\n{}",
                    crate::i18n::trf(
                        "sidebar.metrics.views",
                        &[&crate::db::compact_home(&views.to_string_lossy())]
                    )
                ));
            }
            if let Some(problem) = &model.problem {
                tooltip.push_str(&format!("\n{problem}"));
            }
            meta.insert(
                model_id.clone(),
                SchemaNodeMeta {
                    hint: model
                        .problem
                        .as_ref()
                        .map(|_| tr("sidebar.metrics.unreadable").into()),
                    tooltip: Some(tooltip.into()),
                    semantic_model: model
                        .problem
                        .is_none()
                        .then(|| super::metrics::ModelSource {
                            model: model.path.clone(),
                            views: model.views.clone(),
                        }),
                    ..SchemaNodeMeta::new(
                        SchemaNodeKind::SemanticModel,
                        Some(model.metrics.len().to_string().into()),
                    )
                },
            );
            let metric_items: Vec<TreeItem> = model
                .metrics
                .iter()
                .map(|metric| {
                    let row_id: SharedString = format!("metric:{path}:{}", metric.name).into();
                    let mut tooltip: Vec<String> = Vec::new();
                    if let Some(description) = &metric.description {
                        tooltip.push(description.trim().to_string());
                    }
                    if !metric.synonyms.is_empty() {
                        tooltip.push(crate::i18n::trf(
                            "sidebar.metrics.synonyms",
                            &[&metric.synonyms.join(", ")],
                        ));
                    }
                    if let Some(problem) = &metric.problem {
                        tooltip.push(problem.clone());
                    }
                    meta.insert(
                        row_id.clone(),
                        SchemaNodeMeta {
                            metric: metric.problem.is_none().then(|| MetricRef {
                                model: model.path.clone(),
                                views: model.views.clone(),
                                name: metric.name.clone(),
                                time_field: metric.time_field.clone(),
                            }),
                            hint: match (&metric.problem, &metric.label) {
                                (Some(_), _) => Some(tr("sidebar.metrics.not_queryable").into()),
                                (None, Some(label)) if *label != metric.name => {
                                    Some(label.clone().into())
                                }
                                _ => None,
                            },
                            tooltip: (!tooltip.is_empty()).then(|| tooltip.join("\n").into()),
                            ..SchemaNodeMeta::new(SchemaNodeKind::Metric, None)
                        },
                    );
                    TreeItem::new(row_id, metric.name.clone())
                })
                .collect();
            TreeItem::new(model_id, model.name.clone())
                .expanded(true)
                .children(metric_items)
        })
        .collect::<Vec<_>>();
    Some(
        TreeItem::new(group_id, tr("sidebar.group.metrics"))
            .expanded(true)
            .children(model_items),
    )
}

/// One registered data file: labelled by its view name — what a query names —
/// with the file's own name beside it only when that says something the view
/// name does not, and its folder when another file has the same view name.
/// Children are the view's columns looked up from the catalog.
fn file_tree_item(
    file: &AttachedFileView,
    duplicate: bool,
    catalog: &[DatabaseInfo],
    meta: &mut HashMap<SharedString, SchemaNodeMeta>,
) -> TreeItem {
    let file_id: SharedString = format!("file:{}", file.id).into();
    // An unknown count shows nothing: a dash in the count column reads as a
    // value, and says less than leaving the space empty.
    let detail: Option<SharedString> = file.row_count.map(|n| format_rows(n).into());
    let table = catalog
        .iter()
        .flat_map(|db| db.tables.iter())
        .find(|table| table.name == file.view_name);
    let table_ref = table.map(|table| TableRef {
        database: table.database.clone(),
        schema: table.schema.clone(),
        name: table.name.clone(),
        is_view: true,
    });
    meta.insert(
        file_id.clone(),
        SchemaNodeMeta {
            kind: SchemaNodeKind::File,
            detail,
            file: Some(FileRef {
                id: file.id,
                view_name: file.view_name.clone(),
            }),
            database: None,
            table: table_ref.clone(),
            column: None,
            s3_uri: None,
            doc: None,
            semantic_model: None,
            metric: None,
            hint: file_hint(&file.path, &file.view_name, duplicate),
            tooltip: Some(crate::db::compact_home(&file.path).into()),
        },
    );

    let columns: Vec<TreeItem> = table
        .map(|table| {
            table
                .columns
                .iter()
                .map(|column| {
                    let column_id: SharedString =
                        format!("file-col:{}:{}", file.id, column.name).into();
                    meta.insert(
                        column_id.clone(),
                        SchemaNodeMeta {
                            column: table_ref.clone().map(|table| ColumnRef {
                                table,
                                name: column.name.clone(),
                                data_type: column.data_type.clone(),
                            }),
                            ..SchemaNodeMeta::new(
                                SchemaNodeKind::Column,
                                Some(column.data_type.clone().into()),
                            )
                        },
                    );
                    TreeItem::new(column_id, column.name.clone())
                })
                .collect()
        })
        .unwrap_or_default();

    TreeItem::new(file_id, file.view_name.clone()).children(columns)
}

fn table_tree_item(
    table: &TableInfo,
    meta: &mut HashMap<SharedString, SchemaNodeMeta>,
) -> TreeItem {
    let table_id: SharedString =
        format!("table:{}.{}.{}", table.database, table.schema, table.name).into();
    let is_view = table.kind == NodeKind::View;
    let table_ref = TableRef {
        database: table.database.clone(),
        schema: table.schema.clone(),
        name: table.name.clone(),
        is_view,
    };
    let detail: Option<SharedString> = if is_view {
        None
    } else {
        table.estimated_rows.map(|n| format_rows(n).into())
    };
    meta.insert(
        table_id.clone(),
        SchemaNodeMeta {
            table: Some(table_ref.clone()),
            ..SchemaNodeMeta::new(
                if is_view {
                    SchemaNodeKind::View
                } else {
                    SchemaNodeKind::Table
                },
                detail,
            )
        },
    );

    let columns: Vec<TreeItem> = table
        .columns
        .iter()
        .map(|column| {
            let column_id: SharedString = format!(
                "col:{}.{}.{}.{}",
                table.database, table.schema, table.name, column.name
            )
            .into();
            meta.insert(
                column_id.clone(),
                SchemaNodeMeta {
                    column: Some(ColumnRef {
                        table: table_ref.clone(),
                        name: column.name.clone(),
                        data_type: column.data_type.clone(),
                    }),
                    ..SchemaNodeMeta::new(
                        SchemaNodeKind::Column,
                        Some(column.data_type.clone().into()),
                    )
                },
            );
            TreeItem::new(column_id, column.name.clone())
        })
        .collect();

    TreeItem::new(table_id, table.name.clone()).children(columns)
}

/// `items` cut down to the rows whose label contains `query`, ignoring case,
/// and the rows above them. A row that matches keeps all its rows below (a
/// table its columns, to tick from); one kept only for a match further down
/// opens to show it. An empty query keeps everything as it was.
pub(super) fn filter_tree(items: Vec<TreeItem>, query: &str) -> Vec<TreeItem> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return items;
    }
    fn keep(item: TreeItem, query: &str) -> Option<TreeItem> {
        if item.label.to_lowercase().contains(query) {
            return Some(item);
        }
        let children: Vec<TreeItem> = item
            .children
            .into_iter()
            .filter_map(|child| keep(child, query))
            .collect();
        (!children.is_empty()).then(|| {
            TreeItem::new(item.id, item.label)
                .expanded(true)
                .children(children)
        })
    }
    items
        .into_iter()
        .filter_map(|item| keep(item, &query))
        .collect()
}

/// The sidebar's order for recent documents: by title, case-insensitively,
/// then by path, so same-titled documents keep their places too.
fn sort_documents(documents: &mut [&RecentDocument]) {
    documents.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
}

/// The muted text after a file row's view name: the file name when its stem is
/// not the view name (`sales_2` from `sales.csv`, a sheet's table from its
/// workbook), and the folder when another file shares the view name.
/// What sets a database row apart: read-only, and an engine other than
/// DuckDB behind it (`sqlite`).
fn database_hint(db: &DatabaseInfo) -> Option<SharedString> {
    let mut parts = Vec::new();
    if !db.kind.is_empty() && !db.kind.eq_ignore_ascii_case("duckdb") {
        parts.push(db.kind.to_lowercase());
    }
    if db.read_only {
        parts.push(tr("sidebar.database.read_only").to_string());
    }
    (!parts.is_empty()).then(|| parts.join(crate::i18n::sep()).into())
}

fn file_hint(path: &str, view_name: &str, duplicate: bool) -> Option<SharedString> {
    let path = std::path::Path::new(path);
    let mut parts = Vec::new();
    let stem = path.file_stem().map(|stem| stem.to_string_lossy());
    if stem.as_deref() != Some(view_name) {
        if let Some(name) = path.file_name() {
            parts.push(name.to_string_lossy().to_string());
        }
    }
    if duplicate {
        parts.extend(crate::ui::workspace::parent_name(path));
    }
    (!parts.is_empty()).then(|| parts.join(crate::i18n::sep()).into())
}

#[cfg(test)]
mod file_hint_tests {
    use super::{file_hint, filter_tree, sort_documents};
    use crate::recents::{RecentDocument, RecentKind};
    use gpui_kit::component::tree::TreeItem;

    #[test]
    fn recent_documents_keep_their_places_whichever_was_opened_last() {
        let doc = |title: &str, path: &str| RecentDocument {
            kind: RecentKind::Dashboard,
            path: path.to_string(),
            title: title.to_string(),
        };
        let usage = doc("dashboard", "/ex/usage_dashboard/dashboard.dash");
        let orders = doc("dashboard", "/ex/orders_dashboard/dashboard.dash");
        let sales = doc("Sales", "/ex/sales.dash");
        // Most recent first, as stored, in either order: the sidebar agrees.
        for stored in [vec![&usage, &orders, &sales], vec![&sales, &orders, &usage]] {
            let mut shown = stored;
            sort_documents(&mut shown);
            let paths: Vec<&str> = shown.iter().map(|doc| doc.path.as_str()).collect();
            assert_eq!(
                paths,
                [
                    "/ex/orders_dashboard/dashboard.dash",
                    "/ex/usage_dashboard/dashboard.dash",
                    "/ex/sales.dash"
                ]
            );
        }
    }

    #[test]
    fn dashboards_and_notebooks_each_get_their_own_group() {
        let doc = |kind, path: &str| RecentDocument {
            kind,
            path: path.to_string(),
            title: path.to_string(),
        };
        let recents = [
            doc(RecentKind::Notebook, "/ex/review.sql"),
            doc(RecentKind::Dashboard, "/ex/sales.dash"),
        ];
        let (items, meta) = super::build_tree_items(&[], &[], &recents, &[], None);
        let groups: Vec<(String, Vec<String>)> = items
            .iter()
            .map(|group| {
                let rows = group
                    .children
                    .iter()
                    .map(|row| row.id.to_string())
                    .collect();
                (group.id.to_string(), rows)
            })
            .collect();
        assert_eq!(
            groups,
            [
                (
                    "group:dashboards".to_string(),
                    vec!["group:dashboards:/ex/sales.dash".to_string()]
                ),
                (
                    "group:notebooks".to_string(),
                    vec!["group:notebooks:/ex/review.sql".to_string()]
                ),
            ]
        );
        let row = &meta[&gpui_kit::SharedString::from("group:notebooks:/ex/review.sql")];
        assert_eq!(row.doc.as_ref().unwrap().kind, RecentKind::Notebook);
    }

    #[test]
    fn a_file_named_like_its_view_needs_no_hint() {
        assert_eq!(file_hint("/data/sales.csv", "sales", false), None);
    }

    #[test]
    fn a_renamed_view_shows_its_file_and_a_duplicate_its_folder() {
        assert_eq!(
            file_hint("/data/sales.csv", "sales_2", false).as_deref(),
            Some("sales.csv")
        );
        assert_eq!(
            file_hint("/logs/samples/sales.csv", "sales", true).as_deref(),
            Some("samples")
        );
        assert_eq!(
            file_hint("/x/book.xlsx", "book_Extra", true).as_deref(),
            Some(format!("book.xlsx{}x", crate::i18n::sep()).as_str())
        );
    }

    /// Each kept row as `depth:label`, open or not, in tree order.
    fn outline(items: &[TreeItem]) -> Vec<String> {
        fn walk(item: &TreeItem, depth: usize, out: &mut Vec<String>) {
            let open = if item.is_expanded() { "+" } else { "" };
            out.push(format!("{depth}:{}{open}", item.label));
            for child in &item.children {
                walk(child, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        for item in items {
            walk(item, 0, &mut out);
        }
        out
    }

    fn catalog() -> Vec<TreeItem> {
        let table = |name: &str, columns: &[&str]| {
            TreeItem::new(format!("t:{name}"), name.to_string()).children(
                columns
                    .iter()
                    .map(|c| TreeItem::new(format!("c:{name}.{c}"), c.to_string())),
            )
        };
        vec![TreeItem::new("db", "memory").expanded(true).children([
            table("orders", &["order_id", "Amount"]),
            table("users", &["user_id", "email"]),
        ])]
    }

    #[test]
    fn a_column_match_opens_its_table_and_hides_the_rest() {
        assert_eq!(
            outline(&filter_tree(catalog(), "amount")),
            ["0:memory+", "1:orders+", "2:Amount"]
        );
    }

    #[test]
    fn a_table_match_keeps_all_its_columns_closed() {
        assert_eq!(
            outline(&filter_tree(catalog(), "USERS")),
            ["0:memory+", "1:users", "2:user_id", "2:email"]
        );
    }

    #[test]
    fn no_query_keeps_everything_and_no_match_keeps_nothing() {
        assert_eq!(outline(&filter_tree(catalog(), "  ")), outline(&catalog()));
        assert!(filter_tree(catalog(), "zzz").is_empty());
    }
}
