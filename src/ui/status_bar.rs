//! Bottom status bar: connection status on the left, last-query stats and
//! DuckLocal's own version — the update check — on the right.

use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::separator::Separator;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::{h_flex, ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::state::{AppState, ConnectionChanged, OpenStateChanged, QueryStatsChanged};

pub struct StatusBarView {
    state: Entity<AppState>,
    _subscriptions: Vec<Subscription>,
}

impl StatusBarView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&state, |_, _, _: &ConnectionChanged, cx| cx.notify()),
            cx.subscribe(&state, |_, _, _: &QueryStatsChanged, cx| cx.notify()),
            cx.subscribe(&state, |_, _, _: &OpenStateChanged, cx| cx.notify()),
        ];
        Self {
            state,
            _subscriptions: subscriptions,
        }
    }
}

/// The open file's storage format and creator, for the file label's tooltip.
fn storage_note(server: &crate::db::ServerInfo) -> Option<String> {
    let storage = server.storage.as_ref()?;
    let mut note = trf("storage.tooltip", &[storage]);
    if let Some(created_by) = &server.created_by {
        note.push('\n');
        note.push_str(&trf("storage.tooltip.created_by", &[created_by]));
    }
    Some(note)
}

impl Render for StatusBarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (connected, target_label, storage, last, opening, search_path) = {
            let state = self.state.read(cx);
            (
                state.target.is_some(),
                state.target.as_ref().and_then(|t| t.file_label()),
                state.server.as_ref().and_then(storage_note),
                state.last_query.clone(),
                state.is_opening(),
                state.search_path.clone(),
            )
        };

        let mono = cx.theme().mono_font_family.clone();

        let mut bar = StatusBar::new()
            .h(crate::ui::scale::design(32.))
            .left(if opening {
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(Spinner::new().xsmall())
                    .child(tr("status_bar.opening"))
                    .into_any_element()
            } else if connected {
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(IconName::CircleCheck)
                            .xsmall()
                            .text_color(cx.theme().success),
                    )
                    .child(tr("status_bar.connected"))
                    .children(target_label.map(|label| {
                        // Who else can read this file: what decides whether
                        // it can be handed to a colleague on an older DuckDB.
                        div()
                            .id("database-file")
                            .child(label)
                            .when_some(storage, |this, note| {
                                this.tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(note.clone())
                                        .build(window, cx)
                                })
                            })
                    }))
                    .into_any_element()
            } else {
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(IconName::CircleX)
                            .xsmall()
                            .text_color(cx.theme().danger),
                    )
                    .child(tr("status_bar.disconnected"))
                    .into_any_element()
            });

        // Where an unqualified `stations` resolves: what `USE` changed.
        if let Some(path) = search_path {
            bar = bar.right(Separator::vertical()).right(
                div()
                    .id("search-path")
                    .font_family(mono.clone())
                    .child(format!("{}.{}", path.database, path.schema))
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tr("status_bar.search_path"))
                            .build(window, cx)
                    }),
            );
        }
        if let Some(stats) = last {
            bar = bar
                .right(Separator::vertical())
                .right(div().font_family(mono.clone()).child(trf(
                    "status_bar.last_query",
                    &[
                        &crate::state::format_duration(stats.elapsed_ms as i64),
                        &stats.rows.to_string(),
                        &stats.cols.to_string(),
                    ],
                )));
        }
        bar.right(Separator::vertical()).right(update_status(cx))
    }
}

/// Progress text for `Status::Downloading`: a percentage when the total size
/// is known, megabytes read when it is not, or a plain ellipsis before the
/// first bytes arrive.
fn downloading_label(tag: &str, downloaded: u64, total: Option<u64>) -> String {
    downloading_label_in(crate::i18n::current(), tag, downloaded, total)
}

fn downloading_label_in(
    lang: crate::i18n::Language,
    tag: &str,
    downloaded: u64,
    total: Option<u64>,
) -> String {
    if let Some(total) = total.filter(|&t| t > 0) {
        let percent = ((downloaded as u128 * 100) / total as u128).min(100);
        crate::i18n::trf_in(
            lang,
            "status_bar.update.downloading_progress",
            &[tag, &format!("{percent}%")],
        )
    } else if downloaded > 0 {
        let mb = downloaded as f64 / (1024.0 * 1024.0);
        crate::i18n::trf_in(
            lang,
            "status_bar.update.downloading_progress",
            &[tag, &format!("{mb:.1} MB")],
        )
    } else {
        crate::i18n::trf_in(lang, "status_bar.update.downloading", &[tag])
    }
}

/// Context menu for the status bar's version item. Windows and Linux have no
/// menu bar, so update controls, the guide and the logs folder the DuckLocal
/// menu offers on macOS are reachable from here too.
fn version_context_menu(menu: PopupMenu, _: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let at_launch = crate::ui::update::at_launch(cx);
    menu.menu(
        tr("menu.check_updates"),
        Box::new(crate::ui::CheckForUpdates),
    )
    .item(
        PopupMenuItem::new(tr("menu.check_updates_at_launch"))
            .checked(at_launch)
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(crate::ui::ToggleUpdateCheck), cx);
            }),
    )
    .separator()
    .menu(tr("menu.show_tour"), Box::new(crate::ui::ShowTour))
    .menu(tr("menu.open_logs"), Box::new(crate::ui::OpenLogs))
}

fn render_version_button(label: String, hint: String) -> AnyElement {
    div()
        .id("update-check")
        .cursor_pointer()
        .child(label)
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx)
        })
        .on_click(|_, _, cx| crate::ui::update::check(cx))
        .context_menu(version_context_menu)
        .into_any_element()
}

/// DuckLocal's version, which checks for a newer one when clicked — and,
/// once one is found, either tracks its download and restart or links to its
/// download page when this copy cannot replace itself.
fn update_status(cx: &App) -> AnyElement {
    update_status_for(crate::ui::update::status(cx), cx)
}

fn update_status_for(status: crate::ui::update::Status, cx: &App) -> AnyElement {
    use crate::ui::update::{self, Status, CURRENT};
    let version = format!("DuckLocal {CURRENT}");
    let tooltip = |text: String| {
        move |window: &mut Window, cx: &mut App| {
            gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
        }
    };
    match status {
        Status::Checking => h_flex()
            .id("update-checking")
            .gap_1p5()
            .items_center()
            .child(Spinner::new().xsmall())
            .child(tr("status_bar.update.checking"))
            .into_any_element(),
        Status::Downloading {
            tag,
            downloaded,
            total,
        } => h_flex()
            .id("update-downloading")
            .gap_1p5()
            .items_center()
            .child(Spinner::new().xsmall())
            .child(downloading_label(&tag, downloaded, total))
            .into_any_element(),
        Status::Installing(tag) => h_flex()
            .id("update-installing")
            .gap_1p5()
            .items_center()
            .child(Spinner::new().xsmall())
            .child(trf("status_bar.update.installing", &[&tag]))
            .into_any_element(),
        Status::Ready(tag) => div()
            .id("update-ready")
            .cursor_pointer()
            .text_color(cx.theme().primary)
            .font_weight(FontWeight::MEDIUM)
            .child(trf("status_bar.update.restart", &[&tag]))
            .tooltip(tooltip(tr("status_bar.update.restart_hint").to_string()))
            .on_click(|_, _, cx| update::restart(cx))
            .context_menu(version_context_menu)
            .into_any_element(),
        Status::Available { tag, url } => div()
            .id("update-available")
            .text_color(cx.theme().primary)
            .font_weight(FontWeight::MEDIUM)
            .child(trf("status_bar.update.available", &[&tag]))
            .when_some(url, |this, url| {
                this.cursor_pointer()
                    .tooltip(tooltip(url.clone()))
                    .on_click(move |_, _, cx| cx.open_url(&url))
            })
            .context_menu(version_context_menu)
            .into_any_element(),
        Status::Idle => render_version_button(version, tr("status_bar.update.check").to_string()),
        Status::UpToDate => render_version_button(
            format!(
                "{version}{}{}",
                crate::i18n::sep(),
                tr("status_bar.update.latest")
            ),
            tr("status_bar.update.check").to_string(),
        ),
        Status::Failed(error) => render_version_button(
            format!(
                "{version}{}{}",
                crate::i18n::sep(),
                tr("status_bar.update.failed")
            ),
            error,
        ),
    }
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*` or `use gpui_kit::*`: that pulls in
    // gpui's `test` macro, which shadows the built-in `#[test]`.
    use gpui_kit::component::{Theme, ThemeMode};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        px, size, AppContext, Bounds, Context, IntoElement, Point, Render, TestAppContext, Window,
        WindowBounds, WindowOptions,
    };

    use super::{downloading_label_in, update_status_for};
    use crate::i18n::{trf_in, Language};
    use crate::ui::update::Status;

    #[test]
    fn downloading_label_formats_percentage_clamped() {
        assert_eq!(
            downloading_label_in(Language::En, "v0.7.0", 0, Some(1000)),
            "Downloading v0.7.0 (0%)"
        );
        assert_eq!(
            downloading_label_in(Language::En, "v0.7.0", 420, Some(1000)),
            "Downloading v0.7.0 (42%)"
        );
        assert_eq!(
            downloading_label_in(Language::Zh, "v0.7.0", 420, Some(1000)),
            "正在下载 v0.7.0 (42%)"
        );
        assert_eq!(
            downloading_label_in(Language::En, "v0.7.0", 1500, Some(1000)),
            "Downloading v0.7.0 (100%)"
        );
        assert_eq!(
            downloading_label_in(Language::En, "v0.7.0", u64::MAX, Some(u64::MAX)),
            "Downloading v0.7.0 (100%)"
        );
    }

    #[test]
    fn downloading_label_formats_megabytes_when_total_unknown() {
        let five_mb = 5 * 1024 * 1024;
        assert_eq!(
            downloading_label_in(Language::En, "v0.7.0", five_mb, None),
            "Downloading v0.7.0 (5.0 MB)"
        );
        assert_eq!(
            downloading_label_in(Language::Zh, "v0.7.0", five_mb, Some(0)),
            "正在下载 v0.7.0 (5.0 MB)"
        );
        assert_eq!(
            downloading_label_in(Language::En, "v0.7.0", 0, None),
            "Downloading v0.7.0…"
        );
        assert_eq!(
            downloading_label_in(Language::Zh, "v0.7.0", 0, Some(0)),
            "正在下载 v0.7.0…"
        );
    }

    #[test]
    fn update_strings_exist_in_both_languages() {
        for key in [
            "status_bar.update.downloading",
            "status_bar.update.downloading_progress",
            "status_bar.update.installing",
            "status_bar.update.restart",
            "status_bar.update.restart_hint",
            "status_bar.update.failed",
            "notify.update.ready.title",
            "notify.update.ready",
            "notify.update.restart_now",
            "notify.update.failed.title",
            "notify.update.failed",
        ] {
            for lang in [Language::En, Language::Zh] {
                assert_ne!(trf_in(lang, key, &["v0.7.0", "42%"]), key, "{key}");
            }
        }
    }

    struct StatusHarness {
        status: Status,
    }

    impl Render for StatusHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            update_status_for(self.status.clone(), cx)
        }
    }

    #[gpui_kit::test]
    fn update_status_renders_downloading_installing_and_ready(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Light, None, cx);
        });
        let (window, harness) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: Point::default(),
                            size: size(px(480.), px(80.)),
                        })),
                        ..Default::default()
                    },
                    cx,
                    |_, cx| {
                        cx.new(|_| StatusHarness {
                            status: Status::Idle,
                        })
                    },
                )
            })
            .expect("open test window");

        let cases = [
            Status::Idle,
            Status::Checking,
            Status::Downloading {
                tag: "v0.7.0".into(),
                downloaded: 420,
                total: Some(1000),
            },
            Status::Installing("v0.7.0".into()),
            Status::Ready("v0.7.0".into()),
            Status::Available {
                tag: "v0.7.0".into(),
                url: Some("https://dl.ducklocal.app/v0.7.0/ducklocal-windows-x86_64.zip".into()),
            },
            Status::UpToDate,
            Status::Failed("network error".into()),
        ];

        for status in cases {
            cx.update_window(window, |_, window, cx| {
                harness.update(cx, |this, cx| {
                    this.status = status;
                    cx.notify();
                });
                window.render_frame(cx);
            })
            .unwrap();
        }
    }

    #[gpui_kit::test]
    fn update_notifications_noop_without_window(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::notify_update_ready("v0.7.0".into(), cx);
            crate::ui::notify_update_failed("install failed".into(), cx);
        });
        cx.run_until_parked();
    }

    #[gpui_kit::test]
    fn update_notifications_render_in_open_window(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Light, None, cx);
        });
        let (window, _harness) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: Point::default(),
                            size: size(px(640.), px(240.)),
                        })),
                        ..Default::default()
                    },
                    cx,
                    |_, cx| {
                        cx.new(|_| StatusHarness {
                            status: Status::Ready("v0.7.0".into()),
                        })
                    },
                )
            })
            .expect("open test window");

        cx.update(|cx| {
            crate::ui::notify_update_ready("v0.7.0".into(), cx);
            crate::ui::notify_update_failed("disk full".into(), cx);
        });
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
    }
}
