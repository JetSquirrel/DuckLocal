//! The card a plot's comment button opens: the plot quoted at the top, the
//! threads already on it, and a box to write in — the shape of a comment in
//! a document editor, floating over the dashboard rather than taking the
//! plot's place.
//!
//! The card holds only what is being written; the threads themselves are the
//! dashboard's (read from the comments file, see `comments`), so a reply an
//! agent writes shows up here while the card is open.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::spec::comments::{Author, Thread};
use crate::spec::view::Dashboard;

/// How wide the card is: room for a sentence a line, as a margin comment has.
const CARD_WIDTH: f32 = 340.;
/// The thread list scrolls past this, so a long discussion never pushes the
/// box off the screen.
const THREADS_MAX_HEIGHT: f32 = 300.;

pub struct CommentCard {
    dashboard: WeakEntity<Dashboard>,
    plot: String,
    input: Entity<TextareaState>,
    /// The thread the box answers; none starts a new one.
    replying: Option<String>,
    _subscription: Subscription,
}

impl CommentCard {
    pub fn new(
        dashboard: WeakEntity<Dashboard>,
        plot: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder(tr("dashboard.comments.placeholder"))
        });
        // ⌘↵ / Ctrl+↵ posts; a plain Enter is a new line, as in any
        // multi-line comment box.
        let subscription = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { secondary: true, .. } = event {
                this.post(window, cx);
            }
        });
        Self {
            dashboard,
            plot,
            input,
            replying: None,
            _subscription: subscription,
        }
    }

    /// Put the cursor in the box: the card has just opened.
    pub fn focus(card: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let input = card.read(cx).input.clone();
        input.update(cx, |state, cx| state.focus(window, cx));
    }

    fn post(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let (plot, replying) = (self.plot.clone(), self.replying.clone());
        let posted = self
            .dashboard
            .update(cx, |dashboard, cx| {
                dashboard.post_comment(&plot, replying.as_deref(), &text, cx)
            })
            .unwrap_or(false);
        if posted {
            self.replying = None;
            self.input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replying = None;
        self.input.update(cx, |state, cx| state.set_value("", window, cx));
        self.dashboard
            .update(cx, |dashboard, cx| dashboard.close_comments(cx))
            .ok();
    }

    fn render_thread(&self, thread: &Thread, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, primary) = (theme.muted_foreground, theme.primary);
        let id = thread.id.clone();
        let resolved = thread.resolved;
        let picked = self.replying.as_deref() == Some(id.as_str());
        let comments = thread
            .comments
            .iter()
            .map(|comment| {
                let agent = comment.author == Author::Agent;
                let who = if agent {
                    tr("dashboard.comments.agent")
                } else {
                    tr("dashboard.comments.you")
                };
                // `2026-10-10T14:03:00+08:00` reads as `10-10 14:03`.
                let at = comment
                    .at
                    .get(5..16)
                    .unwrap_or(&comment.at)
                    .replace('T', " ");
                v_flex()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .text_xs()
                            .child(
                                div()
                                    .size_4()
                                    .flex_none()
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(9.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.primary_foreground)
                                    .bg(if agent { primary } else { theme.success })
                                    .child(if agent { "AI" } else { "U" }),
                            )
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(who))
                            .child(div().text_color(muted).child(at)),
                    )
                    .child(div().pl(px(22.)).text_sm().child(comment.text.clone()))
            })
            .collect::<Vec<_>>();
        let reply_id = id.clone();
        let resolve_id = id.clone();
        v_flex()
            .gap_2()
            .px_2()
            .py_2()
            .rounded(theme.radius)
            .when(picked, |this| this.bg(theme.accent))
            .when(resolved, |this| this.opacity(0.55))
            .children(comments)
            .child(
                h_flex()
                    .pl_5()
                    .gap_0p5()
                    .when(resolved, |this| {
                        this.child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(muted)
                                .child(tr("dashboard.comments.resolved")),
                        )
                    })
                    .when(!resolved, |this| this.child(div().flex_1()))
                    .child(
                        Button::new(SharedString::from(format!("comment-reply-{id}")))
                            .xsmall()
                            .ghost()
                            .label(tr("dashboard.comments.reply"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.replying = Some(reply_id.clone());
                                this.input.update(cx, |state, cx| state.focus(window, cx));
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("comment-resolve-{id}")))
                            .xsmall()
                            .ghost()
                            .when(!resolved, |this| this.icon(IconName::Check))
                            .label(if resolved {
                                tr("dashboard.comments.reopen")
                            } else {
                                tr("dashboard.comments.resolve")
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let id = resolve_id.clone();
                                this.dashboard
                                    .update(cx, |dashboard, cx| {
                                        dashboard.set_resolved(&id, !resolved, cx)
                                    })
                                    .ok();
                            })),
                    ),
            )
    }
}

impl Render for CommentCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(dashboard) = self.dashboard.upgrade() else {
            return div().into_any_element();
        };
        let (title, threads, error) = {
            let dashboard = dashboard.read(cx);
            (
                dashboard.plot_title(&self.plot),
                dashboard
                    .comments()
                    .on_plot(&self.plot)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>(),
                dashboard.comments_error(),
            )
        };
        // A thread being answered may have been deleted from the file.
        if let Some(id) = &self.replying {
            if !threads.iter().any(|t| &t.id == id) {
                self.replying = None;
            }
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let warning = theme.warning;
        let rendered = threads
            .iter()
            .map(|thread| self.render_thread(thread, cx).into_any_element())
            .collect::<Vec<_>>();
        let replying = self.replying.clone();
        let has_threads = !rendered.is_empty();
        v_flex()
            .w(px(CARD_WIDTH))
            .gap_2p5()
            // The quoted plot: what the comments are about.
            .child(
                h_flex()
                    .gap_2()
                    .child(div().w(px(2.)).h_4().flex_none().rounded_full().bg(warning))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(muted)
                            .text_ellipsis()
                            .child(title),
                    ),
            )
            .when(has_threads, |this| {
                this.child(
                    div()
                        .id("comment-threads")
                        .max_h(px(THREADS_MAX_HEIGHT))
                        .overflow_y_scroll()
                        .mx_neg_2()
                        .child(v_flex().gap_1().children(rendered)),
                )
                .child(div().h(px(1.)).bg(border))
            })
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when_some(replying.clone(), |this, id| {
                this.child(
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .text_color(muted)
                        .child(trf("dashboard.comments.replying", &[&id]))
                        .child(
                            Button::new("comment-reply-cancel")
                                .xsmall()
                                .ghost()
                                .icon(IconName::Close)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.replying = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(Textarea::new(&self.input).small())
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(muted)
                            .child(tr("dashboard.comments.shortcut")),
                    )
                    .child(
                        Button::new("comment-cancel")
                            .small()
                            .outline()
                            .label(tr("dashboard.comments.cancel"))
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        Button::new("comment-post")
                            .small()
                            .primary()
                            .label(if replying.is_some() {
                                tr("dashboard.comments.reply")
                            } else {
                                tr("dashboard.comments.send")
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.post(window, cx))),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(tr("dashboard.comments.hint")),
            )
            .into_any_element()
    }
}
