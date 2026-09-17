//! Title bar: app name + current database path, server status, theme toggle,
//! and the "打开数据库…" dialog.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputContentType, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme, IconName, Sizable, Theme, ThemeMode, TitleBar, WindowExt,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::{tr, trf, Language};
use crate::state::AppState;

const DIALOG_WIDTH: Pixels = px(440.);

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

    fn toggle_language(_: &ClickEvent, _: &mut Window, cx: &mut App) {
        let next = match crate::i18n::current() {
            Language::Zh => Language::En,
            Language::En => Language::Zh,
        };
        crate::i18n::set_language(next);
        cx.refresh_windows();
    }

    fn open_s3_dialog(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let inputs = S3Inputs {
            endpoint: cx.new(|cx| InputState::new(window, cx).default_value("s3.amazonaws.com")),
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
                .title(tr("dialog.s3.title"))
                .w(DIALOG_WIDTH)
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr("dialog.s3.description")),
                        )
                        .child(
                            v_flex()
                                .gap_2()
                                .child(labeled_field(
                                    tr("dialog.s3.field.endpoint"),
                                    Input::new(&endpoint).into_any_element(),
                                    cx,
                                ))
                                .child(labeled_field(
                                    tr("dialog.s3.field.region"),
                                    Input::new(&region).into_any_element(),
                                    cx,
                                ))
                                .child(labeled_field(
                                    tr("dialog.s3.field.access_key_id"),
                                    Input::new(&key_id).into_any_element(),
                                    cx,
                                ))
                                .child(labeled_field(
                                    tr("dialog.s3.field.secret_access_key"),
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
                                .label(tr("common.cancel"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("save-s3")
                                .primary()
                                .label(tr("dialog.s3.confirm"))
                                .on_click(
                                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                        let endpoint = endpoint.read(cx).value().trim().to_string();
                                        let region = region.read(cx).value().trim().to_string();
                                        let key_id = key_id.read(cx).value().trim().to_string();
                                        let secret = secret.read(cx).value().to_string();
                                        if key_id.is_empty() || secret.is_empty() {
                                            window.push_notification(
                                                Notification::error(tr("notify.s3.missing_keys")),
                                                cx,
                                            );
                                            return;
                                        }
                                        window.close_dialog(cx);
                                        if let Some(view) = confirm_view.upgrade() {
                                            view.update(cx, |this, cx| {
                                                this.configure_s3(
                                                    endpoint, region, key_id, secret, window, cx,
                                                );
                                            });
                                        }
                                    },
                                ),
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
        let config = crate::s3::S3Config {
            endpoint: endpoint.clone(),
            region: region.clone(),
            key_id: key_id.clone(),
            secret: secret.clone(),
        };
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
                        s.set_s3_configured(Some(config.clone()), cx);
                    });
                    window.push_notification(tr("notify.s3.configured"), cx);
                }
                Err(e) => window.push_notification(
                    Notification::error(trf("notify.s3.failed", &[&e.to_string()])),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    fn open_data_dialog(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("~/data/*.parquet"));
        self.db_path_input = Some(input.clone());
        let state = self.state.clone();

        window.open_dialog(cx, move |dialog, _, cx| {
            let memory_state = state.clone();
            let open_state = state.clone();
            dialog
                .title(tr("dialog.open_source.title"))
                .w(DIALOG_WIDTH)
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr("dialog.open_source.description")),
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
                                        .label(tr("dialog.open_source.browse"))
                                        .on_click({
                                            let input = input.clone();
                                            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                                let rx = cx.prompt_for_paths(PathPromptOptions {
                                                    files: true,
                                                    directories: true,
                                                    multiple: false,
                                                    prompt: Some(tr("dialog.open_source.picker_prompt").into()),
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
                                .label(tr("common.cancel"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("memory")
                                .outline()
                                .label(tr("dialog.open_source.memory"))
                                .on_click(move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                    window.close_dialog(cx);
                                    crate::ui::open_memory(memory_state.clone(), window, cx);
                                }),
                        )
                        .child(
                            Button::new("open-file")
                                .primary()
                                .label(tr("dialog.open_source.open_file"))
                                .on_click({
                                    let input = input.clone();
                                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                        let path = input.read(cx).value().trim().to_string();
                                        if path.is_empty() {
                                            return;
                                        }
                                        window.close_dialog(cx);
                                        crate::ui::open_dialog_path(
                                            open_state.clone(),
                                            path,
                                            window,
                                            cx,
                                        );
                                    }
                                }),
                        ),
                )
        });
    }
}

impl Render for TitleBarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target_label = {
            let state = self.state.read(cx);
            state.target.as_ref().map(|t| t.display_label())
        };
        let dark = cx.theme().mode.is_dark();

        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .justify_start()
                        .gap_2()
                        .child(
                            Button::new("open-data")
                                .ghost()
                                .xsmall()
                                .icon(IconName::FolderOpen)
                                .label(tr("title_bar.open_data"))
                                .on_click(cx.listener(Self::open_data_dialog)),
                        )
                        .child(
                            Button::new("configure-s3")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Globe)
                                .label("S3")
                                .tooltip(tr("title_bar.configure_s3"))
                                .on_click(cx.listener(Self::open_s3_dialog)),
                        ),
                )
                .child(
                    h_flex()
                        .min_w_0()
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
                        .flex_1()
                        .min_w_0()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("toggle-language")
                                .ghost()
                                .xsmall()
                                .label(match crate::i18n::current() {
                                    Language::Zh => "EN",
                                    Language::En => "中",
                                })
                                .tooltip(tr("title_bar.toggle_language"))
                                .on_click(Self::toggle_language),
                        )
                        .child(
                            Button::new("toggle-theme")
                                .ghost()
                                .xsmall()
                                .icon(if dark { IconName::Sun } else { IconName::Moon })
                                .tooltip(tr("title_bar.toggle_theme"))
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
