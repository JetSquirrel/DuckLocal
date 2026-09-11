//! Title bar: app name + current database path, server status, theme toggle,
//! and the "打开数据库…" dialog.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputContentType, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, Theme, ThemeMode, TitleBar, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::db::{DatabaseTarget, ServerInfo};
use crate::history::HistoryEntry;
use crate::schema::DatabaseInfo;
use crate::state::AppState;

pub struct TitleBarView {
    state: Entity<AppState>,
    db_path_input: Option<Entity<InputState>>,
    s3_inputs: Option<S3Inputs>,
    _subscriptions: Vec<Subscription>,
}

struct S3Inputs {
    endpoint: Entity<InputState>,
    region: Entity<InputState>,
    key_id: Entity<InputState>,
    secret: Entity<InputState>,
}

impl TitleBarView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |_, _, _: &crate::state::ConnectionChanged, cx| {
            cx.notify();
        });
        Self {
            state,
            db_path_input: None,
            s3_inputs: None,
            _subscriptions: vec![subscription],
        }
    }

    fn toggle_theme(_: &ClickEvent, window: &mut Window, cx: &mut App) {
        let next = if cx.theme().mode.is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(next, Some(window), cx);
        // Theme::change resets font_size to the stock 16; pin our 14px base
        // again (see main.rs).
        Theme::global_mut(cx).font_size = px(14.);
        Theme::sync_base(cx);
    }

    fn open_s3_dialog(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let inputs = S3Inputs {
            endpoint: cx.new(|cx| {
                InputState::new(window, cx).default_value("s3.amazonaws.com")
            }),
            region: cx.new(|cx| InputState::new(window, cx).default_value("us-east-1")),
            key_id: cx.new(|cx| InputState::new(window, cx).placeholder("AKIA…")),
            secret: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("Secret Access Key")
            }),
        };
        let endpoint = inputs.endpoint.clone();
        let region = inputs.region.clone();
        let key_id = inputs.key_id.clone();
        let secret = inputs.secret.clone();
        self.s3_inputs = Some(inputs);
        let view = cx.entity().downgrade();

        window.open_dialog(cx, move |dialog, _, cx| {
            let confirm_view = view.clone();
            let endpoint = endpoint.clone();
            let region = region.clone();
            let key_id = key_id.clone();
            let secret = secret.clone();
            dialog
                .title("配置 S3 数据源")
                .w(px(440.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("配置后可直接查询 s3://bucket/路径 下的数据文件。凭据仅当前会话有效，不会写入磁盘；切换数据库连接后需重新配置。"),
                        )
                        .child(
                            v_flex()
                                .gap_2()
                                .child(labeled_field("Endpoint", Input::new(&endpoint).into_any_element(), cx))
                                .child(labeled_field("Region", Input::new(&region).into_any_element(), cx))
                                .child(labeled_field("Access Key ID", Input::new(&key_id).into_any_element(), cx))
                                .child(labeled_field(
                                    "Secret Access Key",
                                    Input::new(&secret)
                                        .content_type(InputContentType::Password)
                                        .mask_toggle()
                                        .into_any_element(),
                                    cx,
                                )),
                        ),
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
                            Button::new("save-s3")
                                .primary()
                                .label("启用 S3")
                                .on_click(move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                    let endpoint = endpoint.read(cx).value().trim().to_string();
                                    let region = region.read(cx).value().trim().to_string();
                                    let key_id = key_id.read(cx).value().trim().to_string();
                                    let secret = secret.read(cx).value().to_string();
                                    if key_id.is_empty() || secret.is_empty() {
                                        window.push_notification(
                                            Notification::error("Access Key ID 与 Secret Access Key 不能为空。"),
                                            cx,
                                        );
                                        return;
                                    }
                                    window.close_dialog(cx);
                                    if let Some(view) = confirm_view.upgrade() {
                                        view.update(cx, |this, cx| {
                                            this.configure_s3(endpoint, region, key_id, secret, window, cx);
                                        });
                                    }
                                }),
                        ),
                )
        });
    }

    /// Create the session-scoped S3 secret in the current connection.
    fn configure_s3(
        &mut self,
        endpoint: String,
        region: String,
        key_id: String,
        secret: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        let endpoint_label = endpoint.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(move || {
                let esc = |s: &str| s.replace('\'', "''");
                crate::db::with_connection(|conn| {
                    conn.execute_batch(&format!(
                        "INSTALL httpfs; LOAD httpfs;
                         CREATE OR REPLACE SECRET ducklocal_s3 (
                             TYPE s3, PROVIDER config,
                             KEY_ID '{}', SECRET '{}', REGION '{}', ENDPOINT '{}'
                         )",
                        esc(&key_id),
                        esc(&secret),
                        esc(&region),
                        esc(&endpoint),
                    ))
                    .map_err(Into::into)
                })
            })
            .await;

            this.update_in(cx, move |_, window, cx| match result {
                Ok(()) => {
                    state.update(cx, |s, cx| {
                        s.set_s3_configured(Some(endpoint_label.clone()), cx);
                    });
                    window.push_notification("S3 已配置，可查询 s3:// 路径", cx);
                }
                Err(e) => {
                    window.push_notification(Notification::error(format!("S3 配置失败：{e}")), cx)
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_db_dialog(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("~/warehouse/analytics.duckdb")
        });
        self.db_path_input = Some(input.clone());
        let view = cx.entity().downgrade();

        window.open_dialog(cx, move |dialog, _, cx| {
            let open_file_view = view.clone();
            let memory_view = view.clone();
            dialog
                .title("打开数据库")
                .w(px(420.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("输入 .duckdb 文件路径，或直接导入 CSV / Parquet / JSON 数据文件为视图。"),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .items_center()
                                .child(div().flex_1().min_w_0().child(Input::new(&input)))
                                .child(
                                    Button::new("browse")
                                        .outline()
                                        .label("浏览…")
                                        .on_click({
                                            let input = input.clone();
                                            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                                let rx = cx.prompt_for_paths(PathPromptOptions {
                                                    files: true,
                                                    directories: false,
                                                    multiple: false,
                                                    prompt: Some("选择数据库或数据文件".into()),
                                                });
                                                let input = input.clone();
                                                window
                                                    .spawn(cx, async move |cx| {
                                                        if let Ok(Ok(Some(paths))) = rx.await {
                                                            if let Some(path) = paths.first() {
                                                                let value = path.to_string_lossy().to_string();
                                                                input
                                                                    .update_in(cx, |state, window, cx| {
                                                                        state.set_value(value, window, cx);
                                                                    })
                                                                    .ok();
                                                            }
                                                        }
                                                    })
                                                    .detach();
                                            }
                                        }),
                                ),
                        ),
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
                            Button::new("memory")
                                .outline()
                                .label("内存模式")
                                .on_click(move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                    window.close_dialog(cx);
                                    if let Some(view) = memory_view.upgrade() {
                                        view.update(cx, |this, cx| {
                                            this.connect(DatabaseTarget::Memory, window, cx);
                                        });
                                    }
                                }),
                        )
                        .child(
                            Button::new("open-file")
                                .primary()
                                .label("打开文件")
                                .on_click({
                                    let input = input.clone();
                                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                        let path = input.read(cx).value().to_string();
                                        if path.trim().is_empty() {
                                            return;
                                        }
                                        window.close_dialog(cx);
                                        if let Some(view) = open_file_view.upgrade() {
                                            view.update(cx, |this, cx| {
                                                if crate::db::is_data_file(&path) {
                                                    this.attach_data(path.clone(), window, cx);
                                                } else {
                                                    this.connect(
                                                        DatabaseTarget::File(path.clone()),
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            });
                                        }
                                    }
                                }),
                        ),
                )
        });
    }

    /// Attach a CSV/Parquet/JSON file as a view in the current connection,
    /// register it for future sessions, then refresh catalog and history.
    fn attach_data(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let attach_path = path.clone();
            let result = smol::unblock(
                move || -> anyhow::Result<(String, Vec<DatabaseInfo>, Vec<HistoryEntry>, Vec<crate::state::AttachedFileView>)> {
                    let view = crate::db::attach_data_file(&attach_path)?;
                    if let Some(kind) = crate::db::data_file_kind(&attach_path) {
                        crate::history::register_attached_file(&attach_path, &view, kind).ok();
                    }
                    let (catalog, history) = crate::state::load_sidebar_data();
                    let attached = crate::state::load_attached_files();
                    Ok((view, catalog, history, attached))
                },
            )
            .await;

            this.update_in(cx, move |_, window, cx| match result {
                Ok((view, catalog, history, attached)) => {
                    state.update(cx, |s, cx| {
                        s.set_catalog(catalog, cx);
                        s.set_history(history, cx);
                        s.set_attached_files(attached, cx);
                    });
                    window.push_notification(format!("已创建视图 {view}"), cx);
                }
                Err(e) => window.push_notification(
                    Notification::error(format!("无法导入数据文件：{e}")),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// Open the target database, re-attach registered data files, then refresh
    /// server info, catalog, and history.
    fn connect(&mut self, target: DatabaseTarget, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        let label = target.display_label();
        cx.spawn_in(window, async move |this, cx| {
            let opened = target.clone();
            let result = smol::unblock(
                move || -> anyhow::Result<(ServerInfo, Vec<DatabaseInfo>, Vec<HistoryEntry>, Vec<crate::state::AttachedFileView>)> {
                    match &opened {
                        DatabaseTarget::File(path) => crate::db::open_file(path)?,
                        DatabaseTarget::Memory => crate::db::open_memory()?,
                    }
                    crate::state::reattach_registered_files();
                    let server = crate::db::server_info(opened)?;
                    let (catalog, history) = crate::state::load_sidebar_data();
                    let attached = crate::state::load_attached_files();
                    Ok((server, catalog, history, attached))
                },
            )
            .await;

            this.update_in(cx, move |_, window, cx| match result {
                Ok((server, catalog, history, attached)) => {
                    state.update(cx, |s, cx| {
                        s.set_connection(target, server, catalog, cx);
                        s.set_history(history, cx);
                        s.set_attached_files(attached, cx);
                    });
                    window.push_notification(format!("已连接到 {label}"), cx);
                }
                Err(e) => {
                    window.push_notification(Notification::error(format!("无法打开数据库：{e}")), cx)
                }
            })
            .ok();
        })
        .detach();
    }
}

impl Render for TitleBarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (target_label, version) = {
            let state = self.state.read(cx);
            (
                state.target.as_ref().map(|t| t.display_label()),
                state.server.as_ref().map(|s| s.version.clone()),
            )
        };
        let dark = cx.theme().mode.is_dark();

        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    Button::new("open-db")
                        .ghost()
                        .xsmall()
                        .icon(IconName::FolderOpen)
                        .label("打开数据库…")
                        .on_click(cx.listener(Self::open_db_dialog)),
                )
                .child(
                    Button::new("configure-s3")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Globe)
                        .label("S3")
                        .tooltip("配置 S3 数据源（httpfs）")
                        .on_click(cx.listener(Self::open_s3_dialog)),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .justify_center()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_sm()
                                .child("DuckLocal"),
                        )
                        .when_some(target_label, |this, label| {
                            this.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(format!("— {label}")),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .when_some(version, |this, version| {
                            this.child(
                                h_flex()
                                    .gap_1p5()
                                    .items_center()
                                    .child(
                                        div()
                                            .size_2()
                                            .rounded_full()
                                            .bg(cx.theme().success),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("DuckDB {version}")),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("·"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("本地"),
                                    ),
                            )
                        })
                        .child(
                            Button::new("toggle-theme")
                                .ghost()
                                .xsmall()
                                .icon(if dark { IconName::Sun } else { IconName::Moon })
                                .tooltip("切换明暗主题")
                                .on_click(Self::toggle_theme),
                        ),
                ),
        )
    }
}

/// Form row: a muted label above the control.
fn labeled_field(label: &'static str, control: AnyElement, cx: &App) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(control)
}
