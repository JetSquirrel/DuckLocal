//! Left sidebar: Schema / 查询历史 / 扩展 tabs. The schema page renders the
//! catalog as a tree (数据库 → schema → 表/视图 → 列); the history page lists
//! recent queries and refills the active editor on click; the extensions
//! page lists DuckDB's extensions and installs, loads or updates them.
//!
//! ```text
//! src/ui/sidebar/model.rs    what each tree row is and can act on
//! src/ui/sidebar/sql.rs      generated SELECT/ALTER statements (pure, tested)
//! src/ui/sidebar/s3.rs       the lazy S3 browse tree and its async loads
//! src/ui/sidebar/tree.rs     catalog + files + S3, as TreeItems
//! src/ui/sidebar/actions.rs  refresh, remove-file, alter-type actions
//! src/ui/sidebar/selection.rs  files picked together, columns ticked
//! src/ui/sidebar/extensions.rs  the 扩展 page
//! ```

mod actions;
mod extensions;
mod model;
mod s3;
mod selection;
mod sql;
mod tree;

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::tree::{tree, TreeEvent, TreeState};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::history::HistoryEntry;
use crate::i18n::{tr, trf, Language};
use crate::recents::RecentDocument;
use crate::state::{
    format_rows, AppState, AttachedFilesChanged, CatalogChanged, ConnectionChanged, HistoryChanged,
    RecentsChanged, S3ConfigChanged,
};
use crate::ui::workspace::Workspace;

use self::extensions::ExtensionList;
use self::model::ColumnRef;
use self::model::{SchemaNodeKind, SchemaNodeMeta};
use self::s3::S3Browse;
use self::selection::{ColumnPick, FileSelection};
use self::sql::{
    overview_sql, select_column_sql, select_columns_sql, select_s3_file_sql, select_star_sql,
    table_info_text,
};
use self::tree::{build_tree_items, filter_tree};

#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Schema,
    History,
    Extensions,
}

const INDENT_PER_DEPTH: Pixels = px(16.);

pub struct Sidebar {
    state: Entity<AppState>,
    workspace: Entity<Workspace>,
    tab: SidebarTab,
    tree_state: Entity<TreeState>,
    node_meta: Rc<HashMap<SharedString, SchemaNodeMeta>>,
    /// S3 browse tree, present while S3 is configured.
    s3_browse: Option<S3Browse>,
    /// Recently opened dashboards, shown as a clickable group.
    recents: Rc<Vec<RecentDocument>>,
    /// A schema reload is in flight; the refresh button shows loading and
    /// repeat clicks are ignored until it finishes.
    refreshing_schema: bool,
    /// Local files picked together with Shift or Cmd+click, for one remove.
    file_selection: FileSelection,
    /// Columns ticked in the tree, which the editor selects.
    column_pick: ColumnPick,
    /// Narrows the schema tree to rows whose name holds the text.
    search: Entity<InputState>,
    extensions: ExtensionList,
    /// The extension an install, load or update is running for; every
    /// extension button waits while one is.
    extension_busy: Option<String>,
    /// The language the tree's group labels were built in. Nothing notifies
    /// on a language switch beyond `cx.refresh_windows()`, so render compares
    /// and rebuilds the tree when it no longer matches.
    tree_language: Language,
    _subscriptions: Vec<Subscription>,
}

impl Sidebar {
    pub fn new(
        state: Entity<AppState>,
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("sidebar.search.placeholder")));
        let subscriptions = vec![
            cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.rebuild_tree(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&state, |this, _, _: &CatalogChanged, cx| {
                // A new connection also emits CatalogChanged; only drop the
                // browse tree when S3 actually became unconfigured.
                if this.state.read(cx).s3_config.is_none() {
                    this.s3_browse = None;
                }
                this.rebuild_tree(cx);
                cx.notify();
            }),
            cx.subscribe(&state, |_, _, _: &HistoryChanged, cx| cx.notify()),
            // Another database is another engine instance, with its own
            // loaded set.
            cx.subscribe(&state, |this, _, _: &ConnectionChanged, cx| {
                this.extensions = ExtensionList::NotLoaded;
                if this.tab == SidebarTab::Extensions {
                    this.reload_extensions(cx);
                }
            }),
            cx.subscribe(&state, |this, _, _: &AttachedFilesChanged, cx| {
                this.rebuild_tree(cx);
                cx.notify();
            }),
            cx.subscribe(&state, |this, _, _: &RecentsChanged, cx| {
                this.recents = Rc::new(crate::recents::list());
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
            recents: Rc::new(crate::recents::list()),
            refreshing_schema: false,
            file_selection: FileSelection::default(),
            column_pick: ColumnPick::default(),
            search,
            extensions: ExtensionList::NotLoaded,
            extension_busy: None,
            tree_language: crate::i18n::current(),
            _subscriptions: subscriptions,
        }
    }

    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let (catalog, attached_files) = {
            let state = self.state.read(cx);
            (state.catalog.clone(), state.attached_files.clone())
        };
        let (items, meta) = build_tree_items(
            &catalog,
            &attached_files,
            &self.recents,
            self.s3_browse.as_ref(),
        );
        self.column_pick.retain(|table, name| {
            meta.values()
                .filter_map(|meta| meta.column.as_ref())
                .any(|column| column.table == *table && column.name == name)
        });
        let items = filter_tree(items, &self.search.read(cx).value());
        self.node_meta = Rc::new(meta);
        let order: Vec<i64> = attached_files.iter().map(|file| file.id).collect();
        self.file_selection.retain(&order);
        self.tree_language = crate::i18n::current();
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
    }

    /// The registered files' ids, in the order the list shows them.
    fn file_order(&self, cx: &App) -> Vec<i64> {
        self.state
            .read(cx)
            .attached_files
            .iter()
            .map(|file| file.id)
            .collect()
    }

    fn click_file(&mut self, id: i64, shift: bool, toggle: bool, cx: &mut Context<Self>) {
        let order = self.file_order(cx);
        self.file_selection.click(id, &order, shift, toggle);
        // The tree draws its own one selected row over whatever the row
        // asks for: keep it on the file just clicked, or on none when that
        // click dropped it.
        let row = SharedString::from(format!("file:{id}"));
        let picked = self.file_selection.contains(id);
        self.tree_state.update(cx, |tree, cx| {
            let ix = tree.index_of(&row).filter(|_| picked);
            tree.set_selected_index(ix, cx);
        });
        cx.notify();
    }

    /// Tick or untick `column`, and put the ticked columns' SELECT in the
    /// editor: all of the table's once the last is unticked.
    fn toggle_column(&mut self, column: &ColumnRef, window: &mut Window, cx: &mut Context<Self>) {
        let table = &column.table;
        let order: Vec<String> = self
            .state
            .read(cx)
            .catalog
            .iter()
            .filter(|db| db.name == table.database)
            .flat_map(|db| db.tables.iter())
            .find(|t| t.schema == table.schema && t.name == table.name)
            .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default();
        self.column_pick.toggle(table, &column.name, &order);
        let path = self.state.read(cx).search_path.clone();
        let sql = match self.column_pick.picked() {
            Some((table, names)) => select_columns_sql(table, &names, path.as_ref()),
            None => return,
        };
        self.workspace
            .update(cx, |ws, cx| ws.fill_active_editor(sql, window, cx));
        cx.notify();
    }

    fn clear_file_selection(&mut self, cx: &mut Context<Self>) {
        if !self.file_selection.is_empty() {
            self.file_selection.clear();
            cx.notify();
        }
    }

    /// Ask to remove file `id`, and with it every file picked beside it.
    fn confirm_remove_files_from(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.file_selection.targets(id, &self.file_order(cx));
        let files: Vec<_> = targets
            .iter()
            .filter_map(|id| {
                self.node_meta
                    .get(&SharedString::from(format!("file:{id}")))
                    .and_then(|meta| meta.file.clone())
            })
            .collect();
        if !files.is_empty() {
            self.confirm_remove_files(files, window, cx);
        }
    }

    /// The picked files' ids, in list order, and the tree's own selected row.
    #[cfg(test)]
    pub(crate) fn picked_files(&self, cx: &App) -> (Vec<i64>, Option<usize>) {
        let order = self.file_order(cx);
        let picked = order
            .iter()
            .copied()
            .filter(|id| self.file_selection.contains(*id))
            .collect();
        (picked, self.tree_state.read(cx).selected_index())
    }

    /// The label of the tree row at `ix`, as the tree would draw it.
    #[cfg(test)]
    pub(crate) fn tree_label(&self, ix: usize, cx: &App) -> Option<SharedString> {
        let state = self.tree_state.read(cx);
        state.entry(ix).map(|entry| entry.item().label.clone())
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
            .cursor_pointer()
            .hover(|this| this.bg(cx.theme().accent))
            .active(|this| this.bg(cx.theme().accent.opacity(0.7)))
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
        let recents = self.recents.clone();
        let has_items = {
            let state = self.state.read(cx);
            state.catalog_has_content()
                || !state.attached_files.is_empty()
                || state.s3_config.is_some()
                || !recents.is_empty()
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
        let state = self.state.clone();
        let s3_endpoint = self
            .state
            .read(cx)
            .s3_config
            .as_ref()
            .map(|config| config.endpoint.clone());
        // What the rows draw and count from; a click re-renders with the new one.
        let order = self.file_order(cx);
        let picked: Rc<Vec<i64>> = Rc::new(
            order
                .iter()
                .copied()
                .filter(|id| self.file_selection.contains(*id))
                .collect(),
        );
        let ticked = Rc::new(self.column_pick.clone());
        let no_match = !self.search.read(cx).value().trim().is_empty()
            && self.tree_state.read(cx).entry(0).is_none();
        let menu = RowMenu {
            picked: picked.clone(),
            meta: meta.clone(),
            sidebar: sidebar.clone(),
            workspace: workspace.clone(),
            state: state.clone(),
        };
        let rows = tree(&self.tree_state, move |ix, entry, selected, _window, cx| {
            let item = entry.item();
            let node_meta = meta.get(&item.id);
            let icon: Option<gpui_kit::assets::IconName> = node_meta.and_then(|m| match m.kind {
                SchemaNodeKind::Database => Some(IconName::HardDrive.into()),
                // A section is a heading, not a folder: its glyph says it
                // folds, nothing more.
                SchemaNodeKind::LocalFilesGroup
                | SchemaNodeKind::DashboardsGroup
                | SchemaNodeKind::S3Status => Some(if entry.is_expanded() {
                    IconName::ChevronDown.into()
                } else {
                    IconName::ChevronRight.into()
                }),
                SchemaNodeKind::Schema => Some(if entry.is_expanded() {
                    IconName::FolderOpen.into()
                } else {
                    IconName::Folder.into()
                }),
                SchemaNodeKind::RecentDocument => m
                    .doc
                    .as_ref()
                    .map(|_| gpui_kit::assets::IconName::ChartNoAxesColumn),
                SchemaNodeKind::Table => Some(IconName::GalleryVerticalEnd.into()),
                SchemaNodeKind::View => Some(IconName::Eye.into()),
                SchemaNodeKind::File | SchemaNodeKind::S3File => Some(IconName::File.into()),
                SchemaNodeKind::S3Bucket => Some(gpui_kit::assets::IconName::Inbox),
                SchemaNodeKind::S3Prefix => Some(if entry.is_expanded() {
                    IconName::FolderOpen.into()
                } else {
                    IconName::Folder.into()
                }),
                SchemaNodeKind::Column | SchemaNodeKind::S3Message => None,
            });

            let group_name = item.id.clone();
            // Two levels of heading. A section (Local files, Dashboards, S3)
            // is a label over a list: small, muted, set apart by weight. A
            // database is a row that holds rows: full size, bold. Drawn alike
            // they competed, and a database read as one more section.
            let is_section = node_meta.is_some_and(|m| m.kind.is_section());
            let is_database =
                node_meta.is_some_and(|m| m.kind == SchemaNodeKind::Database);
            let hint = node_meta.and_then(|m| m.hint.clone());
            let tooltip: Option<SharedString> = match node_meta.map(|m| m.kind) {
                Some(SchemaNodeKind::S3Status) => s3_endpoint.clone().map(Into::into),
                _ => node_meta.and_then(|m| m.tooltip.clone()),
            };
            let file = node_meta.and_then(|m| m.file.clone());
            let table = node_meta.and_then(|m| m.table.clone());
            let column = node_meta.and_then(|m| m.column.clone());
            let s3_uri = node_meta.and_then(|m| m.s3_uri.clone());
            let doc = node_meta.and_then(|m| m.doc.clone());
            let removable_doc = doc.clone();
            let is_s3_root = node_meta.map(|m| m.kind) == Some(SchemaNodeKind::S3Status);
            let editable_column = column.clone().filter(|column| !column.table.is_view);
            let has_actions = is_s3_root
                || table.is_some()
                || editable_column.is_some()
                || removable_doc.is_some()
                || file.is_some();
            let detail = node_meta.and_then(|m| m.detail.clone());
            // The tree marks only its own selected row, and overrides
            // `selected` with it: the other picked files take the same
            // background themselves.
            let file_id = file.as_ref().map(|file| file.id);
            let also_picked = !selected && file_id.is_some_and(|id| picked.contains(&id));
            let remove_count = match file_id {
                Some(id) if picked.contains(&id) => picked.len(),
                _ => 1,
            };
            ListItem::new(ix)
                .selected(selected)
                .when(also_picked, |this| {
                    this.bg(if cx.theme().list.active_highlight {
                        cx.theme().list_active
                    } else {
                        cx.theme().accent
                    })
                })
                .on_mouse_down(MouseButton::Left, {
                    let sidebar = sidebar.clone();
                    move |event, _, cx| {
                        let Some(sidebar) = sidebar.upgrade() else {
                            return;
                        };
                        let shift = event.modifiers.shift;
                        let toggle = event.modifiers.secondary();
                        sidebar.update(cx, |this, cx| match file_id {
                            Some(id) => this.click_file(id, shift, toggle, cx),
                            None => this.clear_file_selection(cx),
                        });
                        // A Shift or Cmd+click picks; it does not also open
                        // the file's columns the way a plain click does.
                        if file_id.is_some() && (shift || toggle) {
                            cx.stop_propagation();
                        }
                    }
                })
                .pl(INDENT_PER_DEPTH * entry.depth() + px(12.))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .group(group_name.clone())
                        .child(match (icon, &column) {
                            (Some(icon), _) => Icon::new(icon)
                                .xsmall()
                                .text_color(cx.theme().muted_foreground)
                                .into_any_element(),
                            // A column's tick puts it in the editor's SELECT.
                            (None, Some(column)) => {
                                let sidebar = sidebar.clone();
                                let column = column.clone();
                                Checkbox::new(("pick-column", ix))
                                    .xsmall()
                                    .checked(ticked.contains(&column.table, &column.name))
                                    .on_click(move |_, window, cx| {
                                        if let Some(sidebar) = sidebar.upgrade() {
                                            sidebar.update(cx, |this, cx| {
                                                this.toggle_column(&column, window, cx)
                                            });
                                        }
                                    })
                                    .into_any_element()
                            }
                            // The width of an xsmall icon, so rows line up at every size.
                            (None, None) => div().w_3().flex_shrink_0().into_any_element(),
                        })
                        .child(
                            h_flex()
                                .id(("row-label", ix))
                                .flex_1()
                                .min_w_0()
                                .gap_1p5()
                                .items_baseline()
                                .map(|this| {
                                    if is_section {
                                        this.text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(cx.theme().muted_foreground)
                                    } else if is_database {
                                        this.text_sm().font_weight(FontWeight::SEMIBOLD)
                                    } else {
                                        this.text_sm()
                                    }
                                })
                                // The name keeps most of the row; the muted
                                // hint that tells same-named rows apart gives
                                // way first.
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_shrink_0()
                                        .max_w(relative(if hint.is_some() { 0.7 } else { 1. }))
                                        .truncate()
                                        .child(item.label.clone()),
                                )
                                .when_some(hint, |this, hint| {
                                    this.child(
                                        div()
                                            .min_w_0()
                                            .flex_shrink(1.)
                                            .truncate()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(hint),
                                    )
                                })
                                .when_some(tooltip, |this, tooltip| {
                                    this.tooltip(move |window, cx| {
                                        Tooltip::new(tooltip.clone()).build(window, cx)
                                    })
                                })
                                // The name ticks the column as its box does.
                                .when_some(column, |this, column| {
                                    let sidebar = sidebar.clone();
                                    this.cursor_pointer()
                                        .hover(|this| this.text_color(cx.theme().primary))
                                        .active(|this| {
                                            this.text_color(cx.theme().primary.opacity(0.7))
                                        })
                                        .on_click(move |_, window, cx| {
                                            if let Some(sidebar) = sidebar.upgrade() {
                                                sidebar.update(cx, |this, cx| {
                                                    this.toggle_column(&column, window, cx)
                                                });
                                            }
                                        })
                                })
                                .when_some(s3_uri, |this, uri| {
                                    let workspace = workspace.clone();
                                    this.cursor_pointer()
                                        .hover(|this| this.text_color(cx.theme().primary))
                                        .active(|this| {
                                            this.text_color(cx.theme().primary.opacity(0.7))
                                        })
                                        .on_click(move |_, window, cx| {
                                            workspace.update(cx, |ws, cx| {
                                                ws.fill_active_editor(
                                                    select_s3_file_sql(&uri),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        })
                                })
                                .when_some(doc, |this, doc| {
                                    let workspace = workspace.clone();
                                    let state = state.clone();
                                    this.cursor_pointer()
                                        .hover(|this| this.text_color(cx.theme().primary))
                                        .active(|this| {
                                            this.text_color(cx.theme().primary.opacity(0.7))
                                        })
                                        .on_click(move |_, window, cx| {
                                            let path = std::path::PathBuf::from(&doc.path);
                                            if !path.exists() {
                                                crate::recents::remove(&doc.path);
                                                window.push_notification(
                                                    trf("sidebar.recents.gone", &[&doc.title]),
                                                    cx,
                                                );
                                                state.update(cx, |_, cx| {
                                                    cx.emit(RecentsChanged);
                                                });
                                                return;
                                            }
                                            workspace.update(cx, |ws, cx| {
                                                ws.open_dashboard(path, window, cx)
                                            });
                                        })
                                }),
                        )
                        // The count and the hover actions share one slot at the
                        // row's end: the count steps aside while the pointer is
                        // on the row, so no row keeps an empty column for
                        // buttons it is not showing.
                        .when(detail.is_some() || has_actions, |this| this.child(
                            h_flex()
                                .relative()
                                .flex_shrink_0()
                                .justify_end()
                                .when(has_actions, |this| {
                                    this.min_w(crate::ui::scale::design(44.))
                                })
                                .when_some(detail, |this, detail| {
                                    this.child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .when(has_actions, |this| {
                                                this.group_hover(group_name.clone(), |style| {
                                                    style.opacity(0.)
                                                })
                                            })
                                            .child(detail),
                                    )
                                })
                                .when(has_actions, |this| this.child(
                                    h_flex()
                                        .id(("row-actions", ix))
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .right_0()
                                        .items_center()
                                        .justify_end()
                                        .gap_1()
                                        .opacity(0.)
                                        .group_hover(group_name, |style| style.opacity(1.))
                                        // A press on a button is not a click on
                                        // the row: it keeps the files picked.
                                        .on_mouse_down(MouseButton::Left, |_, _, cx: &mut App| {
                                            cx.stop_propagation()
                                        })
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
                                            let state = state.clone();
                                            this.child(
                                                Button::new(("generate-query", ix))
                                                    .ghost()
                                                    .xsmall()
                                                    .icon(IconName::Play)
                                                    .tooltip(tr("sidebar.table.generate_select"))
                                                    .on_click(move |_, window, cx| {
                                                        let path = state.read(cx).search_path.clone();
                                                        workspace.update(cx, |ws, cx| {
                                                            ws.fill_active_editor(
                                                                select_star_sql(&table, path.as_ref()),
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
                                        // A recent dashboard leaves the list —
                                        // only the list: its files and any open tab
                                        // stay, so there is nothing to confirm.
                                        .when_some(removable_doc, |this, doc| {
                                            let state = state.clone();
                                            this.child(
                                                Button::new(("remove-recent", ix))
                                                    .ghost()
                                                    .xsmall()
                                                    .icon(IconName::Close)
                                                    .tooltip(tr("sidebar.recent.remove"))
                                                    .on_click(move |_, _, cx| {
                                                        crate::recents::remove(&doc.path);
                                                        state.update(cx, |_, cx| {
                                                            cx.emit(RecentsChanged);
                                                        });
                                                    }),
                                            )
                                        })
                                        .when_some(file, |this, file| {
                                            this.child(
                                                Button::new(("remove-file", file.id as usize))
                                                    .ghost()
                                                    .xsmall()
                                                    .icon(IconName::Close)
                                                    .tooltip(remove_label(remove_count))
                                                    .on_click({
                                                        let sidebar = sidebar.clone();
                                                        move |_, window, cx| {
                                                            if let Some(sidebar) = sidebar.upgrade() {
                                                                sidebar.update(cx, |this, cx| {
                                                                    this.confirm_remove_files_from(
                                                                        file.id,
                                                                        window,
                                                                        cx,
                                                                    );
                                                                });
                                                            }
                                                        }
                                                    }),
                                            )
                                        }),
                                ))
                        ))
                )
        })
        // Every hover button on a row is a shortcut to an entry here: the
        // buttons only show under the pointer, and a command that can only
        // be found by hovering is not one a keyboard or a new user finds.
        .context_menu(move |_, entry, popup, _, cx| menu.build(&entry.item().id, popup, cx));

        v_flex()
            .size_full()
            .child(
                div().px_2().pb_2().child(
                    Input::new(&self.search)
                        .prefix(Icon::new(IconName::Search).xsmall())
                        .cleanable(true)
                        .small(),
                ),
            )
            .child(div().flex_1().min_h_0().map(|this| {
                if no_match {
                    this.p_4()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .text_center()
                        .child(tr("sidebar.search.no_match"))
                } else {
                    this.child(rows)
                }
            }))
            .into_any_element()
    }
}

/// What a schema row's context menu needs: the row's metadata and handles
/// to act on.
struct RowMenu {
    /// The files picked together, in list order.
    picked: Rc<Vec<i64>>,
    meta: Rc<HashMap<SharedString, SchemaNodeMeta>>,
    sidebar: WeakEntity<Sidebar>,
    workspace: Entity<Workspace>,
    state: Entity<AppState>,
}

impl RowMenu {
    fn build(&self, id: &SharedString, mut menu: PopupMenu, cx: &App) -> PopupMenu {
        let Some(meta) = self.meta.get(id) else {
            return menu;
        };
        if meta.column.is_some() || meta.table.is_some() {
            let workspace = self.workspace.clone();
            let state = self.state.clone();
            let (column, table) = (meta.column.clone(), meta.table.clone());
            menu = menu.item(
                PopupMenuItem::new(tr("sidebar.table.generate_select"))
                    .icon(IconName::Play)
                    .on_click(move |_, window, cx| {
                        // Read at click time: a Run since the menu opened
                        // may have moved the search path.
                        let path = state.read(cx).search_path.clone();
                        let sql = match (&column, &table) {
                            (Some(column), _) => select_column_sql(column, path.as_ref()),
                            (None, Some(table)) => select_star_sql(table, path.as_ref()),
                            (None, None) => return,
                        };
                        workspace.update(cx, |ws, cx| ws.fill_active_editor(sql, window, cx));
                    }),
            );
        }
        if let Some(table) = meta.table.clone() {
            let workspace = self.workspace.clone();
            let state = self.state.clone();
            menu = menu.item(
                PopupMenuItem::new(tr("sidebar.menu.overview"))
                    .icon(gpui_kit::assets::IconName::ChartColumn)
                    .on_click(move |_, window, cx| {
                        let path = state.read(cx).search_path.clone();
                        let sql = overview_sql(&table, path.as_ref());
                        workspace.update(cx, |ws, cx| ws.run_for_overview(sql, window, cx));
                    }),
            );
        }
        let name = meta
            .column
            .as_ref()
            .map(|column| column.name.clone())
            .or_else(|| meta.table.as_ref().map(|table| table.name.clone()))
            .or_else(|| meta.file.as_ref().map(|file| file.view_name.clone()));
        if let Some(name) = name {
            menu = menu.item(
                PopupMenuItem::new(tr("sidebar.menu.copy_name"))
                    .icon(IconName::Copy)
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(name.clone()));
                    }),
            );
        }
        // A table row, or a file row's view — not a column's row, which
        // already says all there is about the column.
        if let Some(table) = meta.table.clone().filter(|_| meta.column.is_none()) {
            let state = self.state.clone();
            menu = menu.item(
                PopupMenuItem::new(tr("sidebar.menu.copy_table_info"))
                    .icon(IconName::Copy)
                    .on_click(move |_, _, cx| {
                        // The columns come from the catalog, read at click
                        // time; the row itself does not carry them.
                        let text = state
                            .read(cx)
                            .catalog
                            .iter()
                            .filter(|db| db.name == table.database)
                            .flat_map(|db| &db.tables)
                            .find(|t| t.schema == table.schema && t.name == table.name)
                            .map(table_info_text);
                        if let Some(text) = text {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                    }),
            );
        }
        if let Some(column) = meta.column.clone().filter(|column| !column.table.is_view) {
            let sidebar = self.sidebar.clone();
            menu = menu.item(
                PopupMenuItem::new(tr("sidebar.column.edit_type"))
                    .icon(gpui_kit::assets::IconName::CaseSensitive)
                    .on_click(move |_, window, cx| {
                        if let Some(sidebar) = sidebar.upgrade() {
                            sidebar.update(cx, |this, cx| {
                                this.open_alter_type_dialog(column.clone(), window, cx)
                            });
                        }
                    }),
            );
        }
        if let Some(database) = meta.database.clone() {
            // Where unqualified names resolve now; unknown until a Run
            // reports it, when the open database is the safe guess.
            let in_use = match &self.state.read(cx).search_path {
                Some(path) => path.database.eq_ignore_ascii_case(&database.name),
                None => database.is_main,
            };
            if !in_use {
                let sidebar = self.sidebar.clone();
                let name = database.name.clone();
                menu = menu.item(
                    PopupMenuItem::new(tr("sidebar.database.use"))
                        .icon(IconName::CircleCheck)
                        .on_click(move |_, window, cx| {
                            if let Some(sidebar) = sidebar.upgrade() {
                                sidebar.update(cx, |this, cx| {
                                    this.use_database(name.clone(), window, cx)
                                });
                            }
                        }),
                );
            }
            if !database.is_main {
                let state = self.state.clone();
                menu = menu.separator().item(
                    PopupMenuItem::new(tr("sidebar.database.detach"))
                        .icon(gpui_kit::assets::IconName::DatabaseX)
                        .on_click(move |_, window, cx| {
                            crate::ui::detach_database(
                                state.clone(),
                                database.name.clone(),
                                window,
                                cx,
                            );
                        }),
                );
            }
        }
        if meta.kind == SchemaNodeKind::S3Status {
            let sidebar = self.sidebar.clone();
            menu = menu.item(
                PopupMenuItem::new(tr("sidebar.s3.refresh_buckets"))
                    .icon(IconName::RotateCw)
                    .on_click(move |_, _, cx| {
                        if let Some(sidebar) = sidebar.upgrade() {
                            sidebar.update(cx, |this, cx| this.refresh_s3(cx));
                        }
                    }),
            );
        }
        if let Some(doc) = meta.doc.clone() {
            let state = self.state.clone();
            menu = menu.separator().item(
                PopupMenuItem::new(tr("sidebar.menu.remove_recent"))
                    .icon(IconName::Close)
                    .on_click(move |_, _, cx| {
                        crate::recents::remove(&doc.path);
                        state.update(cx, |_, cx| cx.emit(RecentsChanged));
                    }),
            );
        }
        if let Some(file) = meta.file.clone() {
            let sidebar = self.sidebar.clone();
            let count = if self.picked.contains(&file.id) {
                self.picked.len()
            } else {
                1
            };
            menu = menu.separator().item(
                PopupMenuItem::new(remove_label(count))
                    .icon(IconName::Close)
                    .on_click(move |_, window, cx| {
                        if let Some(sidebar) = sidebar.upgrade() {
                            sidebar.update(cx, |this, cx| {
                                this.confirm_remove_files_from(file.id, window, cx)
                            });
                        }
                    }),
            );
        }
        menu
    }
}

/// What the remove action on a file row says: the picked files' count when
/// it takes more than the one.
fn remove_label(count: usize) -> SharedString {
    if count > 1 {
        trf("sidebar.file.remove_many", &[&count.to_string()]).into()
    } else {
        tr("sidebar.file.remove").into()
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.tree_language != crate::i18n::current() {
            self.rebuild_tree(cx);
        }
        // An `Rc` handle: the list is read while `cx` is borrowed mutably for
        // rendering, and copying 200 entries every frame is not the way to do it.
        let history: Rc<Vec<HistoryEntry>> = self.state.read(cx).history.clone();

        v_flex()
            .size_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                h_flex()
                    .p_2()
                    .gap_1()
                    .items_center()
                    .child(
                        TabBar::new("sidebar-tabs")
                            .segmented()
                            .with_size(crate::ui::scale::compact_control())
                            .selected_index(match self.tab {
                                SidebarTab::Schema => 0,
                                SidebarTab::History => 1,
                                SidebarTab::Extensions => 2,
                            })
                            .on_click(cx.listener(|this, ix, _, cx| {
                                this.tab = match ix {
                                    0 => SidebarTab::Schema,
                                    1 => SidebarTab::History,
                                    _ => SidebarTab::Extensions,
                                };
                                if this.tab == SidebarTab::Extensions {
                                    this.reload_extensions(cx);
                                }
                                cx.notify();
                            }))
                            .child(Tab::new().label(tr("sidebar.tab.schema")))
                            .child(Tab::new().label(tr("sidebar.tab.history")))
                            .child(Tab::new().label(tr("sidebar.tab.extensions"))),
                    )
                    // The sidebar's own actions share the tabs' row, as borderless
                    // icon buttons. Putting the sidebar away is the title bar's
                    // toggle, which stays put whether the sidebar is shown or not.
                    .child(div().flex_1())
                    .when(self.tab == SidebarTab::Schema, |this| {
                        this.child(
                            Button::new("refresh-schema")
                                .ghost()
                                .xsmall()
                                .icon(IconName::RotateCw)
                                .tooltip(tr("sidebar.refresh_schema"))
                                .loading(self.refreshing_schema)
                                .disabled(self.refreshing_schema)
                                .on_click(cx.listener(Self::refresh_schema)),
                        )
                    })
                    .when(self.tab == SidebarTab::Extensions, |this| {
                        this.child(
                            Button::new("refresh-extensions")
                                .ghost()
                                .xsmall()
                                .icon(IconName::RotateCw)
                                .tooltip(tr("sidebar.extensions.refresh"))
                                .loading(matches!(self.extensions, ExtensionList::Loading))
                                .on_click(cx.listener(|this, _, _, cx| this.reload_extensions(cx))),
                        )
                    }),
            )
            .child(div().flex_1().min_h_0().child(match self.tab {
                SidebarTab::Schema => self.render_schema(window, cx),
                SidebarTab::Extensions => self.render_extensions(cx),
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
