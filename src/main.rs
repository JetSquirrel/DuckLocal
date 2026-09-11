mod app;
mod db;
mod history;
#[cfg(test)]
mod perf_probe;
mod query;
mod schema;
mod state;
mod ui;

use gpui_kit::component::{Root, Theme, ThemeMode, TitleBar};
use gpui_kit::*;

use crate::app::DuckLocalApp;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            ui::init(cx);
            Theme::change(ThemeMode::Light, None, cx);
            // Denser desktop density: 14px rem base (gpui-component ships 16).
            // Must be re-applied after every Theme::change — it rebuilds the
            // theme from stock defaults and resets font_size.
            Theme::global_mut(cx).font_size = px(14.);
            Theme::sync_base(cx);

            let window_bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            cx.spawn(async move |cx| {
                let options = WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                    window_min_size: Some(size(px(960.), px(600.))),
                    ..TitleBar::window_options()
                };
                cx.open_window(options, |window, cx| {
                    let view = cx.new(|cx| DuckLocalApp::new(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("Failed to open window");
            })
            .detach();
        });
}
