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
use gpui_kit::component::tree::{TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::history::HistoryEntry;
use crate::schema::{DatabaseInfo, NodeKind, TableInfo};
use crate::state::{
    AppState, AttachedFileView, AttachedFilesChanged, ConnectionChanged, HistoryChanged,
    S3ConfigChanged, format_rows,
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
}

/// What the remove button on a registered-file row needs to act on.
#[derive(Clone)]
struct FileRef {
    id: i64,
    view_name: String,
    kind: String,
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
}

impl SchemaNodeMeta {
    fn new(kind: SchemaNodeKind, detail: Option<SharedString>) -> Self {
        Self {
            kind,
            detail,
            file: None,
            table: None,
            column: None,
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
                this.rebuild_tree(cx);
                cx.notify();
            }),
            cx.subscribe(&state, |_, _, _: &HistoryChanged, cx| cx.notify()),
            cx.subscribe(&state, |this, _, _: &AttachedFilesChanged, cx| {
                this.rebuild_tree(cx);
                cx.notify();
            }),
            cx.subscribe(&state, |this, _, _: &S3ConfigChanged, cx| {
                this.rebuild_tree(cx);
                cx.notify();
            }),
        ];
        Self {
            state,
            workspace,
            tab: SidebarTab::Schema,
            tree_state,
            node_meta: Rc::new(HashMap::new()),
            _subscriptions: subscriptions,
        }
    }

    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let (catalog, attached_files, s3_endpoint) = {
            let state = self.state.read(cx);
            (
                state.catalog.clone(),
                state.attached_files.clone(),
                state.s3_endpoint.clone(),
            )
        };
        let (items, meta) = build_tree_items(&catalog, &attached_files, s3_endpoint.as_deref());
        self.node_meta = Rc::new(meta);
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
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
                .title(format!("移除“{}”？", file.view_name))
                .description("将从本地文件注册表移除，并删除当前数据库中的视图；原始文件不受影响。")
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("移除")
                        .ok_variant(ButtonVariant::Danger)
                        .show_cancel(true)
                        .cancel_text("取消")
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
        let input = cx.new(|cx| InputState::new(window, cx).default_value(column.data_type.clone()));
        let view = cx.entity().downgrade();

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(format!("修改 {} 的数据类型", column.name))
                .w(px(360.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .child(format!(
                                    "{}.{} · 当前类型 {}",
                                    column.table.name, column.name, column.data_type
                                )),
                        )
                        .child(Input::new(&input)),
                )
                .footer(
                    DialogFooter::new()
                        .gap_2()
                        .child(
                            Button::new("cancel")
                                .outline()
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("confirm-alter-type")
                                .primary()
                                .label("修改")
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
                        Notification::success(format!(
                            "已将 {} 的类型修改为 {new_type}",
                            column.name
                        )),
                        cx,
                    );
                }
                Err(e) => {
                    window.push_notification(
                        Notification::error(format!("修改数据类型失败：{e}")),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_history_item(&self, entry: &HistoryEntry, cx: &mut Context<Self>) -> impl IntoElement {
        let first_line = entry.sql.lines().next().unwrap_or("").trim().to_string();
        let time = entry
            .started_at
            .split_whitespace()
            .last()
            .unwrap_or("")
            .to_string();
        let duration = crate::state::format_duration(entry.duration_ms);
        let rows = entry
            .row_count
            .map(|n| format!("{} 行", format_rows(n)))
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
            .border_b_1()
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
                                .child("失败"),
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
                || state.s3_endpoint.is_some()
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
                        .child("当前数据库没有表或视图。\n通过“打开数据库…”导入数据文件，或运行 CREATE TABLE 后点击刷新。"),
                )
                .into_any_element();
        }

        let sidebar = cx.entity().downgrade();
        let workspace = self.workspace.clone();
        tree(&self.tree_state, move |ix, entry, selected, _window, cx| {
            let item = entry.item();
            let node_meta = meta.get(&item.id);
            let icon = node_meta.and_then(|m| match m.kind {
                SchemaNodeKind::Database => Some(IconName::HardDrive),
                SchemaNodeKind::Schema | SchemaNodeKind::LocalFilesGroup => {
                    Some(if entry.is_expanded() {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    })
                }
                SchemaNodeKind::Table => Some(IconName::GalleryVerticalEnd),
                SchemaNodeKind::View => Some(IconName::Eye),
                SchemaNodeKind::File => Some(match m.file.as_ref().map(|f| f.kind.as_str()) {
                    Some("parquet") => IconName::File,
                    _ => IconName::FileText,
                }),
                SchemaNodeKind::S3Status => Some(IconName::Globe),
                SchemaNodeKind::Column => None,
            });

            let group_name = item.id.clone();
            let file = node_meta.and_then(|m| m.file.clone());
            let table = node_meta.and_then(|m| m.table.clone());
            let column = node_meta.and_then(|m| m.column.clone());
            let editable_column = column
                .clone()
                .filter(|column| !column.table.is_view);
            ListItem::new(ix)
                .selected(selected)
                .pl(px(16.) * entry.depth() + px(8.))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .group(group_name.clone())
                        .when_some(icon, |this, icon| {
                            this.child(
                                Icon::new(icon)
                                    .xsmall()
                                    .text_color(cx.theme().muted_foreground),
                            )
                        })
                        .child(
                            div()
                                .id(("row-label", ix))
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .child(item.label.clone())
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
                                }),
                        )
                        .when_some(node_meta.and_then(|m| m.detail.clone()), |this, detail| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(detail),
                            )
                        })
                        .child(
                            h_flex()
                                .id(("row-actions", ix))
                                .gap_1()
                                .opacity(0.)
                                .group_hover(group_name, |style| style.opacity(1.))
                                .on_click(|_, _, cx: &mut App| cx.stop_propagation())
                                .when_some(table, |this, table| {
                                    let workspace = workspace.clone();
                                    this.child(
                                        Button::new(("generate-query", ix))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::Play)
                                            .tooltip("生成 SELECT 查询")
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
                                            .icon(gpui_kit::assets::IconName::Pencil)
                                            .tooltip("修改数据类型")
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
                                            .tooltip("从本地文件移除")
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
                        .child(Tab::new().label("Schema"))
                        .child(Tab::new().label("查询历史")),
                ),
            )
            .child(
                h_flex()
                    .px_2()
                    .pb_1()
                    .justify_end()
                    .when(self.tab == SidebarTab::Schema, |this| {
                        this.child(
                            Button::new("refresh-schema")
                                .ghost()
                                .xsmall()
                                .icon(IconName::RotateCw)
                                .tooltip("刷新 Schema")
                                .on_click(cx.listener(Self::refresh_schema)),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(match self.tab {
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
                                            .child("还没有查询记录。\n运行一条查询后会显示在这里。"),
                                    )
                                    .into_any_element()
                            } else {
                                v_flex()
                                    .id("history-list")
                                    .size_full()
                                    .overflow_y_scroll()
                                    .children(
                                        history
                                            .iter()
                                            .map(|entry| self.render_history_item(entry, cx)),
                                    )
                                    .into_any_element()
                            }
                        }
                    }),
            )
    }
}

fn build_tree_items(
    catalog: &[DatabaseInfo],
    attached_files: &[AttachedFileView],
    s3_endpoint: Option<&str>,
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
            TreeItem::new(group_id, "本地文件")
                .expanded(true)
                .children(file_items),
        );
    }

    if let Some(endpoint) = s3_endpoint {
        let s3_id: SharedString = "s3:status".into();
        meta.insert(
            s3_id.clone(),
            SchemaNodeMeta::new(
                SchemaNodeKind::S3Status,
                Some(format!("已配置 · {endpoint}").into()),
            ),
        );
        items.push(TreeItem::new(s3_id, "S3"));
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
                kind: file.kind.clone(),
            }),
            table: table_ref.clone(),
            column: None,
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
        Some("VIEW".into())
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
        ColumnRef, TableRef, alter_column_type_sql, select_column_sql, select_star_sql,
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
