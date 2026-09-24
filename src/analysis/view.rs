//! The app view that lives inside a workspace tab.
//!
//! It is an ordinary child view: the workspace renders it in the same region
//! the SQL editor occupies, its toolbar controls are the workspace's, and it
//! closes like any other tab. Nothing here knows about windows.
//!
//! The script runtime is not owned here. One runtime serves every app tab, so
//! the workspace owns it and hands each app a reference; an app that dies
//! takes its mounted view with it and leaves the runtime for the next one.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::*;
use gpui_shell::ShellRuntime;

use crate::analysis::host::{self, Rejection};
use crate::analysis::watch::{AppFiles, Debounce, POLL_INTERVAL};
use crate::i18n::{tr, trf};

/// The app directory, the script view it mounts, and the states around it.
pub struct AnalysisHost {
    /// The app directory, once one has been chosen.
    directory: Option<PathBuf>,
    entry: String,
    /// The mounted script view, once one has loaded.
    mounted: Option<AnyView>,
    /// Why nothing is mounted. Drawn inside the region.
    failure: Option<String>,
    /// Why what is shown is out of date, while a view is still up.
    stale_reason: Option<String>,
    /// Polls the chosen directory. Dropping it ends the watcher, which is how
    /// a directory that is replaced stops being watched; there is none until
    /// a directory has been chosen.
    watcher: Option<Task<()>>,
    /// What the definition view shows: the entry file as last read, or why it
    /// could not be read.
    definition: Option<String>,
    showing_definition: bool,
    /// Whether the user has said this directory's app may run. Until then
    /// nothing is loaded — not even on a file change — and the tab asks; see
    /// [`crate::analysis::apps::is_trusted`] for why.
    trusted: bool,
    /// Declared last on purpose: fields drop in declaration order, and the
    /// mounted view holds QuickJS handles into this runtime, so it has to be
    /// released first.
    runtime: Option<Rc<ShellRuntime>>,
}

impl AnalysisHost {
    /// `runtime` is the workspace's shared one; an error here is the reason the
    /// app cannot run at all, and is shown as such.
    pub fn new(
        directory: PathBuf,
        runtime: Result<Rc<ShellRuntime>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            directory: None,
            entry: host::ENTRY.to_string(),
            mounted: None,
            failure: None,
            stale_reason: None,
            watcher: None,
            definition: None,
            showing_definition: false,
            trusted: false,
            runtime: None,
        };
        match runtime {
            Ok(runtime) => this.runtime = Some(runtime),
            Err(error) => this.failure = Some(error),
        }
        this.show_directory(directory, window, cx);
        this
    }

    /// Whether the tab is showing the app's source instead of the app.
    pub fn is_showing_definition(&self) -> bool {
        self.showing_definition
    }

    /// Swap between the app and its source, reading the source on the way in
    /// so the definition is what is on disk rather than what was on disk when
    /// the app loaded.
    pub fn toggle_definition(&mut self, cx: &mut Context<Self>) {
        self.showing_definition = !self.showing_definition;
        if self.showing_definition {
            self.read_definition();
        }
        cx.notify();
    }

    /// Show `directory`: check it, watch it, and load its app.
    ///
    /// A directory that cannot be an app leaves what is already up alone and
    /// says why, inside this tab.
    pub fn show_directory(
        &mut self,
        directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(rejection) = host::validate_application(&directory) {
            let reason = rejection_reason(&directory, rejection);
            tracing::warn!("Not an analysis app: {reason}");
            if self.mounted.is_some() {
                self.stale_reason = Some(reason);
            } else {
                self.failure = Some(reason);
            }
            cx.notify();
            return;
        }
        self.trusted = crate::analysis::apps::is_trusted(&directory);
        self.directory = Some(directory.clone());
        self.mounted = None;
        self.failure = None;
        self.stale_reason = None;
        tracing::info!("Analysis app directory: {}", directory.display());
        self.watch(directory, window, cx);
        self.load_soon(window, cx);
        cx.notify();
    }

    /// The user said yes: remember it for this folder and run the app. A
    /// history store that cannot be written (a second instance holds it)
    /// still trusts the folder for as long as this tab is open.
    fn trust_and_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(directory) = self.directory.clone() else {
            return;
        };
        if let Err(error) = crate::analysis::apps::trust(&directory) {
            tracing::warn!(
                "Could not remember that {} is trusted: {error}",
                directory.display()
            );
        }
        self.trusted = true;
        self.reload(window, cx);
    }

    /// Mount the app again, the way the toolbar's Refresh does.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.showing_definition {
            self.read_definition();
        }
        self.reload(window, cx);
    }

    /// Load the files as they are now and swap the result in.
    ///
    /// A failure never replaces a working view: the previous one stays up and
    /// the reason appears above it, which is the difference between a typo
    /// while editing and losing the app to it.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(directory) = self.directory.clone() else {
            return;
        };
        if !self.trusted {
            cx.notify();
            return;
        }
        // A directory that is gone is not a broken app: it is an app with no
        // directory, which is the state the tab starts from and can leave by
        // choosing another one.
        if !directory.is_dir() {
            tracing::warn!(
                "The analysis app directory is gone: {}",
                directory.display()
            );
            self.directory = None;
            self.mounted = None;
            self.stale_reason = None;
            self.failure = Some(trf(
                "analysis.rejected.no_longer_there",
                &[&directory.to_string_lossy()],
            ));
            cx.notify();
            return;
        }
        match self.load(&directory, window, cx) {
            Ok(view) => {
                tracing::debug!("Mounted the analysis app from {}", directory.display());
                self.mounted = Some(view);
                self.failure = None;
                self.stale_reason = None;
                if self.showing_definition {
                    self.read_definition();
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::warn!(
                    "The analysis app in {} did not load: {message}",
                    directory.display()
                );
                if self.mounted.is_some() {
                    self.stale_reason = Some(message.clone());
                } else {
                    self.failure = Some(message);
                }
            }
        }
        cx.notify();
    }

    fn load(&self, directory: &Path, window: &mut Window, cx: &mut App) -> anyhow::Result<AnyView> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(tr("analysis.no_runtime")))?;
        let application = runtime.load_application(directory, &self.entry)?;
        // The directory is visible to the host module for this call and for
        // every host call the script's `init` makes inside it, which is where
        // an app asks for `appDir()`.
        let view = host::with_panel_directory(directory, || {
            runtime.mount_application(&application, window, cx)
        })?;
        Ok(view.into())
    }

    /// The first frame belongs to the tab, not to the script: mounting
    /// compiles the module graph synchronously, so it waits until the loading
    /// state has been painted rather than holding the frame that shows it.
    fn load_soon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            smol::Timer::after(Duration::from_millis(1)).await;
            this.update_in(cx, |this, window, cx| this.reload(window, cx))
                .ok();
        })
        .detach();
    }

    /// Poll `directory` for a change and reload when it settles.
    ///
    /// One watcher exists at a time: storing the new task drops the old one,
    /// which cancels it, so the directory that was replaced stops being
    /// watched at the moment it stops being shown.
    fn watch(&mut self, directory: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let watched = directory.clone();
        // The walk runs off the UI thread: it is a `read_dir` per folder and a
        // `stat` per script, four times a second, and an app folder that
        // sits next to a few thousand data files would otherwise cost the
        // UI thread that many syscalls per tick.
        let capture = move |root: PathBuf| smol::unblock(move || (AppFiles::capture(&root), root));
        let watcher = cx.spawn_in(window, async move |this, cx| {
            let (mut stamp, mut watched) = capture(watched).await;
            let mut debounce = Debounce::new();
            loop {
                smol::Timer::after(POLL_INTERVAL).await;
                let next;
                (next, watched) = capture(watched).await;
                let changed = next != stamp;
                let files = next.file_count();
                stamp = next;
                if !debounce.observe(Instant::now(), changed) {
                    continue;
                }
                tracing::info!(
                    "Reloading the analysis app in {} ({files} JavaScript files)",
                    watched.display()
                );
                if this
                    .update_in(cx, |this, window, cx| this.reload(window, cx))
                    .is_err()
                {
                    // The tab is gone, so there is nothing to reload into.
                    break;
                }
            }
        });
        self.watcher = Some(watcher);
    }

    /// Read the entry file into the definition view.
    fn read_definition(&mut self) {
        let Some(directory) = self.directory.as_deref() else {
            self.definition = None;
            return;
        };
        let path = directory.join(&self.entry);
        self.definition = Some(match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => trf(
                "analysis.definition.unreadable",
                &[&path.to_string_lossy(), &error.to_string()],
            ),
        });
    }

    fn render_stale_warning(&self, cx: &App) -> Option<impl IntoElement> {
        let reason = self.stale_reason.clone()?;
        Some(
            v_flex()
                .flex_none()
                .gap_1()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().danger.alpha(0.12))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr("analysis.not_updated")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(reason),
                ),
        )
    }

    /// The artifact itself: where it lives, and the JavaScript in it.
    fn render_definition(&self, cx: &mut Context<Self>) -> AnyElement {
        let directory = self
            .directory
            .as_deref()
            .map(|directory| directory.to_string_lossy().to_string())
            .unwrap_or_default();
        let path = self
            .directory
            .as_deref()
            .map(|directory| directory.join(&self.entry).to_string_lossy().to_string())
            .unwrap_or_default();
        let source = self.definition.clone().unwrap_or_default();

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr("analysis.definition.hint")),
                    )
                    .child(
                        Button::new("app-back")
                            .outline()
                            .xsmall()
                            .label(tr("analysis.definition.back"))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_definition(cx))),
                    ),
            )
            .child(
                v_flex()
                    .flex_none()
                    .gap_1()
                    .px_3()
                    .pb_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(directory),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(path),
                    ),
            )
            .child(
                v_flex()
                    .id("app-definition")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .pb_3()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .children(source.lines().map(|line| div().child(line.to_string()))),
            )
            .into_any_element()
    }

    /// Nothing is mounted and no directory is chosen: the tab's own empty
    /// state, whose one action is the step that starts the work.
    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let reason = self.failure.clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(
                Icon::new(IconName::FolderOpen)
                    .large()
                    // Decoration, not data: deliberately faded.
                    .text_color(cx.theme().muted_foreground.alpha(0.5)),
            )
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(tr("analysis.empty.title")),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .max_w_96()
                    .text_center()
                    .child(tr("analysis.empty.hint")),
            )
            .children(reason.map(|reason| {
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .max_w_96()
                    .text_center()
                    .child(reason)
            }))
            .into_any_element()
    }

    /// A directory was chosen and its app did not load. The tab's toolbar
    /// still offers Refresh, so there is nothing to add here but the reason.
    fn render_failure(&self, cx: &App) -> AnyElement {
        let message = self.failure.clone().unwrap_or_default();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(
                Icon::new(IconName::CircleX)
                    .large()
                    .text_color(cx.theme().danger),
            )
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(tr("analysis.load_failed")),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .max_w_96()
                    .text_center()
                    .child(tr("analysis.load_failed.hint")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .max_w_96()
                    .text_center()
                    .child(message),
            )
            .into_any_element()
    }

    /// The folder's app has not been agreed to: say what running it allows,
    /// offer its source to read first, and run it only on a click.
    fn render_untrusted(&self, cx: &mut Context<Self>) -> AnyElement {
        let directory = self
            .directory
            .as_deref()
            .map(|directory| directory.to_string_lossy().to_string())
            .unwrap_or_default();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(
                Icon::new(IconName::TriangleAlert)
                    .large()
                    .text_color(cx.theme().warning),
            )
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(tr("analysis.trust.title")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .max_w_96()
                    .text_center()
                    .child(directory),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .max_w_96()
                    .text_center()
                    .child(tr("analysis.trust.body")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("app-view-source")
                            .outline()
                            .small()
                            .label(tr("analysis.trust.view_source"))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_definition(cx))),
                    )
                    .child(
                        Button::new("app-trust")
                            .primary()
                            .small()
                            .label(tr("analysis.trust.run"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.trust_and_run(window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_loading(&self, cx: &App) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                Spinner::new()
                    .large()
                    .color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("analysis.loading")),
            )
            .into_any_element()
    }
}

/// Which of the four bodies is drawn.
///
/// Its own function because the difference between two of them is invisible
/// until it is wrong: a directory that has been chosen and has not mounted yet
/// is *loading*, and drawing the empty state in that moment shows "no app
/// open" over an app that is about to appear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Body {
    /// An app is mounted.
    App,
    /// A directory was chosen and its app is being mounted.
    Loading,
    /// A directory was chosen whose app the user has not yet agreed to run.
    Untrusted,
    /// A directory was chosen and its app did not load.
    Failed,
    /// No directory is chosen.
    Empty,
}

fn body(mounted: bool, directory: Option<&Path>, failure: Option<&str>, trusted: bool) -> Body {
    if mounted {
        Body::App
    } else if directory.is_none() {
        // A rejected or vanished directory leaves this state, with the reason
        // shown: the step that starts the work has to stay available.
        Body::Empty
    } else if !trusted {
        Body::Untrusted
    } else if failure.is_some() {
        Body::Failed
    } else {
        Body::Loading
    }
}

impl Render for AnalysisHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.showing_definition {
            return v_flex()
                .size_full()
                .bg(cx.theme().background)
                .text_color(cx.theme().foreground)
                .children(self.render_stale_warning(cx))
                .child(self.render_definition(cx));
        }

        let region = match body(
            self.mounted.is_some(),
            self.directory.as_deref(),
            self.failure.as_deref(),
            self.trusted,
        ) {
            Body::App => self
                .mounted
                .clone()
                .map(|view| {
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(view)
                        .into_any_element()
                })
                .unwrap_or_else(|| self.render_empty(cx)),
            Body::Loading => self.render_loading(cx),
            Body::Untrusted => self.render_untrusted(cx),
            Body::Failed => self.render_failure(cx),
            Body::Empty => self.render_empty(cx),
        };

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .children(self.render_stale_warning(cx))
            .child(region)
    }
}

/// Why a directory cannot be an app, in the reader's language.
fn rejection_reason(directory: &Path, rejection: Rejection) -> String {
    let display = directory.display().to_string();
    match rejection {
        Rejection::NotADirectory => trf("analysis.rejected.not_a_folder", &[&display]),
        Rejection::NoEntryFile => trf("analysis.rejected.no_entry", &[&display]),
    }
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that pulls in `gpui_kit::*`, whose
    // `test` macro shadows the built-in `#[test]`.
    use super::{body, rejection_reason, Body};
    use crate::analysis::host::{self, Rejection};
    use std::path::Path;

    #[test]
    fn a_chosen_directory_that_has_not_mounted_is_loading_not_empty() {
        let directory = Path::new("/apps/sales");
        assert_eq!(body(false, Some(directory), None, true), Body::Loading);
        // The empty state belongs to the tab that has no directory at all.
        assert_eq!(body(false, None, None, true), Body::Empty);
    }

    #[test]
    fn a_rejected_directory_stays_empty_so_its_action_stays_available() {
        let reason = "no main.js";
        assert_eq!(body(false, None, Some(reason), true), Body::Empty);
        assert_eq!(
            body(false, Some(Path::new("/apps/sales")), Some(reason), true),
            Body::Failed
        );
    }

    #[test]
    fn a_mounted_app_wins_over_a_failure_from_the_next_reload() {
        // A reload that failed keeps the app up; the reason is drawn above
        // it, not instead of it.
        assert_eq!(
            body(true, Some(Path::new("/apps/sales")), Some("boom"), true),
            Body::App
        );
    }

    #[test]
    fn an_untrusted_app_asks_before_it_loads_or_fails() {
        let directory = Path::new("/apps/sales");
        assert_eq!(body(false, Some(directory), None, false), Body::Untrusted);
        assert_eq!(body(false, Some(directory), Some("boom"), false), Body::Untrusted);
        // With no directory there is nothing to trust yet.
        assert_eq!(body(false, None, None, false), Body::Empty);
    }

    #[test]
    fn a_rejection_says_which_directory_and_what_is_missing() {
        let path = Path::new("/apps/missing");
        let message = rejection_reason(path, Rejection::NoEntryFile);
        assert!(message.contains("/apps/missing"), "{message}");
        assert!(message.contains(host::ENTRY), "{message}");

        let message = rejection_reason(path, Rejection::NotADirectory);
        assert!(message.contains("/apps/missing"), "{message}");
        assert_ne!(message, rejection_reason(path, Rejection::NoEntryFile));
    }
}
