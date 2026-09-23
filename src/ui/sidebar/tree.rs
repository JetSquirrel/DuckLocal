//! Projection of the catalog, the registered local files, and the S3 browse
//! tree into the `TreeItem`s the sidebar renders, plus the metadata map each
//! row's actions read back.

use std::collections::HashMap;

use gpui_kit::component::tree::TreeItem;
use gpui_kit::SharedString;

use super::model::{ColumnRef, FileRef, SchemaNodeKind, SchemaNodeMeta, TableRef};
use super::s3::{s3_children_items, S3Browse};
use crate::i18n::tr;
use crate::recents::{RecentDocument, RecentKind};
use crate::schema::{DatabaseInfo, NodeKind, TableInfo};
use crate::state::{format_rows, AttachedFileView};

pub(super) fn build_tree_items(
    catalog: &[DatabaseInfo],
    attached_files: &[AttachedFileView],
    recents: &[RecentDocument],
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
        let file_items: Vec<TreeItem> = attached_files
            .iter()
            .map(|file| file_tree_item(file, catalog, &mut meta))
            .collect();
        items.push(
            TreeItem::new(group_id, tr("sidebar.group.local_files"))
                .expanded(true)
                .children(file_items),
        );
    }

    if let Some(group) = recents_group("group:apps", tr("sidebar.group.apps"), recents, RecentKind::App, &mut meta) {
        items.push(group);
    }
    if let Some(group) = recents_group(
        "group:dashboards",
        tr("sidebar.group.dashboards"),
        recents,
        RecentKind::Dashboard,
        &mut meta,
    ) {
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

    for db in catalog {
        let db_id: SharedString = format!("db:{}", db.name).into();
        meta.insert(
            db_id.clone(),
            SchemaNodeMeta::new(
                SchemaNodeKind::Database,
                Some(db.tables.len().to_string().into()),
            ),
        );

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

/// One recents group ("Apps" or "Dashboards"): a row per remembered document,
/// most recent first, like files the user can click back open.
fn recents_group(
    id: &str,
    label: &str,
    recents: &[RecentDocument],
    kind: RecentKind,
    meta: &mut HashMap<SharedString, SchemaNodeMeta>,
) -> Option<TreeItem> {
    let documents: Vec<&RecentDocument> = recents.iter().filter(|doc| doc.kind == kind).collect();
    if documents.is_empty() {
        return None;
    }
    let group_id: SharedString = id.into();
    meta.insert(
        group_id.clone(),
        SchemaNodeMeta::new(
            match kind {
                RecentKind::App => SchemaNodeKind::AppsGroup,
                RecentKind::Dashboard => SchemaNodeKind::DashboardsGroup,
            },
            Some(documents.len().to_string().into()),
        ),
    );
    let rows: Vec<TreeItem> = documents
        .into_iter()
        .map(|doc| {
            let row_id: SharedString = format!("{id}:{}", doc.path).into();
            meta.insert(
                row_id.clone(),
                SchemaNodeMeta {
                    doc: Some(doc.clone()),
                    ..SchemaNodeMeta::new(SchemaNodeKind::RecentDocument, None)
                },
            );
            TreeItem::new(row_id, doc.title.clone())
        })
        .collect();
    Some(
        TreeItem::new(group_id, label)
            .expanded(true)
            .children(rows),
    )
}

/// One registered data file: label is `视图名 · 文件名`, children are the
/// view's columns looked up from the catalog.
fn file_tree_item(
    file: &AttachedFileView,
    catalog: &[DatabaseInfo],
    meta: &mut HashMap<SharedString, SchemaNodeMeta>,
) -> TreeItem {
    let file_id: SharedString = format!("file:{}", file.id).into();
    let detail: SharedString = file
        .row_count
        .map(|n| format_rows(n).into())
        .unwrap_or_else(|| "—".into());
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
            detail: Some(detail),
            file: Some(FileRef {
                id: file.id,
                view_name: file.view_name.clone(),
            }),
            table: table_ref.clone(),
            column: None,
            s3_uri: None,
            doc: None,
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

    let basename = std::path::Path::new(&file.path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| file.path.clone());
    TreeItem::new(file_id, format!("{} · {}", file.view_name, basename)).children(columns)
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
