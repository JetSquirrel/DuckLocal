mod analysis;
mod app;
mod app_export;
mod assets;
mod cli;
mod db;
mod excel;
mod history;
mod i18n;
#[cfg(test)]
mod perf_probe;
mod profile;
mod query;
mod recents;
mod s3;
mod schema;
mod setup;
mod sources;
mod spec;
mod state;
mod ui;

use gpui_kit::component::{Root, Theme, ThemeMode, TitleBar};
use gpui_kit::*;

use crate::app::DuckLocalApp;

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = cli::dispatch(&args) {
        std::process::exit(code);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Resolve the UI language before the first frame: open the history store
    // here (app.rs re-opens it harmlessly later) and read the persisted
    // `language` setting; without one, fall back to the system locale.
    // Nothing is persisted until the user explicitly switches language.
    crate::history::init().ok();
    i18n::set_current(i18n::initial_language());
    ui::scale::load();

    // Paths named on the command line: data files, folders, patterns, or a
    // database to open instead of the in-memory connection. `-psn_…` is what
    // macOS appends when the app is launched from Finder or the Dock. Lossy,
    // like a drop on the window: `std::env::args` panics on a name that is
    // not UTF-8, and a path that cannot be found is an error the UI reports.
    let paths: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .filter(|arg| !arg.starts_with("-psn_"))
        .collect();

    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            ui::init(cx);
            Theme::change(ThemeMode::Light, None, cx);
            // The interface size is the rem base; it must be re-applied after
            // every Theme::change, which resets the theme to stock defaults.
            ui::scale::apply(cx);

            let window_bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            cx.spawn(async move |cx| {
                let options = WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                    window_min_size: Some(size(px(960.), px(600.))),
                    ..TitleBar::window_options()
                };
                cx.open_window(options, |window, cx| {
                    let view = cx.new(|cx| DuckLocalApp::new(paths, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("Failed to open window");
            })
            .detach();
        });
}
