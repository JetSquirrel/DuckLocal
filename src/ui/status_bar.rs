//! Bottom status bar: connection status on the left, last-query stats, the
//! DuckDB version and DuckLocal's own version — the update check — on the
//! right.

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
        let (connected, target_label, storage, version, last, opening, search_path) = {
            let state = self.state.read(cx);
            (
                state.target.is_some(),
                state.target.as_ref().and_then(|t| t.file_label()),
                state.server.as_ref().and_then(storage_note),
                state.server.as_ref().map(|s| s.version.clone()),
                state.last_query.clone(),
                state.is_opening(),
                state.search_path.clone(),
            )
        };

        let mono = cx.theme().mono_font_family.clone();

        let mut bar = StatusBar::new().h(crate::ui::scale::design(32.)).left(if opening {
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
        if let Some(version) = version {
            bar = bar.right(Separator::vertical()).right(
                div()
                    .font_family(mono.clone())
                    .child(format!("DuckDB {version}")),
            );
        }
        bar.right(Separator::vertical()).right(update_status(cx))
    }
}

/// DuckLocal's version, which checks for a newer one when clicked — and,
/// once one is found, a link to its download page instead.
fn update_status(cx: &App) -> AnyElement {
    use crate::ui::update::{self, Status, CURRENT};
    let version = format!("DuckLocal {CURRENT}");
    let tooltip = |text: String| {
        move |window: &mut Window, cx: &mut App| {
            gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
        }
    };
    match update::status(cx) {
        Status::Checking => h_flex()
            .gap_1p5()
            .items_center()
            .child(Spinner::new().xsmall())
            .child(tr("status_bar.update.checking"))
            .into_any_element(),
        Status::Available(tag) => div()
            .id("update-available")
            .cursor_pointer()
            .text_color(cx.theme().primary)
            .font_weight(FontWeight::MEDIUM)
            .child(trf("status_bar.update.available", &[&tag]))
            .tooltip(tooltip(update::RELEASES_URL.to_string()))
            .on_click(|_, _, cx| cx.open_url(update::RELEASES_URL))
            .into_any_element(),
        status => {
            let (label, hint) = match status {
                Status::UpToDate => (
                    format!("{version} · {}", tr("status_bar.update.latest")),
                    tr("status_bar.update.check").to_string(),
                ),
                Status::Failed(error) => (
                    format!("{version} · {}", tr("status_bar.update.failed")),
                    error,
                ),
                _ => (version, tr("status_bar.update.check").to_string()),
            };
            div()
                .id("update-check")
                .cursor_pointer()
                .child(label)
                .tooltip(tooltip(hint))
                .on_click(|_, _, cx| update::check(cx))
                .into_any_element()
        }
    }
}
