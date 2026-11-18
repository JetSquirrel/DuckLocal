//! An Apache Ossie model (`model.ossie.yaml`), read for compiling.
//!
//! The document is the flat 0.2 form: one model at the root, with
//! `datasets`, `relationships` and `metrics`. Keys DuckLocal does not use are
//! ignored, so a model written for other tools still loads.
//!
//! Each expression is taken in the first dialect DuckLocal reads — `DUCKDB`,
//! then `OSSIE_SQL_2026`, then `ANSI_SQL` — and parsed by DuckDB. A field or
//! metric that has no such dialect, or does not parse, stays in the model with
//! the reason it cannot be used: one bad metric does not take the others down.
//! A structural mistake (a duplicate name, a relationship to a dataset that
//! does not exist) fails the whole model, since nothing that uses it could be
//! trusted.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::ast::Sql;
use super::Refusal;

/// The dialects DuckLocal runs, in the order it prefers them.
pub(crate) const DIALECTS: &[&str] = &["DUCKDB", "OSSIE_SQL_2026", "ANSI_SQL"];

/// The largest model file read: a model is definitions, not data.
const MAX_MODEL_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Deserialize)]
struct RawModel {
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    ai_context: Option<Value>,
    #[serde(default)]
    datasets: Vec<RawDataset>,
    #[serde(default)]
    relationships: Vec<RawRelationship>,
    #[serde(default)]
    metrics: Vec<RawMetric>,
}

#[derive(Deserialize)]
struct RawDataset {
    name: String,
    source: String,
    #[serde(default)]
    primary_key: Vec<String>,
    #[serde(default)]
    unique_keys: Vec<Vec<String>>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    ai_context: Option<Value>,
    #[serde(default)]
    fields: Vec<RawField>,
    #[serde(default)]
    metrics: Vec<RawMetric>,
}

#[derive(Deserialize)]
struct RawField {
    name: String,
    expression: RawExpression,
    #[serde(default)]
    dimension: Option<RawDimension>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    ai_context: Option<Value>,
}

#[derive(Deserialize, Default)]
struct RawDimension {
    #[serde(default)]
    is_time: bool,
}

#[derive(Deserialize)]
struct RawMetric {
    name: String,
    expression: RawExpression,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    ai_context: Option<Value>,
}

#[derive(Deserialize)]
struct RawExpression {
    #[serde(default)]
    dialects: Vec<RawDialect>,
    /// The single-dialect shorthand some writers use: `expression: {sql: …}`.
    #[serde(default)]
    sql: Option<String>,
}

#[derive(Deserialize)]
struct RawDialect {
    dialect: String,
    expression: String,
}

#[derive(Deserialize)]
struct RawRelationship {
    name: String,
    from: String,
    to: String,
    from_columns: Vec<String>,
    to_columns: Vec<String>,
}

/// A loaded model.
pub(crate) struct Model {
    pub path: PathBuf,
    pub sha256: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub ai_context: Option<Value>,
    pub datasets: Vec<Dataset>,
    pub relationships: Vec<Relationship>,
    pub metrics: Vec<Metric>,
}

pub(crate) struct Dataset {
    pub name: String,
    /// As written.
    pub source: String,
    /// The table name's parts, for the compiler to quote.
    pub table: Vec<String>,
    pub primary_key: Vec<String>,
    pub unique_keys: Vec<Vec<String>>,
    pub description: Option<String>,
    pub ai_context: Option<Value>,
    pub fields: Vec<Field>,
}

pub(crate) struct Field {
    pub name: String,
    pub is_time: bool,
    pub description: Option<String>,
    pub label: Option<String>,
    pub ai_context: Option<Value>,
    pub expression: Expression,
}

pub(crate) struct Metric {
    /// `name` at the root; `dataset.name` for a metric a dataset defines.
    pub name: String,
    /// The dataset that defines it, for one defined on a dataset.
    pub dataset: Option<String>,
    pub description: Option<String>,
    pub label: Option<String>,
    pub ai_context: Option<Value>,
    pub expression: Expression,
}

/// An expression in the dialect chosen for it, parsed, or why it cannot be.
pub(crate) struct Expression {
    pub dialect: Option<String>,
    pub sql: Option<String>,
    pub tree: Result<Value, Refusal>,
}

pub(crate) struct Relationship {
    pub name: String,
    pub from: String,
    pub to: String,
    pub from_columns: Vec<String>,
    pub to_columns: Vec<String>,
    /// `to_columns` are a key of `to`, so each `from` row meets at most one.
    pub many_to_one: bool,
}

impl Model {
    pub(crate) fn dataset(&self, name: &str) -> Option<&Dataset> {
        self.datasets
            .iter()
            .find(|d| d.name.eq_ignore_ascii_case(name))
    }

    pub(crate) fn metric(&self, name: &str) -> Option<&Metric> {
        self.metrics
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
    }
}

impl Dataset {
    pub(crate) fn field(&self, name: &str) -> Option<&Field> {
        self.fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
    }
}

/// The model file for `path`: the file itself, or `model.ossie.yaml` in a
/// folder.
pub(crate) fn locate(path: Option<&Path>) -> PathBuf {
    let path = path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if path.is_dir() {
        path.join("model.ossie.yaml")
    } else {
        path
    }
}

pub(crate) fn load(path: &Path, sql: &Sql) -> Result<Model, Refusal> {
    let unreadable = |e: &dyn std::fmt::Display| {
        Refusal::new(
            "model_not_found",
            format!("Cannot read the model at {}: {e}", path.display()),
        )
        .with_hint("Name the model file or the folder that holds model.ossie.yaml")
    };
    let size = std::fs::metadata(path).map_err(|e| unreadable(&e))?.len();
    if size > MAX_MODEL_BYTES {
        return Err(Refusal::new(
            "model_invalid",
            format!(
                "{} is {size} bytes; a model is at most {MAX_MODEL_BYTES}",
                path.display()
            ),
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| unreadable(&e))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| Refusal::new("model_invalid", format!("{} is not UTF-8", path.display())))?;
    let sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
    let raw: RawModel = serde_saphyr::from_str(&text)
        .map_err(|e| Refusal::new("model_invalid", format!("{}: {e}", path.display())))?;
    build(raw, path.to_path_buf(), sha256, sql)
}

#[cfg(test)]
pub(crate) fn from_str(text: &str, sql: &Sql) -> Result<Model, Refusal> {
    let raw: RawModel =
        serde_saphyr::from_str(text).map_err(|e| Refusal::new("model_invalid", e.to_string()))?;
    build(raw, PathBuf::from("model.ossie.yaml"), String::new(), sql)
}

fn invalid(message: String) -> Refusal {
    Refusal::new("model_invalid", message)
}

fn duplicates<'a>(what: &str, names: impl Iterator<Item = &'a str>) -> Result<(), Refusal> {
    let mut seen = HashSet::new();
    for name in names {
        if name.trim().is_empty() {
            return Err(invalid(format!("a {what} has an empty name")));
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(invalid(format!("two {what}s are named {name:?}")));
        }
    }
    Ok(())
}

fn build(raw: RawModel, path: PathBuf, sha256: String, sql: &Sql) -> Result<Model, Refusal> {
    duplicates("dataset", raw.datasets.iter().map(|d| d.name.as_str()))?;
    duplicates(
        "relationship",
        raw.relationships.iter().map(|r| r.name.as_str()),
    )?;
    let mut metrics = Vec::new();
    let mut datasets = Vec::new();
    for raw_dataset in raw.datasets {
        duplicates(
            &format!("field of dataset {:?}", raw_dataset.name),
            raw_dataset.fields.iter().map(|f| f.name.as_str()),
        )?;
        let table = sql.table_name(&raw_dataset.source).map_err(|_| {
            invalid(format!(
                "dataset {:?}: source {:?} must name a table or view, such as events or audit.events",
                raw_dataset.name, raw_dataset.source
            ))
        })?;
        let dataset_name = raw_dataset.name.clone();
        let fields = raw_dataset
            .fields
            .into_iter()
            .map(|f| Field {
                expression: expression(&f.expression, sql),
                is_time: f.dimension.as_ref().is_some_and(|d| d.is_time),
                name: f.name,
                description: f.description,
                label: f.label,
                ai_context: f.ai_context,
            })
            .collect();
        for m in raw_dataset.metrics {
            metrics.push(Metric {
                expression: expression(&m.expression, sql),
                name: format!("{}.{}", dataset_name, m.name),
                dataset: Some(dataset_name.clone()),
                description: m.description,
                label: m.label,
                ai_context: m.ai_context,
            });
        }
        datasets.push(Dataset {
            name: raw_dataset.name,
            source: raw_dataset.source,
            table,
            primary_key: raw_dataset.primary_key,
            unique_keys: raw_dataset.unique_keys,
            description: raw_dataset.description,
            ai_context: raw_dataset.ai_context,
            fields,
        });
    }
    for m in raw.metrics {
        if m.name.contains('.') {
            return Err(invalid(format!(
                "metric {:?}: a name with a dot would read as dataset.field",
                m.name
            )));
        }
        metrics.push(Metric {
            expression: expression(&m.expression, sql),
            name: m.name,
            dataset: None,
            description: m.description,
            label: m.label,
            ai_context: m.ai_context,
        });
    }
    duplicates("metric", metrics.iter().map(|m| m.name.as_str()))?;

    let by_name: HashMap<String, &Dataset> = datasets
        .iter()
        .map(|d| (d.name.to_lowercase(), d))
        .collect();
    let mut relationships = Vec::new();
    for r in raw.relationships {
        let (Some(from), Some(to)) = (
            by_name.get(&r.from.to_lowercase()),
            by_name.get(&r.to.to_lowercase()),
        ) else {
            return Err(invalid(format!(
                "relationship {:?} joins {:?} to {:?}, and the model has no dataset named {:?}",
                r.name,
                r.from,
                r.to,
                if by_name.contains_key(&r.from.to_lowercase()) {
                    &r.to
                } else {
                    &r.from
                }
            )));
        };
        if r.from_columns.is_empty() || r.from_columns.len() != r.to_columns.len() {
            return Err(invalid(format!(
                "relationship {:?}: from_columns and to_columns must pair up, one for one",
                r.name
            )));
        }
        let key: HashSet<String> = r.to_columns.iter().map(|c| c.to_lowercase()).collect();
        let is_key = |columns: &Vec<String>| {
            !columns.is_empty()
                && columns
                    .iter()
                    .map(|c| c.to_lowercase())
                    .collect::<HashSet<_>>()
                    == key
        };
        let many_to_one = is_key(&to.primary_key) || to.unique_keys.iter().any(is_key);
        relationships.push(Relationship {
            name: r.name,
            from: from.name.clone(),
            to: to.name.clone(),
            from_columns: r.from_columns,
            to_columns: r.to_columns,
            many_to_one,
        });
    }
    Ok(Model {
        path,
        sha256,
        name: raw.name,
        version: raw.version,
        description: raw.description,
        ai_context: raw.ai_context,
        datasets,
        relationships,
        metrics,
    })
}

fn expression(raw: &RawExpression, sql: &Sql) -> Expression {
    let chosen = DIALECTS.iter().find_map(|wanted| {
        raw.dialects
            .iter()
            .find(|d| d.dialect.eq_ignore_ascii_case(wanted))
            .map(|d| (wanted.to_string(), d.expression.clone()))
    });
    let chosen = chosen.or_else(|| raw.sql.clone().map(|text| ("ANSI_SQL".to_string(), text)));
    let Some((dialect, text)) = chosen else {
        let found: Vec<&str> = raw.dialects.iter().map(|d| d.dialect.as_str()).collect();
        return Expression {
            dialect: None,
            sql: None,
            tree: Err(Refusal::new(
                "metric_no_dialect",
                format!(
                    "no expression in a dialect DuckLocal runs (found {}; reads {})",
                    if found.is_empty() {
                        "none".to_string()
                    } else {
                        found.join(", ")
                    },
                    DIALECTS.join(", ")
                ),
            )),
        };
    };
    let tree = sql
        .expression(&text)
        .map_err(|message| Refusal::new("model_invalid", format!("{text:?}: {message}")));
    Expression {
        dialect: Some(dialect),
        sql: Some(text),
        tree,
    }
}

/// The synonyms an `ai_context` lists, when it is an object that lists them.
pub(crate) fn synonyms(context: &Option<Value>) -> Vec<String> {
    context
        .as_ref()
        .and_then(|c| c.get("synonyms"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `name` among `candidates` that is closest, for "did you mean".
pub(crate) fn closest<'a>(
    name: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    let name = name.to_lowercase();
    candidates
        .map(|c| (crate::cli::edit_distance(&name, &c.to_lowercase()), c))
        .filter(|(distance, c)| *distance <= (c.len() / 3).max(2))
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, c)| c)
}
