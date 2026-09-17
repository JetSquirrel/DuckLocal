//! Left sidebar: Schema / 查询历史 tabs. The schema page renders the catalog
//! as a tree (数据库 → schema → 表/视图 → 列); the history page lists recent
//! queries and refills the active editor on click.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::dialog::{DialogButtonProps, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::tree::{tree, TreeEvent, TreeItem, TreeState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::history::HistoryEntry;
use crate::i18n::{tr, trf};
use crate::schema::{DatabaseInfo, NodeKind, TableInfo};
use crate::state::{
    format_rows, AppState, AttachedFileView, AttachedFilesChanged, ConnectionChanged,
    HistoryChanged, S3ConfigChanged,
};
use crate::ui::completion::identifier_insert;
use crate::ui::workspace::Workspace;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Schema,
    History,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SchemaNodeKind {
    Database,
    Schema,
    Table,
    View,
    Column,
    LocalFilesGroup,
    File,
    S3Status,
    S3Bucket,
    S3Prefix,
    S3File,
    S3Message,
}

const INDENT_PER_DEPTH: Pixels = px(16.);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum S3NodeKind {
    Bucket,
    Prefix,
    File,
}

/// Lazily loaded children of an S3 tree node (root, bucket, or prefix).
#[derive(Clone)]
enum S3Children {
    NotLoaded,
    Loading,
    Loaded(Vec<S3Node>),
    Failed(String),
}

/// One entry of the S3 browse tree kept by the sidebar.
#[derive(Clone)]
struct S3Node {
    id: SharedString,
    label: String,
    kind: S3NodeKind,
    /// Bucket + prefix used to list this node's children on expand.
    bucket: String,
    prefix: String,
    /// `s3://bucket/key` for files, used to generate the SELECT query.
    s3_uri: Option<String>,
    detail: Option<String>,
    children: S3Children,
    expanded: bool,
}

impl S3Node {
    fn bucket_node(name: String) -> Self {
        Self {
            id: format!("s3:bucket:{name}").into(),
            label: name.clone(),
            kind: S3NodeKind::Bucket,
            bucket: name,
            prefix: String::new(),
            s3_uri: None,
            detail: None,
            children: S3Children::NotLoaded,
            expanded: false,
        }
    }

    fn prefix_node(bucket: &str, prefix: String) -> Self {
        let label = prefix
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&prefix)
            .to_string();
        Self {
            id: format!("s3:prefix:{bucket}/{prefix}").into(),
            label,
            kind: S3NodeKind::Prefix,
            bucket: bucket.to_string(),
            prefix,
            s3_uri: None,
            detail: None,
            children: S3Children::NotLoaded,
            expanded: false,
        }
    }

    fn file_node(bucket: &str, key: String, size: i64) -> Self {
        let label = key.rsplit('/').next().unwrap_or(&key).to_string();
        Self {
            id: format!("s3:file:{bucket}/{key}").into(),
            label,
            kind: S3NodeKind::File,
            bucket: bucket.to_string(),
            prefix: String::new(),
            // Only data files DuckDB can read directly get a click-to-query action.
            s3_uri: crate::db::is_data_file(&key).then(|| format!("s3://{bucket}/{key}")),
            detail: Some(format_size(size)),
            children: S3Children::NotLoaded,
            expanded: false,
        }
    }
}

/// Browse state of the S3 root node, kept across tree rebuilds.
#[derive(Clone)]
struct S3Browse {
    expanded: bool,
    children: S3Children,
}

impl Default for S3Browse {
    fn default() -> Self {
        Self {
            expanded: false,
            children: S3Children::NotLoaded,
        }
    }
}

/// Which node's listing an in-flight S3 request belongs to.
#[derive(Clone)]
enum S3LoadTarget {
    Root,
    Node(SharedString),
}

/// Human-readable object size for the sidebar detail column.
fn format_size(bytes: i64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / (1 << 20) as f64)
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn select_s3_file_sql(uri: &str) -> String {
    format!("SELECT *\nFROM '{}'\nLIMIT 100;", uri.replace('\'', "''"))
}

/// What the remove button on a registered-file row needs to act on.
#[derive(Clone)]
struct FileRef {
    id: i64,
    view_name: String,
}

/// A catalog table or view a tree node can act on (generate queries, alter).
#[derive(Clone)]
struct TableRef {
    database: String,
    schema: String,
    name: String,
    is_view: bool,
}

/// What the type-edit button on a column row needs to act on.
#[derive(Clone)]
struct ColumnRef {
    table: TableRef,
    name: String,
    data_type: String,
}

#[derive(Clone)]
struct SchemaNodeMeta {
    kind: SchemaNodeKind,
    detail: Option<SharedString>,
    file: Option<FileRef>,
    table: Option<TableRef>,
    column: Option<ColumnRef>,
    /// `s3://bucket/key` on S3 file rows, for generating the SELECT query.
    s3_uri: Option<String>,
}

impl SchemaNodeMeta {
    fn new(kind: SchemaNodeKind, detail: Option<SharedString>) -> Self {
        Self {
            kind,
            detail,
            file: None,
            table: None,
            column: None,
            s3_uri: None,
        }
    }
}

/// Fully-qualified, quoted table path for generated SQL.
fn qualified_table_name(table: &TableRef) -> String {
    format!(
        "{}.{}.{}",
        identifier_insert(&table.database),
        identifier_insert(&table.schema),
        identifier_insert(&table.name)
    )
}

fn select_star_sql(table: &TableRef) -> String {
    format!("SELECT *\nFROM {}\nLIMIT 100;", qualified_table_name(table))
}

fn select_column_sql(column: &ColumnRef) -> String {
    format!(
        "SELECT {}\nFROM {}\nLIMIT 100;",
        identifier_insert(&column.name),
        qualified_table_name(&column.table)
    )
}

/// `new_type` stays raw: types with parameters (`DECIMAL(10,2)`) are valid
/// input, and the database owner is the one typing it.
fn alter_column_type_sql(column: &ColumnRef, new_type: &str) -> String {
    format!(
        "ALTER TABLE {} ALTER COLUMN {} SET DATA TYPE {new_type}",
        qualified_table_name(&column.table),
        identifier_insert(&column.name)
    )
}

pub struct Sidebar {
    state: Entity<AppState>,
    workspace: Entity<Workspace>,
    tab: SidebarTab,
    tree_state: Entity<TreeState>,
    node_meta: Rc<HashMap<SharedString, SchemaNodeMeta>>,
    /// S3 browse tree, present while S3 is configured.
    s3_browse: Option<S3Browse>,
    _subscriptions: Vec<Subscription>,
}

impl Sidebar {
    pub fn new(
        state: Entity<AppState>,
        workspace: Entity<Workspace>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let subscriptions = vec![
            cx.subscribe(&state, |this, _, _: &ConnectionChanged, cx| {
                // `set_catalog` also emits ConnectionChanged, so only drop the
                // browse tree when S3 actually became unconfigured.
                if this.state.read(cx).s3_config.is_none() {
                    this.s3_browse = None;
                }
                this.rebuild_tree(cx);
                cx.notify();
            }),
            cx.subscribe(&state, |_, _, _: &HistoryChanged, cx| cx.notify()),
            cx.subscribe(&state, |this, _, _: &AttachedFilesChanged, cx| {
                this.rebuild_tree(cx);
                cx.notify();
            }),
            cx.subscribe(&state, |this, _, _: &S3ConfigChanged, cx| {
                this.s3_browse = this
                    .state
                    .read(cx)
                    .s3_config
                    .as_ref()
                    .map(|_| S3Browse::default());
                this.rebuild_tree(cx);
                cx.notify();
            }),
            cx.subscribe(&tree_state, |this, _, event: &TreeEvent, cx| {
                this.on_tree_event(event, cx);
            }),
        ];
        let s3_browse = state
            .read(cx)
            .s3_config
            .as_ref()
            .map(|_| S3Browse::default());
        Self {
            state,
            workspace,
            tab: SidebarTab::Schema,
            tree_state,
            node_meta: Rc::new(HashMap::new()),
            s3_browse,
            _subscriptions: subscriptions,
        }
    }

    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let (catalog, attached_files) = {
            let state = self.state.read(cx);
            (state.catalog.clone(), state.attached_files.clone())
        };
        let (items, meta) = build_tree_items(&catalog, &attached_files, self.s3_browse.as_ref());
        self.node_meta = Rc::new(meta);
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
    }

    /// Track expansion of S3 nodes and kick off lazy listing on first expand.
    fn on_tree_event(&mut self, event: &TreeEvent, cx: &mut Context<Self>) {
        let (id, expanded) = match event {
            TreeEvent::Expanded(id) => (id, true),
            TreeEvent::Collapsed(id) => (id, false),
        };
        if !id.starts_with("s3:") {
            return;
        }
        let Some(browse) = self.s3_browse.as_mut() else {
            return;
        };
        let load = if id == "s3:root" {
            browse.expanded = expanded;
            if expanded && matches!(browse.children, S3Children::NotLoaded) {
                browse.children = S3Children::Loading;
                Some(S3LoadTarget::Root)
            } else {
                None
            }
        } else if let Some(node) = find_s3_node_mut(&mut browse.children, id) {
            node.expanded = expanded;
            if expanded && matches!(node.children, S3Children::NotLoaded) {
                node.children = S3Children::Loading;
                Some(S3LoadTarget::Node(node.id.clone()))
            } else {
                None
            }
        } else {
            None
        };

        if let Some(target) = load {
            self.rebuild_tree(cx);
            self.start_s3_load(target, cx);
        }
    }

    /// Fire the blocking list call for a node that just entered `Loading`.
    fn start_s3_load(&mut self, target: S3LoadTarget, cx: &mut Context<Self>) {
        let Some(config) = self.state.read(cx).s3_config.clone() else {
            return;
        };
        let request = match &target {
            S3LoadTarget::Root => None,
            S3LoadTarget::Node(id) => self.s3_browse.as_ref().and_then(|browse| {
                find_s3_node(&browse.children, id)
                    .map(|node| (node.bucket.clone(), node.prefix.clone()))
            }),
        };
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || -> anyhow::Result<Vec<S3Node>> {
                match &request {
                    None => Ok(crate::s3::list_buckets(&config)?
                        .into_iter()
                        .map(S3Node::bucket_node)
                        .collect()),
                    Some((bucket, prefix)) => {
                        let listing = crate::s3::list_objects(&config, bucket, prefix)?;
                        let mut nodes: Vec<S3Node> = listing
                            .prefixes
                            .into_iter()
                            .map(|prefix| S3Node::prefix_node(bucket, prefix))
                            .collect();
                        nodes.extend(
                            listing
                                .objects
                                .into_iter()
                                .map(|object| S3Node::file_node(bucket, object.key, object.size)),
                        );
                        Ok(nodes)
                    }
                }
            })
            .await;
            this.update(cx, |this, cx| {
                this.finish_s3_load(target, result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_s3_load(
        &mut self,
        target: S3LoadTarget,
        result: anyhow::Result<Vec<S3Node>>,
        cx: &mut Context<Self>,
    ) {
        let children = match result {
            Ok(nodes) => S3Children::Loaded(nodes),
            Err(e) => S3Children::Failed(trf("sidebar.s3.load_failed", &[&e.to_string()])),
        };
        if let Some(browse) = self.s3_browse.as_mut() {
            match target {
                S3LoadTarget::Root => browse.children = children,
                S3LoadTarget::Node(id) => {
                    if let Some(node) = find_s3_node_mut(&mut browse.children, &id) {
                        node.children = children;
                    }
                }
            }
        }
        self.rebuild_tree(cx);
    }

    /// Reload the bucket list (also the retry path after a failure).
    fn refresh_s3(&mut self, cx: &mut Context<Self>) {
        if let Some(browse) = self.s3_browse.as_mut() {
            browse.expanded = true;
            browse.children = S3Children::Loading;
        }
        self.rebuild_tree(cx);
        self.start_s3_load(S3LoadTarget::Root, cx);
    }

    fn refresh_schema(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        cx.spawn(async move |_, cx| {
            let (catalog, attached) = smol::unblock(|| {
                (
                    crate::schema::load_catalog().unwrap_or_default(),
                    crate::state::load_attached_files(),
                )
            })
            .await;
            state.update(cx, |s, cx| {
                s.set_catalog(catalog, cx);
                s.set_attached_files(attached, cx);
            });
        })
        .detach();
    }

    fn confirm_remove_file(&mut self, file: FileRef, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let on_ok_view = view.clone();
            let on_ok_file = file.clone();
            alert
                .title(trf("dialog.remove_file.title", &[&file.view_name]))
                .description(tr("dialog.remove_file.description"))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(tr("dialog.remove_file.confirm"))
                        .ok_variant(ButtonVariant::Danger)
                        .show_cancel(true)
                        .cancel_text(tr("common.cancel"))
                        .on_ok(move |_, window, cx| {
                            if let Some(view) = on_ok_view.upgrade() {
                                view.update(cx, |this, cx| {
                                    this.remove_file(on_ok_file.clone(), window, cx);
                                });
                            }
                            true
                        }),
                )
        });
    }

    /// Remove from the registry and DROP the view, then refresh both lists.
    fn remove_file(&mut self, file: FileRef, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        cx.spawn_in(window, async move |_, cx| {
            let result = smol::unblock(move || -> anyhow::Result<()> {
                crate::history::remove_attached_file(file.id)?;
                crate::db::with_connection(|conn| {
                    let quoted = file.view_name.replace('"', "\"\"");
                    conn.execute_batch(&format!("DROP VIEW IF EXISTS \"{quoted}\""))?;
                    Ok(())
                })?;
                Ok(())
            })
            .await;
            if result.is_ok() {
                let (catalog, attached) = smol::unblock(|| {
                    (
                        crate::schema::load_catalog().unwrap_or_default(),
                        crate::state::load_attached_files(),
                    )
                })
                .await;
                state.update(cx, |s, cx| {
                    s.set_catalog(catalog, cx);
                    s.set_attached_files(attached, cx);
                });
            }
        })
        .detach();
    }

    fn open_alter_type_dialog(
        &mut self,
        column: ColumnRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input =
            cx.new(|cx| InputState::new(window, cx).default_value(column.data_type.clone()));
        let view = cx.entity().downgrade();

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(trf("dialog.alter_type.title", &[&column.name]))
                .w(px(360.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_xs().child(trf(
                            "dialog.alter_type.current_type",
                            &[&column.table.name, &column.name, &column.data_type],
                        )))
                        .child(Input::new(&input)),
                )
                .footer(
                    DialogFooter::new()
                        .gap_2()
                        .child(
                            Button::new("cancel")
                                .outline()
                                .label(tr("common.cancel"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("confirm-alter-type")
                                .primary()
                                .label(tr("dialog.alter_type.confirm"))
                                .on_click({
                                    let input = input.clone();
                                    let view = view.clone();
                                    let column = column.clone();
                                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                        let new_type = input.read(cx).value().trim().to_string();
                                        window.close_dialog(cx);
                                        if new_type.is_empty() {
                                            return;
                                        }
                                        if let Some(view) = view.upgrade() {
                                            view.update(cx, |this, cx| {
                                                this.alter_column_type(
                                                    column.clone(),
                                                    new_type,
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }
                                    }
                                }),
                        ),
                )
        });
    }

    /// Run `ALTER TABLE ... SET DATA TYPE`, then reload the catalog so the
    /// sidebar shows the new type.
    fn alter_column_type(
        &mut self,
        column: ColumnRef,
        new_type: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let sql = alter_column_type_sql(&column, &new_type);
            let result = smol::unblock(move || -> anyhow::Result<()> {
                crate::db::with_connection(|conn| {
                    conn.execute_batch(&sql)?;
                    Ok(())
                })
            })
            .await;
            let outcome = match result {
                Ok(()) => {
                    let catalog =
                        smol::unblock(|| crate::schema::load_catalog().unwrap_or_default()).await;
                    Ok(catalog)
                }
                Err(e) => Err(e),
            };
            this.update_in(cx, move |_, window, cx| match outcome {
                Ok(catalog) => {
                    state.update(cx, |s, cx| s.set_catalog(catalog, cx));
                    window.push_notification(
                        Notification::success(trf(
                            "notify.alter_type.success",
                            &[&column.name, &new_type],
                        )),
                        cx,
                    );
                }
                Err(e) => {
                    window.push_notification(
                        Notification::error(trf("notify.alter_type.failed", &[&e.to_string()])),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_history_item(
        &self,
        entry: &HistoryEntry,
        is_last: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let first_line = entry.sql.lines().next().unwrap_or("").trim().to_string();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let time = match entry.started_at.split_once(' ') {
            Some((date, clock)) if date == today => clock.to_string(),
            Some((date, clock)) if date.len() >= 10 => {
                format!("{} {}", &date[5..], clock.get(..5).unwrap_or(clock))
            }
            _ => entry.started_at.clone(),
        };
        let duration = crate::state::format_duration(entry.duration_ms);
        let rows = entry
            .row_count
            .map(|n| trf("sidebar.history.rows", &[&format_rows(n)]))
            .unwrap_or_else(|| "—".to_string());
        let ok = entry.ok;
        let sql = entry.sql.clone();
        let id = entry.id;

        let workspace = self.workspace.clone();
        div()
            .id(("history", id as usize))
            .w_full()
            .px_3()
            .py_2()
            .when(!is_last, |this| this.border_b_1())
            .border_color(cx.theme().border)
            .hover(|this| this.bg(cx.theme().accent))
            .child(
                div()
                    .text_sm()
                    .truncate()
                    .when(ok, |this| this.text_color(cx.theme().foreground))
                    .when(!ok, |this| this.text_color(cx.theme().danger))
                    .child(first_line),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(time)
                    .child(duration)
                    .child(rows)
                    .when(!ok, |this| {
                        this.child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(tr("sidebar.history.failed")),
                        )
                    }),
            )
            .on_click(move |_, window, cx| {
                workspace.update(cx, |ws, cx| {
                    ws.fill_active_editor(sql.clone(), window, cx);
                });
            })
    }

    fn render_schema(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let meta = self.node_meta.clone();
        let has_items = {
            let state = self.state.read(cx);
            !state.catalog.is_empty()
                || !state.attached_files.is_empty()
                || state.s3_config.is_some()
        };

        if !has_items {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .text_center()
                        .child(tr("sidebar.schema.empty")),
                )
                .into_any_element();
        }

        let sidebar = cx.entity().downgrade();
        let workspace = self.workspace.clone();
        let s3_endpoint = self
            .state
            .read(cx)
            .s3_config
            .as_ref()
            .map(|config| config.endpoint.clone());
        tree(&self.tree_state, move |ix, entry, selected, _window, cx| {
            let item = entry.item();
            let node_meta = meta.get(&item.id);
            let icon: Option<gpui_kit::assets::IconName> = node_meta.and_then(|m| match m.kind {
                SchemaNodeKind::Database => Some(IconName::HardDrive.into()),
                SchemaNodeKind::Schema | SchemaNodeKind::LocalFilesGroup => {
                    Some(if entry.is_expanded() {
                        IconName::FolderOpen.into()
                    } else {
                        IconName::Folder.into()
                    })
                }
                SchemaNodeKind::Table => Some(IconName::GalleryVerticalEnd.into()),
                SchemaNodeKind::View => Some(IconName::Eye.into()),
                SchemaNodeKind::File | SchemaNodeKind::S3File => Some(IconName::File.into()),
                SchemaNodeKind::S3Status => Some(IconName::Globe.into()),
                SchemaNodeKind::S3Bucket => Some(gpui_kit::assets::IconName::Inbox),
                SchemaNodeKind::S3Prefix => Some(if entry.is_expanded() {
                    IconName::FolderOpen.into()
                } else {
                    IconName::Folder.into()
                }),
                SchemaNodeKind::Column | SchemaNodeKind::S3Message => None,
            });

            let group_name = item.id.clone();
            let s3_tooltip = match node_meta.map(|m| m.kind) {
                Some(SchemaNodeKind::S3Status) => s3_endpoint.clone(),
                _ => None,
            };
            let file = node_meta.and_then(|m| m.file.clone());
            let table = node_meta.and_then(|m| m.table.clone());
            let column = node_meta.and_then(|m| m.column.clone());
            let s3_uri = node_meta.and_then(|m| m.s3_uri.clone());
            let is_s3_root = node_meta.map(|m| m.kind) == Some(SchemaNodeKind::S3Status);
            let editable_column = column.clone().filter(|column| !column.table.is_view);
            ListItem::new(ix)
                .selected(selected)
                .pl(INDENT_PER_DEPTH * entry.depth() + px(12.))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .group(group_name.clone())
                        .child(match icon {
                            Some(icon) => Icon::new(icon)
                                .xsmall()
                                .text_color(cx.theme().muted_foreground)
                                .into_any_element(),
                            None => div().w(px(12.)).flex_shrink_0().into_any_element(),
                        })
                        .child(
                            div()
                                .id(("row-label", ix))
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .child(item.label.clone())
                                .when_some(s3_tooltip, |this, endpoint| {
                                    this.tooltip(move |window, cx| {
                                        Tooltip::new(endpoint.clone()).build(window, cx)
                                    })
                                })
                                .when_some(column, |this, column| {
                                    let workspace = workspace.clone();
                                    this.cursor_pointer().on_click(move |_, window, cx| {
                                        workspace.update(cx, |ws, cx| {
                                            ws.fill_active_editor(
                                                select_column_sql(&column),
                                                window,
                                                cx,
                                            );
                                        });
                                    })
                                })
                                .when_some(s3_uri, |this, uri| {
                                    let workspace = workspace.clone();
                                    this.cursor_pointer().on_click(move |_, window, cx| {
                                        workspace.update(cx, |ws, cx| {
                                            ws.fill_active_editor(
                                                select_s3_file_sql(&uri),
                                                window,
                                                cx,
                                            );
                                        });
                                    })
                                }),
                        )
                        .when_some(node_meta.and_then(|m| m.detail.clone()), |this, detail| {
                            this.child(
                                div()
                                    .ml_auto()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(detail),
                            )
                        })
                        .child(
                            h_flex()
                                .id(("row-actions", ix))
                                .w(px(44.))
                                .flex_shrink_0()
                                .justify_end()
                                .gap_1()
                                .opacity(0.)
                                .group_hover(group_name, |style| style.opacity(1.))
                                .on_click(|_, _, cx: &mut App| cx.stop_propagation())
                                .when(is_s3_root, |this| {
                                    let sidebar = sidebar.clone();
                                    this.child(
                                        Button::new(("refresh-s3", ix))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::RotateCw)
                                            .tooltip(tr("sidebar.s3.refresh_buckets"))
                                            .on_click(move |_, _, cx| {
                                                if let Some(sidebar) = sidebar.upgrade() {
                                                    sidebar.update(cx, |this, cx| {
                                                        this.refresh_s3(cx);
                                                    });
                                                }
                                            }),
                                    )
                                })
                                .when_some(table, |this, table| {
                                    let workspace = workspace.clone();
                                    this.child(
                                        Button::new(("generate-query", ix))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::Play)
                                            .tooltip(tr("sidebar.table.generate_select"))
                                            .on_click(move |_, window, cx| {
                                                workspace.update(cx, |ws, cx| {
                                                    ws.fill_active_editor(
                                                        select_star_sql(&table),
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                })
                                .when_some(editable_column, |this, column| {
                                    this.child(
                                        Button::new(("edit-column-type", ix))
                                            .ghost()
                                            .xsmall()
                                            .icon(gpui_kit::assets::IconName::CaseSensitive)
                                            .tooltip(tr("sidebar.column.edit_type"))
                                            .on_click({
                                                let sidebar = sidebar.clone();
                                                move |_, window, cx| {
                                                    if let Some(sidebar) = sidebar.upgrade() {
                                                        sidebar.update(cx, |this, cx| {
                                                            this.open_alter_type_dialog(
                                                                column.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        });
                                                    }
                                                }
                                            }),
                                    )
                                })
                                .when_some(file, |this, file| {
                                    this.child(
                                        Button::new(("remove-file", file.id as usize))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::Close)
                                            .tooltip(tr("sidebar.file.remove"))
                                            .on_click({
                                                let sidebar = sidebar.clone();
                                                move |_, window, cx| {
                                                    if let Some(sidebar) = sidebar.upgrade() {
                                                        sidebar.update(cx, |this, cx| {
                                                            this.confirm_remove_file(
                                                                file.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        });
                                                    }
                                                }
                                            }),
                                    )
                                }),
                        ),
                )
        })
        .into_any_element()
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // An `Rc` handle: the list is read while `cx` is borrowed mutably for
        // rendering, and copying 200 entries every frame is not the way to do it.
        let history: Rc<Vec<HistoryEntry>> = self.state.read(cx).history.clone();

        v_flex()
            .size_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                h_flex().p_2().gap_1().items_center().child(
                    TabBar::new("sidebar-tabs")
                        .segmented()
                        .xsmall()
                        .selected_index(match self.tab {
                            SidebarTab::Schema => 0,
                            SidebarTab::History => 1,
                        })
                        .on_click(cx.listener(|this, ix, _, cx| {
                            this.tab = match ix {
                                0 => SidebarTab::Schema,
                                _ => SidebarTab::History,
                            };
                            cx.notify();
                        }))
                        .child(Tab::new().label(tr("sidebar.tab.schema")))
                        .child(Tab::new().label(tr("sidebar.tab.history"))),
                ),
            )
            .when(self.tab == SidebarTab::Schema, |this| {
                this.child(
                    h_flex().px_2().pb_1().justify_end().child(
                        Button::new("refresh-schema")
                            .ghost()
                            .xsmall()
                            .icon(IconName::RotateCw)
                            .tooltip(tr("sidebar.refresh_schema"))
                            .on_click(cx.listener(Self::refresh_schema)),
                    ),
                )
            })
            .child(div().flex_1().min_h_0().child(match self.tab {
                SidebarTab::Schema => self.render_schema(window, cx),
                SidebarTab::History => {
                    if history.is_empty() {
                        v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .p_4()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .text_center()
                                    .child(tr("sidebar.history.empty")),
                            )
                            .into_any_element()
                    } else {
                        v_flex()
                            .id("history-list")
                            .size_full()
                            .overflow_y_scroll()
                            .children(history.iter().enumerate().map(|(ix, entry)| {
                                self.render_history_item(entry, ix + 1 == history.len(), cx)
                            }))
                            .into_any_element()
                    }
                }
            }))
    }
}

fn find_s3_node<'a>(children: &'a S3Children, id: &str) -> Option<&'a S3Node> {
    fn find_in<'a>(nodes: &'a [S3Node], id: &str) -> Option<&'a S3Node> {
        for node in nodes {
            if node.id.as_ref() == id {
                return Some(node);
            }
            if let Some(found) = find_s3_node(&node.children, id) {
                return Some(found);
            }
        }
        None
    }
    match children {
        S3Children::Loaded(nodes) => find_in(nodes, id),
        _ => None,
    }
}

fn find_s3_node_mut<'a>(children: &'a mut S3Children, id: &str) -> Option<&'a mut S3Node> {
    fn find_in<'a>(nodes: &'a mut [S3Node], id: &str) -> Option<&'a mut S3Node> {
        for node in nodes {
            if node.id.as_ref() == id {
                return Some(node);
            }
            if let S3Children::Loaded(children) = &mut node.children {
                if let Some(found) = find_in(children, id) {
                    return Some(found);
                }
            }
        }
        None
    }
    match children {
        S3Children::Loaded(nodes) => find_in(nodes, id),
        _ => None,
    }
}

fn build_tree_items(
    catalog: &[DatabaseInfo],
    attached_files: &[AttachedFileView],
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

/// Children of an S3 node: loading/error placeholders until the listing
/// arrives. An unloaded node still needs one (disabled) child so the tree
/// treats it as an expandable folder.
fn s3_children_items(
    parent_id: &str,
    children: &S3Children,
    meta: &mut HashMap<SharedString, SchemaNodeMeta>,
) -> Vec<TreeItem> {
    let mut placeholder = |suffix: &str, label: String| {
        let id: SharedString = format!("{parent_id}/{suffix}").into();
        meta.insert(
            id.clone(),
            SchemaNodeMeta::new(SchemaNodeKind::S3Message, None),
        );
        TreeItem::new(id, label).disabled(true)
    };
    match children {
        S3Children::NotLoaded | S3Children::Loading => {
            vec![placeholder("loading", tr("sidebar.s3.loading").to_string())]
        }
        S3Children::Failed(message) => vec![placeholder("error", message.clone())],
        S3Children::Loaded(nodes) => {
            if nodes.is_empty() {
                vec![placeholder("empty", tr("sidebar.s3.empty").to_string())]
            } else {
                nodes.iter().map(|node| s3_node_item(node, meta)).collect()
            }
        }
    }
}

fn s3_node_item(node: &S3Node, meta: &mut HashMap<SharedString, SchemaNodeMeta>) -> TreeItem {
    let kind = match node.kind {
        S3NodeKind::Bucket => SchemaNodeKind::S3Bucket,
        S3NodeKind::Prefix => SchemaNodeKind::S3Prefix,
        S3NodeKind::File => SchemaNodeKind::S3File,
    };
    meta.insert(
        node.id.clone(),
        SchemaNodeMeta {
            s3_uri: node.s3_uri.clone(),
            ..SchemaNodeMeta::new(kind, node.detail.clone().map(Into::into))
        },
    );
    let children = match node.kind {
        S3NodeKind::File => Vec::new(),
        _ => s3_children_items(&node.id, &node.children, meta),
    };
    TreeItem::new(node.id.clone(), node.label.clone())
        .expanded(node.expanded)
        .children(children)
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

#[cfg(test)]
mod tests {
    // No `use super::*;`: the module's `gpui_kit::*` glob would drag gpui's
    // `test` attribute macro into scope and shadow the built-in `#[test]`.
    use super::{
        alter_column_type_sql, find_s3_node, format_size, select_column_sql, select_s3_file_sql,
        select_star_sql, ColumnRef, S3Children, S3Node, S3NodeKind, TableRef,
    };

    fn table_ref(name: &str, is_view: bool) -> TableRef {
        TableRef {
            database: "memory".to_string(),
            schema: "main".to_string(),
            name: name.to_string(),
            is_view,
        }
    }

    #[test]
    fn s3_file_sql_escapes_quotes() {
        assert_eq!(
            select_s3_file_sql("s3://logs/2024/a.parquet"),
            "SELECT *\nFROM 's3://logs/2024/a.parquet'\nLIMIT 100;"
        );
        assert_eq!(
            select_s3_file_sql("s3://logs/it's.csv"),
            "SELECT *\nFROM 's3://logs/it''s.csv'\nLIMIT 100;"
        );
    }

    #[test]
    fn s3_node_labels_and_ids() {
        let bucket = S3Node::bucket_node("logs".to_string());
        assert_eq!(bucket.id.as_ref(), "s3:bucket:logs");
        assert!(matches!(bucket.children, S3Children::NotLoaded));

        let prefix = S3Node::prefix_node("logs", "2024/09/".to_string());
        assert_eq!(prefix.id.as_ref(), "s3:prefix:logs/2024/09/");
        assert_eq!(prefix.label, "09");
        assert_eq!(prefix.prefix, "2024/09/");

        let file = S3Node::file_node("logs", "2024/09/data.parquet".to_string(), 2048);
        assert_eq!(file.id.as_ref(), "s3:file:logs/2024/09/data.parquet");
        assert_eq!(file.label, "data.parquet");
        assert_eq!(file.kind, S3NodeKind::File);
        assert_eq!(
            file.s3_uri.as_deref(),
            Some("s3://logs/2024/09/data.parquet")
        );
        assert_eq!(file.detail.as_deref(), Some("2 KB"));
    }

    #[test]
    fn find_s3_node_walks_loaded_subtrees() {
        let mut child = S3Node::prefix_node("b", "x/".to_string());
        child.children = S3Children::Loaded(vec![S3Node::file_node("b", "x/f.csv".to_string(), 1)]);
        let mut root = S3Node::bucket_node("b".to_string());
        root.children = S3Children::Loaded(vec![child]);
        let tree = S3Children::Loaded(vec![root]);

        assert!(find_s3_node(&tree, "s3:file:b/x/f.csv").is_some());
        assert!(find_s3_node(&tree, "s3:file:b/missing.csv").is_none());
        // Unloaded subtrees are opaque to the search.
        assert!(find_s3_node(&S3Children::NotLoaded, "s3:bucket:b").is_none());
    }

    #[test]
    fn format_size_picks_human_units() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn generated_selects_qualify_and_quote() {
        assert_eq!(
            select_star_sql(&table_ref("orders", false)),
            "SELECT *\nFROM memory.main.orders\nLIMIT 100;"
        );
        // Names that are not plain identifiers get double-quoted.
        assert_eq!(
            select_star_sql(&table_ref("order items", false)),
            "SELECT *\nFROM memory.main.\"order items\"\nLIMIT 100;"
        );

        let column = ColumnRef {
            table: table_ref("orders", false),
            name: "total amount".to_string(),
            data_type: "DOUBLE".to_string(),
        };
        assert_eq!(
            select_column_sql(&column),
            "SELECT \"total amount\"\nFROM memory.main.orders\nLIMIT 100;"
        );
    }

    #[test]
    fn alter_type_sql_quotes_identifiers_and_keeps_raw_type() {
        let column = ColumnRef {
            table: table_ref("orders", false),
            name: "amount".to_string(),
            data_type: "INTEGER".to_string(),
        };
        assert_eq!(
            alter_column_type_sql(&column, "DECIMAL(10,2)"),
            "ALTER TABLE memory.main.orders ALTER COLUMN amount SET DATA TYPE DECIMAL(10,2)"
        );
    }

    #[test]
    fn quotes_escape_embedded_double_quotes() {
        assert_eq!(
            select_star_sql(&table_ref("we\"ird", false)),
            "SELECT *\nFROM memory.main.\"we\"\"ird\"\nLIMIT 100;"
        );
    }

    #[test]
    fn alter_type_sql_is_accepted_by_duckdb() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE orders(id INTEGER, amount INTEGER); INSERT INTO orders VALUES (1, 42);",
        )
        .unwrap();
        let column = ColumnRef {
            table: table_ref("orders", false),
            name: "amount".to_string(),
            data_type: "INTEGER".to_string(),
        };
        conn.execute_batch(&alter_column_type_sql(&column, "DECIMAL(10,2)"))
            .unwrap();
        let value: String = conn
            .query_row("SELECT amount::VARCHAR FROM orders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, "42.00");
    }
}
