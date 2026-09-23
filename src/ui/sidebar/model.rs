//! Per-node metadata the sidebar keeps beside each `TreeItem`: what a row
//! is, what it can act on, and what to show in its detail column.

use gpui_kit::SharedString;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SchemaNodeKind {
    Database,
    Schema,
    Table,
    View,
    Column,
    LocalFilesGroup,
    File,
    /// Group header for recently opened analysis apps.
    AppsGroup,
    /// Group header for recently opened dashboards.
    DashboardsGroup,
    /// A recently opened document row (app or dashboard).
    RecentDocument,
    S3Status,
    S3Bucket,
    S3Prefix,
    S3File,
    S3Message,
}

/// What the remove button on a registered-file row needs to act on.
#[derive(Clone)]
pub(super) struct FileRef {
    pub(super) id: i64,
    pub(super) view_name: String,
}

/// A catalog table or view a tree node can act on (generate queries, alter).
#[derive(Clone)]
pub(super) struct TableRef {
    pub(super) database: String,
    pub(super) schema: String,
    pub(super) name: String,
    pub(super) is_view: bool,
}

/// What the type-edit button on a column row needs to act on.
#[derive(Clone)]
pub(super) struct ColumnRef {
    pub(super) table: TableRef,
    pub(super) name: String,
    pub(super) data_type: String,
}

#[derive(Clone)]
pub(super) struct SchemaNodeMeta {
    pub(super) kind: SchemaNodeKind,
    pub(super) detail: Option<SharedString>,
    pub(super) file: Option<FileRef>,
    pub(super) table: Option<TableRef>,
    pub(super) column: Option<ColumnRef>,
    /// `s3://bucket/key` on S3 file rows, for generating the SELECT query.
    pub(super) s3_uri: Option<String>,
    /// The document a recent-document row reopens on click.
    pub(super) doc: Option<crate::recents::RecentDocument>,
}

impl SchemaNodeMeta {
    pub(super) fn new(kind: SchemaNodeKind, detail: Option<SharedString>) -> Self {
        Self {
            kind,
            detail,
            file: None,
            table: None,
            column: None,
            s3_uri: None,
            doc: None,
        }
    }
}
