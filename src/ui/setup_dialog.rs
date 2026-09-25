//! The "Command line & AI" dialog: put `ducklocal` on the PATH, and hand
//! AI agents what they need to use it. The work lives in [`crate::setup`].

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::setup::{self, CommandStatus, SkillStatus};

const DIALOG_WIDTH: f32 = 520.;

/// Read where things stand (off the UI thread: on Windows it asks
/// PowerShell), then open the dialog.
pub fn open(window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            let (command, skill) =
                smol::unblock(|| (setup::command_status(), setup::claude_skill_status())).await;
            cx.update(move |window, cx| show(command, skill, window, cx))
                .ok();
        })
        .detach();
}

fn show(command: CommandStatus, skill: SkillStatus, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let command_done = matches!(command, CommandStatus::Installed(_));
        let skill_done = matches!(skill, SkillStatus::Current(_));
        let (command_status, command_ok): (SharedString, bool) = match &command {
            CommandStatus::Installed(path) => (
                trf("setup.command.installed", &[&path.display().to_string()]).into(),
                true,
            ),
            CommandStatus::Elsewhere(path) => (
                trf("setup.command.elsewhere", &[&path.display().to_string()]).into(),
                false,
            ),
            CommandStatus::NotInstalled => (tr("setup.command.not_installed").into(), false),
        };
        let (skill_status, skill_ok) = match &skill {
            SkillStatus::Current(_) => (tr("setup.ai.claude.installed"), true),
            SkillStatus::Outdated(_) => (tr("setup.ai.claude.outdated"), false),
            SkillStatus::NotInstalled => (tr("setup.ai.claude.missing"), false),
        };

        dialog
            .title(tr("setup.title"))
            .w(crate::ui::scale::design(DIALOG_WIDTH))
            .child(
                v_flex()
                    .gap_4()
                    .child(
                        section(
                            tr("setup.command.heading"),
                            tr("setup.command.description"),
                            cx,
                        )
                        .child(row(
                            status(command_status, command_ok, cx),
                            // The first step not yet done is the one
                            // primary action in the dialog.
                            Some(
                                Button::new("install-command")
                                    .small()
                                    .map(|b| {
                                        if command_done {
                                            b.outline()
                                        } else {
                                            b.primary()
                                        }
                                    })
                                    .label(if command_done {
                                        tr("setup.command.reinstall")
                                    } else {
                                        tr("setup.command.install")
                                    })
                                    .on_click(|_, window, cx| {
                                        window.close_dialog(cx);
                                        install_command(window, cx);
                                    }),
                            ),
                        )),
                    )
                    .child(div().h_px().bg(cx.theme().border))
                    .child(
                        section(tr("setup.ai.heading"), tr("setup.ai.description"), cx)
                            .child(row(
                                v_flex()
                                    .child(div().text_sm().child(tr("setup.ai.claude")))
                                    .child(status(skill_status.into(), skill_ok, cx)),
                                // Nothing to do when the skill is current;
                                // the check beside it says so.
                                (!skill_done).then(|| {
                                    Button::new("install-skill")
                                        .small()
                                        .map(|b| {
                                            if command_done {
                                                b.primary()
                                            } else {
                                                b.outline()
                                            }
                                        })
                                        .label(match skill {
                                            SkillStatus::NotInstalled => {
                                                tr("setup.ai.claude.install")
                                            }
                                            _ => tr("setup.ai.claude.update"),
                                        })
                                        .on_click(|_, window, cx| {
                                            window.close_dialog(cx);
                                            install_skill(window, cx);
                                        })
                                }),
                            ))
                            .child(row(
                                v_flex()
                                    .child(div().text_sm().child(tr("setup.ai.other")))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(tr("setup.ai.other.description")),
                                    ),
                                Some(
                                    Button::new("copy-brief")
                                        .small()
                                        .outline()
                                        .icon(IconName::Copy)
                                        .label(tr("setup.ai.other.copy"))
                                        .on_click(|_, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                setup::agent_prompt(),
                                            ));
                                            window.push_notification(
                                                Notification::success(tr(
                                                    "notify.setup.prompt_copied",
                                                )),
                                                cx,
                                            );
                                        }),
                                ),
                            )),
                    ),
            )
    });
}

/// A section: its title, a muted line saying what it is for, then its rows.
fn section(title: &'static str, description: &'static str, cx: &App) -> Div {
    v_flex().gap_3().child(
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(description),
            ),
    )
}

/// What a thing is on the left, the action for it (if any) on the right.
fn row(label: impl IntoElement, action: Option<impl IntoElement>) -> impl IntoElement {
    h_flex()
        .gap_3()
        .items_center()
        .justify_between()
        .child(div().min_w_0().flex_1().child(label))
        .children(action.map(|action| div().flex_none().child(action)))
}

/// A status line: a check when done, so the state is not carried by the
/// text color alone.
fn status(text: SharedString, ok: bool, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_1()
        .items_center()
        .text_xs()
        .text_color(if ok {
            cx.theme().success
        } else {
            cx.theme().muted_foreground
        })
        .when(ok, |this| {
            this.child(Icon::new(IconName::CircleCheck).xsmall())
        })
        .child(div().min_w_0().truncate().child(text))
}

fn install_command(window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            let result = smol::unblock(setup::install_command).await;
            cx.update(move |window, cx| {
                let notification = match result {
                    Ok(installed) if installed.needs_path => Notification::warning(trf(
                        "notify.setup.command_needs_path",
                        &[&installed.path.display().to_string()],
                    )),
                    Ok(_) => Notification::success(tr("notify.setup.command_installed")),
                    Err(e) => {
                        Notification::error(trf("notify.setup.command_failed", &[&e.to_string()]))
                    }
                };
                window.push_notification(notification, cx);
            })
            .ok();
        })
        .detach();
}

fn install_skill(window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            let result = smol::unblock(setup::install_claude_skill).await;
            cx.update(move |window, cx| {
                let notification = match result {
                    Ok(dir) => Notification::success(trf(
                        "notify.setup.skill_installed",
                        &[&dir.display().to_string()],
                    )),
                    Err(e) => {
                        Notification::error(trf("notify.setup.skill_failed", &[&e.to_string()]))
                    }
                };
                window.push_notification(notification, cx);
            })
            .ok();
        })
        .detach();
}
