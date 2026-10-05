//! The sidebar's 扩展 page: every extension DuckDB knows of, whether it is
//! built in, installed or loaded, and buttons to install, load or update it.
//!
//! The list is read on demand — when the page is shown, after each action,
//! and on a new connection while the page is open — never cached past that:
//! a `LOAD` typed in a query tab changes it too.

use std::rc::Rc;

use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use super::Sidebar;
use crate::extensions::{ExtensionAction, ExtensionInfo, Origin};
use crate::i18n::{tr, trf};

/// What the page shows.
pub(super) enum ExtensionList {
    NotLoaded,
    Loading,
    Loaded(Rc<Vec<ExtensionInfo>>),
    Failed(String),
}

impl Sidebar {
    /// Re-read the list from the engine.
    pub(super) fn reload_extensions(&mut self, cx: &mut Context<Self>) {
        if matches!(self.extensions, ExtensionList::Loading) {
            return;
        }
        // Keep showing the old list while the new one loads, so a row's
        // buttons do not flicker away after every action.
        if !matches!(self.extensions, ExtensionList::Loaded(_)) {
            self.extensions = ExtensionList::Loading;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result =
                smol::unblock(|| crate::db::with_connection(crate::extensions::list_of)).await;
            this.update(cx, |this, cx| {
                this.extensions = match result {
                    Ok(list) => ExtensionList::Loaded(Rc::new(list)),
                    Err(e) => ExtensionList::Failed(e.to_string()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn run_extension_action(
        &mut self,
        name: String,
        action: ExtensionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.extension_busy.is_some() {
            return;
        }
        self.extension_busy = Some(name.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let target = name.clone();
            let result = smol::unblock(move || {
                crate::db::with_connection(|conn| {
                    crate::extensions::apply_of(conn, &target, action)
                })
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                this.extension_busy = None;
                let key = match action {
                    ExtensionAction::Install => "sidebar.extensions.installed_notice",
                    ExtensionAction::Load => "sidebar.extensions.loaded_notice",
                    ExtensionAction::Update => "sidebar.extensions.updated_notice",
                };
                match result {
                    Ok(()) => window.push_notification(trf(key, &[&name]), cx),
                    Err(e) => window.push_notification(
                        Notification::error(trf(
                            "sidebar.extensions.failed",
                            &[&name, &e.to_string()],
                        )),
                        cx,
                    ),
                }
                // A loaded extension can bring tables and functions with it.
                if action != ExtensionAction::Install {
                    this.refresh_catalog(cx);
                }
                this.reload_extensions(cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_extensions(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let message = |text: String, cx: &mut Context<Self>| {
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
                        .child(text),
                )
                .into_any_element()
        };
        let list = match &self.extensions {
            ExtensionList::NotLoaded | ExtensionList::Loading => {
                return message(tr("sidebar.extensions.loading").to_string(), cx)
            }
            ExtensionList::Failed(e) => return message(e.clone(), cx),
            ExtensionList::Loaded(list) => list.clone(),
        };
        let busy = self.extension_busy.clone();
        v_flex()
            .id("extension-list")
            .size_full()
            .overflow_y_scroll()
            .children(list.iter().enumerate().map(|(ix, ext)| {
                let is_last = ix + 1 == list.len();
                self.render_extension(ix, ext, is_last, busy.as_deref(), cx)
            }))
            .into_any_element()
    }

    fn render_extension(
        &self,
        ix: usize,
        ext: &ExtensionInfo,
        is_last: bool,
        busy: Option<&str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let status = if ext.loaded {
            Tag::success().child(tr("sidebar.extensions.status.loaded"))
        } else if ext.installed {
            Tag::info().child(tr("sidebar.extensions.status.installed"))
        } else {
            Tag::secondary().child(tr("sidebar.extensions.status.available"))
        };
        let origin = match ext.origin {
            Origin::Builtin => Some(tr("sidebar.extensions.origin.builtin").to_string()),
            Origin::Repository | Origin::Other => ext.installed_from.clone(),
            Origin::NotInstalled => None,
        };
        let meta = [ext.version.clone(), origin]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let this_busy = busy == Some(ext.name.as_str());
        let any_busy = busy.is_some();

        let action_button = |id: &'static str,
                             label: &'static str,
                             action: ExtensionAction,
                             cx: &mut Context<Self>| {
            let name = ext.name.clone();
            Button::new((id, ix))
                .outline()
                .xsmall()
                .label(tr(label))
                .loading(this_busy)
                .disabled(any_busy)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.run_extension_action(name.clone(), action, window, cx);
                }))
        };

        v_flex()
            .id(("extension", ix))
            .w_full()
            .px_3()
            .py_2()
            .gap_1()
            .when(!is_last, |this| this.border_b_1())
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_family(cx.theme().mono_font_family.clone())
                            .truncate()
                            .child(ext.name.clone()),
                    )
                    .child(status.xsmall())
                    .child(div().flex_1())
                    .when(ext.can_install(), |row| {
                        row.child(action_button(
                            "extension-install",
                            "sidebar.extensions.install",
                            ExtensionAction::Install,
                            cx,
                        ))
                    })
                    .when(ext.can_load(), |row| {
                        row.child(action_button(
                            "extension-load",
                            "sidebar.extensions.load",
                            ExtensionAction::Load,
                            cx,
                        ))
                    })
                    .when(ext.can_update(), |row| {
                        row.child(action_button(
                            "extension-update",
                            "sidebar.extensions.update",
                            ExtensionAction::Update,
                            cx,
                        ))
                    }),
            )
            .when_some(ext.description.clone(), |this, description| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .line_clamp(2)
                        .child(description),
                )
            })
            .when(!meta.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .font_family(cx.theme().mono_font_family.clone())
                        .truncate()
                        .child(meta),
                )
            })
    }
}
