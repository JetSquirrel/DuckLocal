//! Bottom status bar: connection status on the left, last-query stats and the
//! DuckDB version on the right.

use gpui_kit::component::separator::Separator;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, h_flex};
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::state::{AppState, ConnectionChanged, QueryStatsChanged};

pub struct StatusBarView {
    state: Entity<AppState>,
    _subscriptions: Vec<Subscription>,
}

impl StatusBarView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&state, |_, _, _: &ConnectionChanged, cx| cx.notify()),
            cx.subscribe(&state, |_, _, _: &QueryStatsChanged, cx| cx.notify()),
        ];
        Self {
            state,
            _subscriptions: subscriptions,
        }
    }
}

impl Render for StatusBarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (target_label, version, last) = {
            let state = self.state.read(cx);
            (
                state.target.as_ref().map(|t| t.display_label()),
                state.server.as_ref().map(|s| s.version.clone()),
                state.last_query.clone(),
            )
        };

        let mono = cx.theme().mono_font_family.clone();

        let mut bar = StatusBar::new()
            .h(px(32.))
            .left(if let Some(label) = target_label {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(
                    Icon::new(IconName::CircleCheck)
                        .xsmall()
                        .text_color(cx.theme().success),
                )
                .child(tr("status_bar.connected"))
                .child(label)
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

        if let Some(stats) = last {
            bar = bar.right(Separator::vertical()).right(
                div().font_family(mono.clone()).child(trf(
                    "status_bar.last_query",
                    &[
                        &crate::state::format_duration(stats.elapsed_ms as i64),
                        &stats.rows.to_string(),
                        &stats.cols.to_string(),
                    ],
                )),
            );
        }
        if let Some(version) = version {
            bar = bar.right(Separator::vertical()).right(
                div()
                    .font_family(mono.clone())
                    .child(format!("DuckDB {version}")),
            );
        }
        bar
    }
}
