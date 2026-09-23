//! The sidebar's row and toolbar actions: reload the schema, remove a
//! registered file, and edit a column's type through a dialog.

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::dialog::{DialogButtonProps, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{v_flex, WindowExt};
use gpui_kit::*;

use super::model::{ColumnRef, FileRef};
use super::sql::alter_column_type_sql;
use super::Sidebar;
use crate::i18n::{tr, trf};

impl Sidebar {
    pub(super) fn refresh_schema(
        &mut self,
        _: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.refreshing_schema {
            return;
        }
        self.refreshing_schema = true;
        cx.notify();
        let state = self.state.clone();
        cx.spawn(async move |this, cx| {
            let (catalog, attached) = smol::unblock(|| {
                (
                    crate::schema::load_catalog().unwrap_or_default(),
                    crate::state::load_attached_files(),
                )
            })
            .await;
            state.update(cx, |s, cx| {
                s.set_catalog(catalog, cx);
                s.set_attached_files(attached, cx);
            });
            this.update(cx, |this, cx| {
                this.refreshing_schema = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn confirm_remove_file(
        &mut self,
        file: FileRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let on_ok_view = view.clone();
            let on_ok_file = file.clone();
            alert
                .title(trf("dialog.remove_file.title", &[&file.view_name]))
                .description(tr("dialog.remove_file.description"))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(tr("dialog.remove_file.confirm"))
                        .ok_variant(ButtonVariant::Danger)
                        .show_cancel(true)
                        .cancel_text(tr("common.cancel"))
                        .on_ok(move |_, window, cx| {
                            if let Some(view) = on_ok_view.upgrade() {
                                view.update(cx, |this, cx| {
                                    this.remove_file(on_ok_file.clone(), window, cx);
                                });
                            }
                            true
                        }),
                )
        });
    }

    /// Remove from the registry and DROP the view, then refresh both lists.
    fn remove_file(&mut self, file: FileRef, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        cx.spawn_in(window, async move |_, cx| {
            let result = smol::unblock(move || -> anyhow::Result<()> {
                crate::history::remove_attached_file(file.id)?;
                crate::db::with_connection(|conn| {
                    let quoted = file.view_name.replace('"', "\"\"");
                    conn.execute_batch(&format!("DROP VIEW IF EXISTS \"{quoted}\""))?;
                    Ok(())
                })?;
                Ok(())
            })
            .await;
            if result.is_ok() {
                let (catalog, attached) = smol::unblock(|| {
                    (
                        crate::schema::load_catalog().unwrap_or_default(),
                        crate::state::load_attached_files(),
                    )
                })
                .await;
                state.update(cx, |s, cx| {
                    s.set_catalog(catalog, cx);
                    s.set_attached_files(attached, cx);
                });
            }
        })
        .detach();
    }

    pub(super) fn open_alter_type_dialog(
        &mut self,
        column: ColumnRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input =
            cx.new(|cx| InputState::new(window, cx).default_value(column.data_type.clone()));
        let view = cx.entity().downgrade();

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(trf("dialog.alter_type.title", &[&column.name]))
                .w(crate::ui::scale::design(360.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_xs().child(trf(
                            "dialog.alter_type.current_type",
                            &[&column.table.name, &column.name, &column.data_type],
                        )))
                        .child(Input::new(&input)),
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
                            Button::new("confirm-alter-type")
                                .primary()
                                .label(tr("dialog.alter_type.confirm"))
                                .on_click({
                                    let input = input.clone();
                                    let view = view.clone();
                                    let column = column.clone();
                                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                        let new_type = input.read(cx).value().trim().to_string();
                                        window.close_dialog(cx);
                                        if new_type.is_empty() {
                                            return;
                                        }
                                        if let Some(view) = view.upgrade() {
                                            view.update(cx, |this, cx| {
                                                this.alter_column_type(
                                                    column.clone(),
                                                    new_type,
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }
                                    }
                                }),
                        ),
                )
        });
    }

    /// Run `ALTER TABLE ... SET DATA TYPE`, then reload the catalog so the
    /// sidebar shows the new type.
    fn alter_column_type(
        &mut self,
        column: ColumnRef,
        new_type: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let sql = alter_column_type_sql(&column, &new_type);
            let result = smol::unblock(move || -> anyhow::Result<()> {
                crate::db::with_connection(|conn| {
                    conn.execute_batch(&sql)?;
                    Ok(())
                })
            })
            .await;
            let outcome = match result {
                Ok(()) => {
                    let catalog =
                        smol::unblock(|| crate::schema::load_catalog().unwrap_or_default()).await;
                    Ok(catalog)
                }
                Err(e) => Err(e),
            };
            this.update_in(cx, move |_, window, cx| match outcome {
                Ok(catalog) => {
                    state.update(cx, |s, cx| s.set_catalog(catalog, cx));
                    window.push_notification(
                        Notification::success(trf(
                            "notify.alter_type.success",
                            &[&column.name, &new_type],
                        )),
                        cx,
                    );
                }
                Err(e) => {
                    window.push_notification(
                        Notification::error(trf("notify.alter_type.failed", &[&e.to_string()])),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }
}
