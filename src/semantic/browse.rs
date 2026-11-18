//! The semantic models the window can show: found beside the dashboards the
//! person has opened, summed up for the sidebar, and compiled to SQL the
//! query editor runs as it is.
//!
//! A model sits at a project's root and its datasets' sources are usually a
//! dashboard's `source` and `view` blocks, so a model is looked for from each
//! dashboard's folder upward, and the SQL handed to the editor carries those
//! blocks as CTEs: it runs on the window's connection without the dashboard
//! being open.

use std::path::{Path, PathBuf};

use super::{ast, compile, model};

/// The model's file name, in a folder.
const MODEL_FILE: &str = "model.ossie.yaml";
/// How many folders above a dashboard's own a model is looked for, when no
/// project manifest says where the project ends first.
const MAX_LEVELS_UP: usize = 3;

/// A model, as the sidebar lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelEntry {
    pub path: PathBuf,
    pub name: String,
    pub description: Option<String>,
    /// The dashboard whose `source` and `view` blocks define the model's
    /// datasets, put in front of the SQL. `None` when no dashboard beside it
    /// does: the datasets are then the connection's own tables.
    pub views: Option<PathBuf>,
    pub metrics: Vec<MetricEntry>,
    /// Why the model could not be read; it then has no metrics.
    pub problem: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MetricEntry {
    pub name: String,
    pub label: Option<String>,
    pub description: Option<String>,
    pub synonyms: Vec<String>,
    /// Why it cannot be queried; `None` when it can.
    pub problem: Option<String>,
    /// The first usable time field of the dataset it counts, as
    /// `dataset.field`, for a breakdown over time.
    pub time_field: Option<String>,
}

/// The models beside `dashboards`, each once, in the order first met.
pub(crate) fn discover(dashboards: &[PathBuf]) -> Vec<ModelEntry> {
    // Each model with the dashboards that led to it.
    let mut found: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for dashboard in dashboards {
        let Some(path) = model_near(dashboard) else {
            continue;
        };
        match found.iter_mut().find(|(model, _)| *model == path) {
            Some((_, dashboards)) => dashboards.push(dashboard.clone()),
            None => found.push((path, vec![dashboard.clone()])),
        }
    }
    if found.is_empty() {
        return Vec::new();
    }
    let sql = match ast::Sql::open() {
        Ok(sql) => sql,
        Err(e) => {
            return found
                .into_iter()
                .map(|(path, _)| unreadable(path, e.clone()))
                .collect()
        }
    };
    found
        .into_iter()
        .map(|(path, dashboards)| match model::load(&path, &sql) {
            Ok(model) => entry(&model, &sql, &dashboards),
            Err(refusal) => unreadable(path, refusal.message),
        })
        .collect()
}

/// The model file in `dashboard`'s folder or one above it, stopping at the
/// folder that holds the project's manifest.
fn model_near(dashboard: &Path) -> Option<PathBuf> {
    let start = std::path::absolute(dashboard).ok()?;
    for folder in start.ancestors().skip(1).take(MAX_LEVELS_UP + 1) {
        let candidate = folder.join(MODEL_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        if folder.join(crate::project::MANIFEST).is_file() {
            break;
        }
    }
    None
}

fn unreadable(path: PathBuf, problem: String) -> ModelEntry {
    let name = path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| MODEL_FILE.to_string());
    ModelEntry {
        path,
        name,
        description: None,
        views: None,
        metrics: Vec::new(),
        problem: Some(problem),
    }
}

fn entry(model: &model::Model, sql: &ast::Sql, dashboards: &[PathBuf]) -> ModelEntry {
    let metrics = model
        .metrics
        .iter()
        .map(|metric| {
            let request = compile::Request {
                metrics: vec![metric.name.clone()],
                ..Default::default()
            };
            let (problem, time_field) = match compile::compile(model, sql, &request) {
                Ok(compiled) => (None, time_field(model, sql, &compiled.base)),
                Err(refusal) => (Some(said(&refusal)), None),
            };
            MetricEntry {
                name: metric.name.clone(),
                label: metric.label.clone(),
                description: metric.description.clone(),
                synonyms: model::synonyms(&metric.ai_context),
                problem,
                time_field,
            }
        })
        .collect();
    ModelEntry {
        path: model.path.clone(),
        name: model.name.clone(),
        description: model.description.clone(),
        views: views_for(model, dashboards),
        metrics,
        problem: None,
    }
}

fn time_field(model: &model::Model, sql: &ast::Sql, base: &str) -> Option<String> {
    let dataset = model.dataset(base)?;
    dataset
        .fields
        .iter()
        .find(|field| field.is_time && compile::check_field(model, sql, dataset, field).is_ok())
        .map(|field| format!("{}.{}", dataset.name, field.name))
}

/// Of `dashboards`, the one whose `source` and `view` blocks define the most
/// of the model's dataset sources; `None` when none defines any.
fn views_for(model: &model::Model, dashboards: &[PathBuf]) -> Option<PathBuf> {
    let wanted: Vec<&str> = model.datasets.iter().map(|d| d.source.as_str()).collect();
    dashboards
        .iter()
        .filter_map(|dashboard| {
            let defined = defined_names(dashboard)?;
            let covered = wanted
                .iter()
                .filter(|source| defined.iter().any(|name| name.eq_ignore_ascii_case(source)))
                .count();
            (covered > 0).then_some((covered, dashboard))
        })
        // The first of the best: max_by_key keeps the last of equals.
        .rev()
        .max_by_key(|(covered, _)| *covered)
        .map(|(_, dashboard)| dashboard.clone())
}

/// The names a dashboard's `source` and `view` blocks define, or `None` for
/// one that does not read.
fn defined_names(dashboard: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(dashboard).ok()?;
    let parsed = crate::spec::model::parse(&text).ok()?;
    let spec = crate::spec::model::validate(&parsed).ok()?;
    Some(
        spec.sources
            .iter()
            .map(|source| source.name.clone())
            .chain(spec.views.iter().map(|view| view.name.clone()))
            .collect(),
    )
}

fn said(refusal: &super::Refusal) -> String {
    match &refusal.hint {
        Some(hint) => format!("{} ({hint})", refusal.message),
        None => refusal.message.clone(),
    }
}

/// The SQL for `metric` of the model at `model_path`, broken down by `by`
/// when given, with `views`' blocks in front so it runs on any connection.
/// It opens with a comment naming the metric and the model it came from.
pub(crate) fn metric_sql(
    model_path: &Path,
    views: Option<&Path>,
    metric: &str,
    by: Option<&str>,
) -> Result<String, String> {
    let sql = ast::Sql::open()?;
    let model = model::load(model_path, &sql).map_err(|refusal| said(&refusal))?;
    let request = compile::Request {
        metrics: vec![metric.to_string()],
        by: by.map(str::to_string).into_iter().collect(),
        ..Default::default()
    };
    let compiled = compile::compile(&model, &sql, &request).map_err(|refusal| said(&refusal))?;
    let body = match views {
        Some(file) => super::expand_views(&compiled.sql, file)?,
        None => compiled.sql,
    };
    let file = model_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let by = by.map(|by| format!(" by {by}")).unwrap_or_default();
    Ok(format!(
        "-- Metric {metric}{by}, from model {} ({file}, sha256 {})\n{body}",
        model.name,
        &model.sha256[..12.min(model.sha256.len())],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/cloudtrail_investigation")
    }

    #[test]
    fn a_dashboard_below_the_model_finds_it_and_its_views() {
        let dashboard = example().join("reports/investigation.dash");
        let models = discover(&[dashboard.clone(), dashboard.clone()]);
        assert_eq!(
            models.len(),
            1,
            "one model, however many dashboards lead to it"
        );
        let model = &models[0];
        assert_eq!(model.name, "cloudtrail");
        assert_eq!(model.problem, None);
        assert_eq!(
            model
                .views
                .as_deref()
                .map(std::path::absolute)
                .transpose()
                .unwrap(),
            Some(std::path::absolute(&dashboard).unwrap())
        );
        let error_count = model
            .metrics
            .iter()
            .find(|m| m.name == "error_count")
            .unwrap();
        assert_eq!(error_count.problem, None);
        assert_eq!(error_count.time_field.as_deref(), Some("events.event_time"));
        assert!(error_count.synonyms.contains(&"denied".to_string()));
    }

    #[test]
    fn a_dashboard_with_no_model_above_it_finds_none() {
        let dashboard = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/orders_dashboard")
            .read_dir()
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| path.extension().is_some_and(|ext| ext == "dash"));
        if let Some(dashboard) = dashboard {
            assert!(discover(&[dashboard]).is_empty());
        }
    }

    #[test]
    fn the_metric_sql_carries_the_views_and_runs_on_a_bare_connection() {
        let model = example().join(MODEL_FILE);
        let views = example().join("reports/investigation.dash");
        let sql = metric_sql(
            &model,
            Some(&views),
            "event_count",
            Some("events.event_time:day"),
        )
        .unwrap();
        assert!(sql
            .starts_with("-- Metric event_count by events.event_time:day, from model cloudtrail"));
        assert!(sql.contains("\"events\" AS ("), "the view is a CTE: {sql}");
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let rows: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM ({})",
                    sql.lines().skip(1).collect::<Vec<_>>().join("\n")
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(rows > 0);
    }

    #[test]
    fn an_unknown_metric_says_what_it_might_be() {
        let error = metric_sql(&example().join(MODEL_FILE), None, "error_cnt", None).unwrap_err();
        assert!(error.contains("error_count"), "{error}");
    }
}
