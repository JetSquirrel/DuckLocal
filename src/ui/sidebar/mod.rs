//! Left sidebar: Schema / 查询历史 tabs. The schema page renders the catalog
//! as a tree (数据库 → schema → 表/视图 → 列); the history page lists recent
//! queries and refills the active editor on click.
//!
//! ```text
//! src/ui/sidebar/model.rs    what each tree row is and can act on
//! src/ui/sidebar/sql.rs      generated SELECT/ALTER statements (pure, tested)
//! src/ui/sidebar/s3.rs       the lazy S3 browse tree and its async loads
//! src/ui/sidebar/tree.rs     catalog + files + S3, as TreeItems
//! src/ui/sidebar/actions.rs  refresh, remove-file, alter-type actions
//! ```

mod actions;
mod model;
mod s3;
mod sql;
mod tree;

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::tree::{tree, TreeEvent, TreeState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::history::HistoryEntry;
use crate::i18n::{tr, trf};
use crate::recents::{RecentDocument, RecentKind};
use crate::state::{
    format_rows, AppState, AttachedFilesChanged, ConnectionChanged, HistoryChanged, RecentsChanged,
    S3ConfigChanged,
};
use crate::ui::workspace::Workspace;

use self::model::{SchemaNodeKind, SchemaNodeMeta};
use self::s3::S3Browse;
use self::sql::{select_column_sql, select_s3_file_sql, select_star_sql};
use self::tree::build_tree_items;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Schema,
    History,
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
    /// Recently opened apps and dashboards, shown as clickable groups.
    recents: Rc<Vec<RecentDocument>>,
    /// A schema reload is in flight; the refresh button shows loading and
    /// repeat clicks are ignored until it finishes.
    refreshing_schema: bool,
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
        self.node_meta = Rc::new(meta);
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
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
            !state.catalog.is_empty()
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
        tree(&self.tree_state, move |ix, entry, selected, _window, cx| {
            let item = entry.item();
            let node_meta = meta.get(&item.id);
            let icon: Option<gpui_kit::assets::IconName> = node_meta.and_then(|m| match m.kind {
                SchemaNodeKind::Database => Some(IconName::HardDrive.into()),
                // A section is a heading, not a folder: its glyph says it
                // folds, nothing more.
                SchemaNodeKind::LocalFilesGroup
                | SchemaNodeKind::AppsGroup
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
                SchemaNodeKind::RecentDocument => m.doc.as_ref().map(|doc| match doc.kind {
                    RecentKind::App => gpui_kit::assets::IconName::AppWindow,
                    RecentKind::Dashboard => gpui_kit::assets::IconName::LayoutDashboard,
                }),
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
            let is_section = node_meta.is_some_and(|m| m.kind.is_section());
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
                            // The width of an xsmall icon, so rows line up at every size.
                            None => div().w_3().flex_shrink_0().into_any_element(),
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
                                    } else {
                                        this.text_sm()
                                    }
                                })
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .child(item.label.clone()),
                                )
                                .when_some(hint, |this, hint| {
                                    this.child(
                                        div()
                                            .flex_shrink_0()
                                            .max_w_1_2()
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
                                .when_some(column, |this, column| {
                                    let workspace = workspace.clone();
                                    this.cursor_pointer()
                                        .hover(|this| this.text_color(cx.theme().primary))
                                        .active(|this| {
                                            this.text_color(cx.theme().primary.opacity(0.7))
                                        })
                                        .on_click(move |_, window, cx| {
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
                                            workspace.update(cx, |ws, cx| match doc.kind {
                                                RecentKind::App => ws.open_app(path, window, cx),
                                                RecentKind::Dashboard => {
                                                    ws.open_dashboard(path, window, cx)
                                                }
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
                                .w(crate::ui::scale::design(44.))
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
                                // A recent app or dashboard leaves the list —
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
                )
                // The sidebar's own actions share the tabs' row, as borderless
                // icon buttons: refresh while the schema is shown, and the
                // button that puts the whole sidebar away.
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
                .child(
                    Button::new("collapse-sidebar")
                        .ghost()
                        .xsmall()
                        .icon(IconName::PanelLeftClose)
                        .tooltip_with_action(
                            tr("sidebar.collapse"),
                            &crate::ui::ToggleSidebar,
                            None,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |state, cx| state.toggle_sidebar(cx));
                        })),
                ),
            )
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
