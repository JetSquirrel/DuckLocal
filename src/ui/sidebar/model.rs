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
    /// Muted text after the label: what tells a row apart from a same-named
    /// one, or a file name that differs from its view's.
    pub(super) hint: Option<SharedString>,
    /// Shown on hover over the label: a file's or document's full path.
    pub(super) tooltip: Option<SharedString>,
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
            hint: None,
            tooltip: None,
        }
    }
}

impl SchemaNodeKind {
    /// The tree's top-level groupings, which are headings rather than things:
    /// drawn as a small muted title with a disclosure arrow, so they never
    /// read as a folder the way a schema does.
    pub(super) fn is_section(self) -> bool {
        matches!(
            self,
            SchemaNodeKind::LocalFilesGroup
                | SchemaNodeKind::AppsGroup
                | SchemaNodeKind::DashboardsGroup
                | SchemaNodeKind::S3Status
        )
    }
}
