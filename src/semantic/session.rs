//! A model held open for the sidebar's Metrics tab, where a request is built
//! one ticked box at a time.
//!
//! [`super::browse`] reads each model once to list it and compiles one
//! metric per click. The Metrics tab compiles after every change to the
//! request, so a [`Session`] keeps the parser connection and the model
//! between compiles rather than reopening both. It is `Send`, so loading
//! and compiling happen off the UI thread.

use std::path::Path;

use super::ast::Sql;
use super::model::{self, Model};
use super::{compile, Refusal};

pub(crate) use super::compile::{Request, GRAINS};

/// What can be asked of a model: every metric, and every field to break the
/// metrics down by.
#[derive(Debug, Clone)]
pub(crate) struct Vocabulary {
    pub name: String,
    pub description: Option<String>,
    pub metrics: Vec<MetricEntry>,
    pub datasets: Vec<DatasetEntry>,
}

#[derive(Debug, Clone)]
pub(crate) struct MetricEntry {
    pub name: String,
    pub label: Option<String>,
    pub description: Option<String>,
    /// Why the metric cannot be asked for at all, on its own.
    pub problem: Option<Refusal>,
}

#[derive(Debug, Clone)]
pub(crate) struct DatasetEntry {
    pub name: String,
    pub fields: Vec<FieldEntry>,
}

#[derive(Debug, Clone)]
pub(crate) struct FieldEntry {
    pub name: String,
    /// `dataset.field`, as a request names it.
    pub reference: String,
    pub label: Option<String>,
    pub description: Option<String>,
    pub is_time: bool,
    /// Why the field cannot be broken down by.
    pub problem: Option<Refusal>,
}

/// A request compiled, and the SQL that runs it in a query tab.
#[derive(Debug, Clone)]
pub(crate) struct Ready {
    /// The SELECT the compiler wrote, over the model's sources by name.
    pub sql: String,
    /// `sql` with the dashboard's source and view blocks in front of it, when
    /// the model's datasets are a dashboard's, after a comment naming the
    /// request and the model: what the query tab gets.
    pub runnable: String,
    pub columns: Vec<String>,
    /// The dataset the metrics count the rows of.
    pub base: String,
}

pub(crate) struct Session {
    sql: Sql,
    model: Model,
}

impl Session {
    /// Load the model at `path`, and say what can be asked of it.
    pub(crate) fn open(path: &Path) -> Result<(Self, Vocabulary), Refusal> {
        let sql = Sql::open().map_err(|e| Refusal::new("internal", e))?;
        let model = model::load(path, &sql)?;
        let session = Self { sql, model };
        let vocabulary = session.vocabulary();
        Ok((session, vocabulary))
    }

    fn vocabulary(&self) -> Vocabulary {
        let (model, sql) = (&self.model, &self.sql);
        let metrics = model
            .metrics
            .iter()
            .map(|m| {
                let alone = Request {
                    metrics: vec![m.name.clone()],
                    ..Default::default()
                };
                MetricEntry {
                    name: m.name.clone(),
                    label: m.label.clone(),
                    description: m.description.clone(),
                    problem: compile::compile(model, sql, &alone).err(),
                }
            })
            .collect();
        let datasets = model
            .datasets
            .iter()
            .map(|d| DatasetEntry {
                name: d.name.clone(),
                fields: d
                    .fields
                    .iter()
                    .map(|f| FieldEntry {
                        name: f.name.clone(),
                        reference: format!("{}.{}", d.name, f.name),
                        label: f.label.clone(),
                        description: f.description.clone(),
                        is_time: f.is_time,
                        problem: compile::check_field(model, sql, d, f).err(),
                    })
                    .collect(),
            })
            .collect();
        Vocabulary {
            name: model.name.clone(),
            description: model.description.clone(),
            metrics,
            datasets,
        }
    }

    /// Compile `request`; with `views`, a `.dash` file whose source and view
    /// blocks define the model's datasets, as `metrics query --views` does.
    pub(crate) fn compile(
        &self,
        request: &Request,
        views: Option<&Path>,
    ) -> Result<Ready, Refusal> {
        let compiled = compile::compile(&self.model, &self.sql, request)?;
        let body = match views {
            Some(file) => super::expand_views(&compiled.sql, file)
                .map_err(|e| Refusal::new("views_invalid", e))?,
            None => compiled.sql.clone(),
        };
        // After the expansion, which drops a leading comment.
        let runnable = format!("{}\n{body}", self.provenance(request));
        Ok(Ready {
            sql: compiled.sql,
            runnable,
            columns: compiled.columns.into_iter().map(|c| c.name).collect(),
            base: compiled.base,
        })
    }

    /// The comment a query tab's SQL opens with: the request, and the model
    /// it was asked of, in the words of `browse::metric_sql`'s.
    fn provenance(&self, request: &Request) -> String {
        let noun = if request.metrics.len() == 1 {
            "Metric"
        } else {
            "Metrics"
        };
        let mut line = format!("-- {noun} {}", request.metrics.join(", "));
        if !request.by.is_empty() {
            line.push_str(&format!(" by {}", request.by.join(", ")));
        }
        for filter in &request.filters {
            // One line: a newline in the condition would end the comment.
            let filter = filter.split_whitespace().collect::<Vec<_>>().join(" ");
            line.push_str(&format!(" where {filter}"));
        }
        let file = self
            .model
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        line.push_str(&format!(
            ", from model {} ({file}, sha256 {})",
            self.model.name,
            &self.model.sha256[..12.min(self.model.sha256.len())]
        ));
        line
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn example() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/cloudtrail_investigation")
    }

    #[test]
    fn a_session_lists_the_model_and_compiles_runnable_sql() {
        let (session, vocabulary) = Session::open(&example().join("model.ossie.yaml")).unwrap();
        assert_eq!(vocabulary.name, "cloudtrail");
        assert!(vocabulary.metrics.iter().all(|m| m.problem.is_none()));
        let events = &vocabulary.datasets[0];
        let time = events
            .fields
            .iter()
            .find(|f| f.name == "event_time")
            .unwrap();
        assert!(time.is_time);
        assert_eq!(time.reference, "events.event_time");

        let request = Request {
            metrics: vec!["event_count".into(), "error_count".into()],
            by: vec!["events.event_time:day".into()],
            filters: vec!["events.user_name = 'ci-deploy'".into()],
            ..Default::default()
        };
        let views = example().join("reports/investigation.dash");
        let ready = session.compile(&request, Some(&views)).unwrap();
        assert_eq!(ready.base, "events");
        assert_eq!(ready.columns.len(), 3);
        assert!(
            ready.runnable.starts_with(
                "-- Metrics event_count, error_count by events.event_time:day \
                 where events.user_name = 'ci-deploy', from model cloudtrail"
            ),
            "{}",
            ready.runnable
        );
        // The runnable SQL brings its own sources: it runs on a bare
        // connection, as a query tab's would.
        let conn = duckdb::Connection::open_in_memory().unwrap();
        crate::db::preload_extensions(&conn);
        let body = ready
            .runnable
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n");
        let rows: i64 = conn
            .query_row(&format!("SELECT count(*) FROM ({body})"), [], |r| r.get(0))
            .unwrap();
        assert!(rows > 0);

        // A refusal comes back by kind, as the CLI's does.
        let unknown = Request {
            metrics: vec!["no_such_metric".into()],
            ..Default::default()
        };
        assert_eq!(
            session.compile(&unknown, None).unwrap_err().kind,
            "metric_unknown_name"
        );
    }
}
