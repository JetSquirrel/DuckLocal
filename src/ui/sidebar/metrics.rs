//! The Metrics tab: a request to a semantic model, built by ticking boxes.
//!
//! The Schema tree lists each model's metrics and runs one per click. Here
//! a request is any number of the model's metrics, broken down by any of
//! its fields (a time field at a grain), under an optional condition. It
//! compiles off the UI thread after every change, so a refusal shows while
//! the boxes are being ticked rather than after a run. The compiled SQL
//! opens in a new query tab with the dashboard's source and view blocks in
//! front of it, so it runs on the window's own connection.
//!
//! The models are the ones the tree found beside the remembered dashboards,
//! or a model file picked by hand.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, IconName, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::{sep, tr, trf};
use crate::semantic::session::{Ready, Request, Session, Vocabulary, GRAINS};
use crate::semantic::Refusal;

use super::Sidebar;

/// Where the model and the definitions of its datasets come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ModelSource {
    pub(super) model: PathBuf,
    /// The dashboard whose source and view blocks define the model's
    /// datasets; without one the SQL names them as they are.
    pub(super) views: Option<PathBuf>,
}

enum Load {
    Idle,
    Loading,
    Failed(Refusal),
    Ready {
        session: Arc<Mutex<Session>>,
        vocabulary: Rc<Vocabulary>,
    },
}

/// A field ticked to break down by.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Dimension {
    reference: String,
    /// A time field's grain; `None` groups by the value as it is.
    grain: Option<String>,
}

impl Dimension {
    fn request_text(&self) -> String {
        match &self.grain {
            Some(grain) => format!("{}:{grain}", self.reference),
            None => self.reference.clone(),
        }
    }
}

/// What a time field is broken down by when it is first ticked: a raw
/// timestamp would make a group of nearly every row.
const DEFAULT_GRAIN: &str = "day";

pub(super) struct MetricsPage {
    source: Option<ModelSource>,
    load: Load,
    metrics: Vec<String>,
    by: Vec<Dimension>,
    /// A condition over `dataset.field` or metric names.
    filter: Entity<InputState>,
    result: Option<Result<Ready, Refusal>>,
    compiling: bool,
    /// Bumped by every load and compile; a result for an older one is
    /// dropped.
    generation: u64,
}

impl MetricsPage {
    pub(super) fn new(filter: Entity<InputState>) -> Self {
        Self {
            source: None,
            load: Load::Idle,
            metrics: Vec::new(),
            by: Vec::new(),
            filter,
            result: None,
            compiling: false,
            generation: 0,
        }
    }

    pub(super) fn is_loading(&self) -> bool {
        matches!(self.load, Load::Loading)
    }

    pub(super) fn has_source(&self) -> bool {
        self.source.is_some()
    }

    fn request(&self, cx: &App) -> Request {
        let filter = self.filter.read(cx).value().trim().to_string();
        Request {
            metrics: self.metrics.clone(),
            by: self.by.iter().map(Dimension::request_text).collect(),
            filters: if filter.is_empty() {
                Vec::new()
            } else {
                vec![filter]
            },
            ..Default::default()
        }
    }

    /// Drop the picks the (re)loaded model does not have.
    fn keep_known(&mut self, vocabulary: &Vocabulary) {
        self.metrics.retain(|name| {
            vocabulary
                .metrics
                .iter()
                .any(|m| &m.name == name && m.problem.is_none())
        });
        self.by.retain(|dimension| {
            vocabulary.datasets.iter().any(|d| {
                d.fields
                    .iter()
                    .any(|f| f.reference == dimension.reference && f.problem.is_none())
            })
        });
    }
}

/// Whether `a` and `b` name the same file, however each is spelled.
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

impl Sidebar {
    /// The models the tree found, readable ones only, as the tab offers them.
    fn model_choices(&self) -> Vec<(String, ModelSource)> {
        self.models
            .iter()
            .filter(|model| model.problem.is_none())
            .map(|model| {
                (
                    model.name.clone(),
                    ModelSource {
                        model: model.path.clone(),
                        views: model.views.clone(),
                    },
                )
            })
            .collect()
    }

    /// With no model chosen yet, the first one the tree found.
    pub(super) fn ensure_metrics_model(&mut self, cx: &mut Context<Self>) {
        if self.metrics.source.is_some() {
            return;
        }
        if let Some((_, source)) = self.model_choices().into_iter().next() {
            self.select_metrics_model(source, cx);
        }
    }

    /// Ask `source`'s model; the Schema tree's model rows open it here.
    pub(super) fn select_metrics_model(&mut self, source: ModelSource, cx: &mut Context<Self>) {
        if self.metrics.source.as_ref() == Some(&source) {
            return;
        }
        self.metrics.source = Some(source);
        self.load_metrics_model(cx);
    }

    /// A model file from the system picker. When the tree found the same
    /// model beside a dashboard, that dashboard defines its datasets.
    pub(super) fn pick_metrics_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("sidebar.builder.pick_prompt").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(model) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| {
                let views = this
                    .models
                    .iter()
                    .find(|found| same_file(&found.path, &model))
                    .and_then(|found| found.views.clone());
                this.select_metrics_model(ModelSource { model, views }, cx);
            })
            .ok();
        })
        .detach();
    }

    /// (Re)read the model: when another is chosen, and for the reload
    /// button, which picks up edits to the file.
    pub(super) fn load_metrics_model(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.metrics.source.clone() else {
            return;
        };
        self.metrics.load = Load::Loading;
        self.metrics.result = None;
        self.metrics.compiling = false;
        self.metrics.generation += 1;
        cx.notify();
        let path = source.model.clone();
        // Loading opens a DuckDB parser; off the UI thread, as the tree's
        // model discovery is.
        let opening = cx.background_spawn(async move { Session::open(&path) });
        cx.spawn(async move |this, cx| {
            let opened = opening.await;
            this.update(cx, |this, cx| {
                // Another model was chosen while this one loaded.
                if this.metrics.source.as_ref() != Some(&source) {
                    return;
                }
                match opened {
                    Ok((session, vocabulary)) => {
                        this.metrics.keep_known(&vocabulary);
                        this.metrics.load = Load::Ready {
                            session: Arc::new(Mutex::new(session)),
                            vocabulary: Rc::new(vocabulary),
                        };
                        this.recompile_metrics(cx);
                    }
                    Err(refusal) => this.metrics.load = Load::Failed(refusal),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn recompile_metrics(&mut self, cx: &mut Context<Self>) {
        let Load::Ready { session, .. } = &self.metrics.load else {
            return;
        };
        self.metrics.generation += 1;
        if self.metrics.metrics.is_empty() {
            self.metrics.result = None;
            self.metrics.compiling = false;
            cx.notify();
            return;
        }
        let session = session.clone();
        let request = self.metrics.request(cx);
        let views = self.metrics.source.as_ref().and_then(|s| s.views.clone());
        let generation = self.metrics.generation;
        self.metrics.compiling = true;
        cx.notify();
        let compiling = cx.background_spawn(async move {
            let session = session.lock().unwrap_or_else(|e| e.into_inner());
            session.compile(&request, views.as_deref())
        });
        cx.spawn(async move |this, cx| {
            let result = compiling.await;
            this.update(cx, |this, cx| {
                if this.metrics.generation == generation {
                    this.metrics.result = Some(result);
                    this.metrics.compiling = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn toggle_metric(&mut self, name: &str, cx: &mut Context<Self>) {
        let picked = &mut self.metrics.metrics;
        match picked.iter().position(|m| m == name) {
            Some(ix) => {
                picked.remove(ix);
            }
            None => picked.push(name.to_string()),
        }
        self.recompile_metrics(cx);
    }

    fn toggle_dimension(&mut self, reference: &str, is_time: bool, cx: &mut Context<Self>) {
        let by = &mut self.metrics.by;
        match by.iter().position(|d| d.reference == reference) {
            Some(ix) => {
                by.remove(ix);
            }
            None => by.push(Dimension {
                reference: reference.to_string(),
                grain: is_time.then(|| DEFAULT_GRAIN.to_string()),
            }),
        }
        self.recompile_metrics(cx);
    }

    fn set_grain(&mut self, reference: &str, grain: Option<String>, cx: &mut Context<Self>) {
        if let Some(dimension) = self
            .metrics
            .by
            .iter_mut()
            .find(|d| d.reference == reference)
        {
            dimension.grain = grain;
            self.recompile_metrics(cx);
        }
    }

    /// The compiled request in a new query tab, run when `run`.
    fn open_metrics_query(&mut self, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Ok(ready)) = &self.metrics.result else {
            return;
        };
        let sql = ready.runnable.clone();
        let title = self.metrics.metrics.join(sep());
        let run = run && self.run_metrics;
        self.workspace.update(cx, |ws, cx| {
            let id = ws.open_query_tab(sql, Some(title), window, cx);
            if run && !ws.run_query_tab(id, window, cx) {
                window.push_notification(Notification::warning(tr("sidebar.builder.busy")), cx);
            }
        });
    }

    pub(super) fn render_metrics(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let message = |text: &'static str, cx: &mut Context<Self>| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .text_center()
                .child(text)
        };
        let vocabulary = match &self.metrics.load {
            Load::Idle => {
                return v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .p_4()
                    .child(message(tr("sidebar.builder.empty"), cx))
                    .child(
                        Button::new("metrics-pick-empty")
                            .outline()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label(tr("sidebar.builder.pick"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.pick_metrics_model(window, cx)
                            })),
                    )
                    .into_any_element()
            }
            Load::Loading => {
                return v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .p_4()
                    .child(message(tr("sidebar.builder.loading"), cx))
                    .into_any_element()
            }
            Load::Failed(refusal) => {
                let refusal = refusal.clone();
                return v_flex()
                    .size_full()
                    .p_3()
                    .gap_2()
                    .child(self.render_model_header(None, cx))
                    .child(render_refusal(&refusal, cx))
                    .into_any_element();
            }
            Load::Ready { vocabulary, .. } => vocabulary.clone(),
        };

        v_flex()
            .size_full()
            .child(
                v_flex()
                    .id("metrics-page")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .pb_3()
                    .gap_3()
                    .child(self.render_model_header(Some(&vocabulary), cx))
                    .child(self.render_metric_list(&vocabulary, cx))
                    .child(self.render_dimension_list(&vocabulary, cx))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(section_title(tr("sidebar.builder.where"), cx))
                            .child(Input::new(&self.metrics.filter).xsmall()),
                    ),
            )
            .child(self.render_metrics_footer(cx))
            .into_any_element()
    }

    /// The model's name, which switches to another found model, and where
    /// its file and its datasets' definitions are.
    fn render_model_header(
        &self,
        vocabulary: Option<&Vocabulary>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let source = self.metrics.source.clone();
        let file = |path: &Path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        };
        let views = source
            .as_ref()
            .and_then(|s| s.views.as_deref())
            .map(|views| trf("sidebar.builder.views", &[&file(views)]))
            .unwrap_or_else(|| tr("sidebar.builder.no_views").to_string());
        let path = source
            .as_ref()
            .map(|s| crate::db::compact_home(&s.model.to_string_lossy()))
            .unwrap_or_default();
        let name = vocabulary
            .map(|v| v.name.clone())
            .or_else(|| source.as_ref().map(|s| file(&s.model)))
            .unwrap_or_default();
        let choices = self.model_choices();
        let sidebar = cx.entity().downgrade();
        let switcher = Button::new("metrics-model")
            .ghost()
            .compact()
            .small()
            .label(name)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for (name, choice) in &choices {
                    let sidebar = sidebar.clone();
                    let picked = source.as_ref() == Some(choice);
                    let choice = choice.clone();
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(picked).on_click(
                        move |_, _, cx| {
                            sidebar
                                .update(cx, |this, cx| {
                                    this.select_metrics_model(choice.clone(), cx)
                                })
                                .ok();
                        },
                    ));
                }
                let sidebar = sidebar.clone();
                if !choices.is_empty() {
                    menu = menu.separator();
                }
                menu.item(PopupMenuItem::new(tr("sidebar.builder.pick")).on_click(
                    move |_, window, cx| {
                        sidebar
                            .update(cx, |this, cx| this.pick_metrics_model(window, cx))
                            .ok();
                    },
                ))
            });
        v_flex()
            .pt_1()
            .gap_0p5()
            .child(h_flex().child(switcher))
            .when_some(
                vocabulary.and_then(|v| v.description.clone()),
                |this, description| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .line_clamp(3)
                            .child(description),
                    )
                },
            )
            .child(
                div()
                    .id("metrics-model-path")
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .font_family(cx.theme().mono_font_family.clone())
                    .truncate()
                    .child(path.clone())
                    .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(views),
            )
    }

    fn render_metric_list(
        &self,
        vocabulary: &Vocabulary,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sidebar = cx.entity().downgrade();
        v_flex()
            .gap_1()
            .child(section_title(tr("sidebar.builder.metrics"), cx))
            .when(vocabulary.metrics.is_empty(), |this| {
                this.child(muted(tr("sidebar.builder.no_metrics").into(), cx))
            })
            .children(vocabulary.metrics.iter().enumerate().map(|(ix, metric)| {
                let name = metric.name.clone();
                let sidebar = sidebar.clone();
                let note = metric
                    .label
                    .clone()
                    .filter(|label| *label != metric.name)
                    .or_else(|| metric.description.clone());
                v_flex()
                    .child(
                        Checkbox::new(("metric", ix))
                            .xsmall()
                            .label(metric.name.clone())
                            .when_some(metric.description.clone(), |this, d| this.tooltip(d))
                            .checked(self.metrics.metrics.contains(&metric.name))
                            .disabled(metric.problem.is_some())
                            .on_click(move |_, _, cx| {
                                sidebar
                                    .update(cx, |this, cx| this.toggle_metric(&name, cx))
                                    .ok();
                            }),
                    )
                    .when_some(note, |this, note| {
                        this.child(indented(muted(note.into(), cx).line_clamp(2)))
                    })
                    .when_some(metric.problem.as_ref(), |this, problem| {
                        this.child(indented(problem_text(problem, cx)))
                    })
            }))
    }

    fn render_dimension_list(
        &self,
        vocabulary: &Vocabulary,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sidebar = cx.entity().downgrade();
        let mut ix = 0usize;
        let mut list = v_flex()
            .gap_1()
            .child(section_title(tr("sidebar.builder.by"), cx));
        for dataset in &vocabulary.datasets {
            list = list.child(
                div()
                    .pt_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .font_family(cx.theme().mono_font_family.clone())
                    .truncate()
                    .child(dataset.name.clone()),
            );
            for field in &dataset.fields {
                ix += 1;
                let picked = self
                    .metrics
                    .by
                    .iter()
                    .find(|d| d.reference == field.reference)
                    .cloned();
                let is_time = field.is_time;
                let toggle = {
                    let sidebar = sidebar.clone();
                    let reference = field.reference.clone();
                    move |_: &bool, _: &mut Window, cx: &mut App| {
                        sidebar
                            .update(cx, |this, cx| {
                                this.toggle_dimension(&reference, is_time, cx)
                            })
                            .ok();
                    }
                };
                let label = match &field.label {
                    Some(label) if *label != field.name => format!("{} ({label})", field.name),
                    _ => field.name.clone(),
                };
                let trailing = match (&picked, is_time) {
                    (Some(dimension), true) => Some(
                        self.grain_menu(ix, &field.reference, dimension.grain.clone(), cx)
                            .into_any_element(),
                    ),
                    (None, true) => {
                        Some(muted(tr("sidebar.builder.time").into(), cx).into_any_element())
                    }
                    _ => None,
                };
                let row = h_flex()
                    .gap_1()
                    .child(
                        Checkbox::new(("dimension", ix))
                            .xsmall()
                            .label(label)
                            .when_some(field.description.clone(), |this, d| this.tooltip(d))
                            .checked(picked.is_some())
                            .disabled(field.problem.is_some())
                            .on_click(toggle),
                    )
                    .when_some(trailing, |row, trailing| {
                        row.child(div().flex_1()).child(trailing)
                    });
                list = list.child(
                    v_flex()
                        .child(row)
                        .when_some(field.problem.as_ref(), |this, problem| {
                            this.child(indented(problem_text(problem, cx)))
                        }),
                );
            }
        }
        list
    }

    /// The grain a ticked time field is broken down by.
    fn grain_menu(
        &self,
        ix: usize,
        reference: &str,
        grain: Option<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sidebar = cx.entity().downgrade();
        let reference = reference.to_string();
        let label = grain
            .clone()
            .unwrap_or_else(|| tr("sidebar.builder.grain.none").to_string());
        Button::new(("grain", ix))
            .ghost()
            .compact()
            .xsmall()
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let item = |label: String, value: Option<String>| {
                    let sidebar = sidebar.clone();
                    let reference = reference.clone();
                    let checked = value == grain;
                    PopupMenuItem::new(label)
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            sidebar
                                .update(cx, |this, cx| {
                                    this.set_grain(&reference, value.clone(), cx)
                                })
                                .ok();
                        })
                };
                let mut menu = menu
                    .item(item(tr("sidebar.builder.grain.none").to_string(), None))
                    .separator();
                for grain in GRAINS {
                    menu = menu.item(item(grain.to_string(), Some(grain.to_string())));
                }
                menu
            })
    }

    /// What the ticked boxes compile to, and the buttons that use it; below
    /// the scrolling list, so it stays in view.
    fn render_metrics_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match (&self.metrics.result, self.metrics.compiling) {
            (None, true) => muted(tr("sidebar.builder.compiling").into(), cx).into_any_element(),
            (None, false) => {
                muted(tr("sidebar.builder.pick_a_metric").into(), cx).into_any_element()
            }
            (Some(Err(refusal)), _) => render_refusal(refusal, cx).into_any_element(),
            (Some(Ok(ready)), _) => {
                let summary = trf(
                    "sidebar.builder.ready",
                    &[&ready.columns.len().to_string(), &ready.base],
                );
                v_flex()
                    .gap_1()
                    .child(muted(summary.into(), cx))
                    .child(
                        div()
                            .id("metrics-sql")
                            .max_h(px(140.))
                            .overflow_y_scroll()
                            .p_1()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().muted)
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(ready.sql.clone()),
                    )
                    .into_any_element()
            }
        };
        let can_open = matches!(self.metrics.result, Some(Ok(_))) && !self.metrics.compiling;
        v_flex()
            .p_3()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(body)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("metrics-open")
                            .outline()
                            .small()
                            .label(tr("sidebar.builder.open"))
                            .tooltip(tr("sidebar.builder.open.tooltip"))
                            .disabled(!can_open)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_metrics_query(false, window, cx)
                            })),
                    )
                    .child(
                        Button::new("metrics-run")
                            .primary()
                            .small()
                            .icon(IconName::Play)
                            .label(tr("sidebar.builder.run"))
                            .disabled(!can_open)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_metrics_query(true, window, cx)
                            })),
                    ),
            )
    }
}

fn section_title(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn muted(text: SharedString, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn problem_text(problem: &Refusal, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().danger)
        .line_clamp(3)
        .child(problem.message.clone())
}

/// Under a checkbox's label rather than its box.
fn indented(child: impl IntoElement) -> Div {
    div().pl_5().child(child)
}

/// Why a model or request cannot be answered: the message, what to do about
/// it, and the stable kind the CLI reports too.
fn render_refusal(refusal: &Refusal, cx: &App) -> impl IntoElement {
    v_flex()
        .gap_0p5()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(refusal.message.clone()),
        )
        .when_some(refusal.hint.clone(), |this, hint| {
            this.child(muted(hint.into(), cx))
        })
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .font_family(cx.theme().mono_font_family.clone())
                .child(refusal.kind),
        )
}

#[cfg(test)]
impl Sidebar {
    /// What the ticked boxes compiled to: the query tab's SQL, or the
    /// refusal's kind; `None` before anything is ticked or while compiling.
    pub(crate) fn metrics_outcome(&self) -> Option<Result<String, &'static str>> {
        if self.metrics.compiling {
            return None;
        }
        self.metrics.result.as_ref().map(|result| match result {
            Ok(ready) => Ok(ready.runnable.clone()),
            Err(refusal) => Err(refusal.kind),
        })
    }

    /// The condition box.
    pub(crate) fn metrics_filter(&self) -> Entity<InputState> {
        self.metrics.filter.clone()
    }
}
