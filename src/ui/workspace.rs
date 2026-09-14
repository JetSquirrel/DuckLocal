//! Center workspace: query tabs, toolbar (运行 / 格式化 / EXPLAIN), SQL
//! editor, and the results panel, split vertically by a resizable handle.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState, TabSize};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, IconName, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::query::QueryOutcome;
use crate::state::{AppState, ConnectionChanged, QueryStats};
use crate::ui::completion;
use crate::ui::results::ResultsPanel;
use crate::ui::{RUN_QUERY_KEYSTROKE, RunQuery, WORKSPACE_KEY_CONTEXT};

const RESULTS_PANEL_DEFAULT: f32 = 320.;
const RESULTS_PANEL_MIN: f32 = 160.;
const RESULTS_PANEL_MAX: f32 = 640.;

pub struct QueryTab {
    pub id: u64,
    pub title: SharedString,
    pub editor: Entity<EditorState>,
}

pub struct Workspace {
    state: Entity<AppState>,
    tabs: Vec<QueryTab>,
    active: usize,
    next_tab_id: u64,
    running: bool,
    explaining: bool,
    results: Entity<ResultsPanel>,
    rename_input: Option<Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let results = cx.new(|cx| ResultsPanel::new(window, cx));
        let mut this = Self {
            state: state.clone(),
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
            running: false,
            explaining: false,
            results,
            rename_input: None,
            _subscriptions: vec![cx.subscribe(&state, |_, _, _: &ConnectionChanged, cx| {
                cx.notify()
            })],
        };
        let tab = this.new_tab_editor(window, cx);
        let run_hint = Keystroke::parse(RUN_QUERY_KEYSTROKE)
            .map(|k| Kbd::format(&k))
            .unwrap_or_else(|_| "⌘↵".to_string());
        tab.editor.update(cx, |editor, cx| {
            editor.set_value(trf("workspace.welcome_sql", &[&run_hint]), window, cx);
        });
        this.tabs.push(tab);
        this
    }

    fn new_tab_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> QueryTab {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
        });
        let provider = completion::SqlCompletionProvider::new(self.state.clone());
        editor.update(cx, |state, cx| {
            state.lsp_mut().completion_provider = Some(provider);
            cx.notify();
        });
        // ⌘↵ reaches the editor as `secondary-enter`, which the Input key
        // context (deeper than the workspace's) binds to "insert newline".
        // The keybinding in `ui::init` therefore never fires while the editor
        // is focused; the PressEnter event is the only signal left. The
        // newline is already inserted by then, so undo it before running.
        let subscription = cx.subscribe_in(&editor, window, |this, editor, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { secondary: true, .. }) {
                Self::remove_secondary_enter_newline(&editor, window, cx);
                this.run_active(window, cx);
            }
        });
        self._subscriptions.push(subscription);
        QueryTab {
            id,
            title: trf("workspace.tab.default_title", &[&id.to_string()]).into(),
            editor,
        }
    }

    pub fn focus_active_editor(&self, window: &mut Window, cx: &mut App) {
        if let Some(tab) = self.tabs.get(self.active) {
            tab.editor.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    /// Replace the active editor's content with a history entry.
    pub fn fill_active_editor(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        tab.editor.update(cx, |editor, cx| {
            editor.set_value(sql, window, cx);
        });
        self.focus_active_editor(window, cx);
    }

    /// The editor inserts `"\n" + indent` before emitting `PressEnter`; the
    /// cursor then sits right after the insertion. Delete exactly that
    /// insertion — walk back over the indent, then require a newline, and do
    /// nothing if the text before the cursor does not match that shape.
    /// With multiple cursors only the active one is repaired.
    fn remove_secondary_enter_newline(
        editor: &Entity<EditorState>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let range = editor.update(cx, |editor, _| {
            let cursor = editor.cursor();
            let text = editor.text();
            let mut start = cursor;
            let mut chars = text.chars_at(cursor);
            let mut prev = chars.prev();
            // The inserted indent is plain spaces/tabs (ASCII), so byte
            // arithmetic on `start` stays on char boundaries.
            while matches!(prev, Some(' ' | '\t')) {
                start -= 1;
                prev = chars.prev();
            }
            if prev != Some('\n') {
                return None;
            }
            Some(start - 1..cursor)
        });
        if let Some(range) = range {
            editor.update(cx, |editor, cx| {
                editor.set_selected_range(range, cx);
                editor.replace("", window, cx);
            });
        }
    }

    fn add_tab(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.new_tab_editor(window, cx);
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        cx.notify();
        self.focus_active_editor(window, cx);
    }

    fn close_tab(&mut self, tab_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            return;
        }
        let Some(ix) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        self.tabs.remove(ix);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > ix {
            self.active -= 1;
        }
        cx.notify();
        self.focus_active_editor(window, cx);
    }

    fn active_sql(&self, cx: &App) -> Option<String> {
        let tab = self.tabs.get(self.active)?;
        let sql = tab.editor.read(cx).value().to_string();
        (!sql.trim().is_empty()).then_some(sql)
    }

    fn open_rename_dialog(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(tab.title.clone()));
        self.rename_input = Some(input.clone());
        let view = cx.entity().downgrade();

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(tr("dialog.rename.title"))
                .w(px(360.))
                .child(Input::new(&input))
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
                            Button::new("confirm-rename")
                                .primary()
                                .label(tr("dialog.rename.confirm"))
                                .on_click({
                                    let input = input.clone();
                                    let view = view.clone();
                                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                        let title = input.read(cx).value().trim().to_string();
                                        window.close_dialog(cx);
                                        if let Some(view) = view.upgrade() {
                                            view.update(cx, |this, cx| {
                                                this.rename_active_tab(&title, cx);
                                            });
                                        }
                                    }
                                }),
                        ),
                )
        });
    }

    /// Rename the active tab; an empty title falls back to "查询 N".
    fn rename_active_tab(&mut self, title: &str, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        tab.title = if title.is_empty() {
            trf("workspace.tab.default_title", &[&tab.id.to_string()]).into()
        } else {
            title.into()
        };
        cx.notify();
    }

    fn run_query_action(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        self.run_active(window, cx);
    }

    fn run_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.explaining {
            return;
        }
        let Some(sql) = self.active_sql(cx) else {
            return;
        };
        self.running = true;
        self.results.update(cx, |results, cx| results.set_running(cx));
        cx.notify();

        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let run_sql = sql.clone();
            let (outcome, history, catalog) = smol::unblock(move || {
                // Decided before `run_sql` is handed to the history entry.
                let reload_catalog = crate::query::may_change_catalog(&run_sql);
                let outcome = crate::query::run(&run_sql);
                let (duration_ms, row_count, ok, error) = match &outcome {
                    Ok(QueryOutcome::Rows(result)) => {
                        (result.elapsed_ms as i64, Some(result.row_count() as i64), true, None)
                    }
                    Ok(QueryOutcome::Affected { count, elapsed_ms }) => {
                        (*elapsed_ms as i64, Some(*count as i64), true, None)
                    }
                    Err(e) => (0, None, false, Some(e.to_string())),
                };
                crate::history::record(&crate::history::HistoryEntry {
                    id: 0,
                    sql: run_sql,
                    started_at: crate::history::now_timestamp(),
                    duration_ms,
                    row_count,
                    ok,
                    error,
                })
                .ok();
                let history = crate::history::recent(200).unwrap_or_default();
                // A plain read leaves the sidebar valid, so skip the reload
                // instead of paying for it on the way back from every query.
                let catalog =
                    reload_catalog.then(|| crate::schema::load_catalog().unwrap_or_default());
                (outcome, history, catalog)
            })
            .await;

            this.update_in(cx, move |this, window, cx| {
                this.running = false;
                let stats = match &outcome {
                    Ok(QueryOutcome::Rows(result)) => Some(QueryStats {
                        elapsed_ms: result.elapsed_ms,
                        rows: result.row_count(),
                        cols: result.columns.len(),
                    }),
                    Ok(QueryOutcome::Affected { elapsed_ms, count }) => Some(QueryStats {
                        elapsed_ms: *elapsed_ms,
                        rows: *count as usize,
                        cols: 0,
                    }),
                    Err(_) => None,
                };
                this.results.update(cx, |results, cx| {
                    results.set_outcome(outcome, sql.clone(), window, cx);
                });
                state.update(cx, |s, cx| {
                    s.set_history(history, cx);
                    if let Some(catalog) = catalog {
                        s.set_catalog(catalog, cx);
                    }
                    if let Some(stats) = stats {
                        s.set_last_query(stats, sql.clone(), cx);
                    }
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn format_active(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        tab.editor.update(cx, |editor, cx| {
            let sql = editor.value().to_string();
            let formatted = sqlformat::format(
                &sql,
                &sqlformat::QueryParams::None,
                &sqlformat::FormatOptions {
                    indent: sqlformat::Indent::Spaces(2),
                    uppercase: Some(true),
                    ..Default::default()
                },
            );
            if !formatted.trim().is_empty() {
                editor.replace_all(formatted, window, cx);
            }
        });
    }

    fn explain_active(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.explaining || self.running {
            return;
        }
        let Some(sql) = self.active_sql(cx) else {
            return;
        };
        self.explaining = true;
        self.results.update(cx, |results, cx| results.set_running(cx));
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let explain_sql = sql.clone();
            let result = smol::unblock(move || {
                crate::db::with_connection(|conn| crate::query::explain_of(conn, &explain_sql))
            })
            .await;
            this.update_in(cx, move |this, window, cx| {
                this.explaining = false;
                this.results.update(cx, |results, cx| {
                    results.set_explain(result, window, cx);
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let closable = self.tabs.len() > 1;
        TabBar::new("query-tabs")
            .small()
            .selected_index(self.active)
            .on_click(cx.listener(|this, ix, _, cx| {
                this.active = *ix;
                cx.notify();
            }))
            .children(self.tabs.iter().map(|tab| {
                let tab_id = tab.id;
                Tab::new().label(tab.title.clone()).when(closable, |this| {
                    this.suffix(
                        Button::new(("close-tab", tab_id as usize))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close_tab(tab_id, window, cx);
                            })),
                    )
                })
            }))
            .suffix(
                Button::new("add-tab")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip(tr("workspace.new_query"))
                    .on_click(cx.listener(Self::add_tab)),
            )
    }

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let server = self.state.read(cx).server.clone();
        let run_keystroke = Keystroke::parse(RUN_QUERY_KEYSTROKE).ok();

        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("run-query")
                    .primary()
                    .small()
                    .icon(IconName::Play)
                    .label(tr("workspace.run"))
                    .loading(self.running)
                    .tooltip(tr("workspace.run.tooltip"))
                    .when_some(run_keystroke, |this, keystroke| {
                        this.child(Kbd::new(keystroke))
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.run_active(window, cx);
                    })),
            )
            .child(
                Button::new("format-sql")
                    .outline()
                    .small()
                    .label(tr("workspace.format"))
                    .tooltip(tr("workspace.format.tooltip"))
                    .on_click(cx.listener(Self::format_active)),
            )
            .child(
                Button::new("explain-sql")
                    .outline()
                    .small()
                    .label("EXPLAIN")
                    .loading(self.explaining)
                    .tooltip(tr("workspace.explain.tooltip"))
                    .on_click(cx.listener(Self::explain_active)),
            )
            .child(div().flex_1())
            .child(
                Button::new("rename-tab")
                    .ghost()
                    .small()
                    .label(tr("workspace.rename"))
                    .tooltip(tr("workspace.rename.tooltip"))
                    .on_click(cx.listener(Self::open_rename_dialog)),
            )
            .when_some(server, |this, server| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(trf(
                            "workspace.server_info",
                            &[&server.threads, &server.memory_limit],
                        )),
                )
            })
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self.tabs.get(self.active).map(|tab| tab.editor.clone());

        v_flex()
            .id("workspace")
            .size_full()
            .min_w_0()
            .key_context(WORKSPACE_KEY_CONTEXT)
            .on_action(cx.listener(Self::run_query_action))
            .child(self.render_tab_bar(cx))
            .child(self.render_toolbar(cx))
            .child(
                div().flex_1().min_h_0().child(
                    v_resizable("editor-results")
                        .child(
                            resizable_panel().child(
                                div()
                                    .size_full()
                                    .when_some(editor, |this, editor| {
                                        this.child(
                                            Editor::new(&editor)
                                                .h(relative(1.))
                                                .bordered(false),
                                        )
                                    }),
                            ),
                        )
                        .child(
                            resizable_panel()
                                .size(px(RESULTS_PANEL_DEFAULT))
                                .size_range(px(RESULTS_PANEL_MIN)..px(RESULTS_PANEL_MAX))
                                .child(self.results.clone()),
                        ),
                ),
            )
    }
}
