//! `ducklocal metrics`: a folder's semantic model, compiled to SQL.
//!
//! An Apache Ossie model (`model.ossie.yaml`) says once what a metric means;
//! a request names metrics and the fields to break them down by, and this
//! writes the one SELECT that answers it. Asking by name keeps an agent's
//! answers to the model's definitions instead of to SQL it improvises.
//!
//! The compiler is DuckLocal's own, built on DuckDB's parser (`ast`): it
//! reads every expression as DuckDB will, has DuckDB write the SQL back, and
//! never needs data to do it. Running the result goes through the same path
//! as `ducklocal query`, `--restricted` included. A request the model cannot
//! answer exactly is refused with a stable `kind` (`metric_fanout`,
//! `metric_multi_grain`, ...), not answered approximately.

mod ast;
mod compile;
mod model;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::cli::{parse_args, Arg, CliError, FlagSpec, Format};
use compile::{Order, Request};

pub(crate) const HELP: &str = "\
Usage: ducklocal metrics [MODEL]
       ducklocal metrics compile [MODEL] --metric NAME [REQUEST]
       ducklocal metrics query [MODEL] --metric NAME [REQUEST] [RUN]

A semantic model's vocabulary, and SQL compiled from it. MODEL is an Apache
Ossie model file, or a folder holding model.ossie.yaml; the default is the
current folder.

`metrics` lists the model's metrics, with whether each can be queried and why
not; its fields, by dataset, with which are time fields; and how the datasets
relate. `compile` writes the one SELECT that answers a request and reads no
data. `query` compiles and runs it the way `ducklocal query` runs SQL.

Request:
  --metric NAME[,NAME]   Metrics to compute; repeatable
  --by REF[,REF]         Break down by dataset.field; dataset.field:GRAIN
                         truncates a time field to second, minute, hour, day,
                         week, month, quarter or year; repeatable
  --where SQL            A condition over dataset.field (filters rows) or
                         metric names (filters groups); repeatable, all apply
  --order-by NAME[:desc] Order by an output column; repeatable. Default: the
                         --by columns, ascending
  --limit N              At most N rows

Run (query only):
  --views FILE.dash      Define the model's sources with this dashboard's
                         source and view blocks
  --database PATH        Existing database file, opened read-only
  --format FORMAT        json (default), md or html
  --restricted, --allow-read PATH, --timeout SECONDS, --memory-limit SIZE
                         As for ducklocal query --restricted

Output: compile prints {sql, columns, base, joins, request, model, compiler,
sql_sha256}; query prints the query result with that under \"semantic\". The
model is identified by its sha256: the same request on the same model and
DuckLocal compiles to the same SQL.

A request the model cannot answer exactly is refused, exit 2, with a kind:
metric_unknown_name, metric_fanout (a dimension on the many side of a
relationship), metric_multi_grain (metrics of different datasets),
metric_ambiguous_path, metric_no_path, metric_window_unsupported,
metric_not_aggregate, metric_filter_refused, metric_bad_grain, model_invalid.
";

/// Why a model or request cannot be compiled.
#[derive(Debug, Clone)]
pub(crate) struct Refusal {
    pub kind: &'static str,
    pub message: String,
    pub hint: Option<String>,
}

impl Refusal {
    pub(crate) fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            hint: None,
        }
    }

    pub(crate) fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The same refusal, said in a wider context.
    pub(crate) fn reworded(&self, message: String) -> Self {
        Self {
            kind: self.kind,
            message,
            hint: self.hint.clone(),
        }
    }

    fn report(&self) -> Value {
        let mut out = json!({"kind": self.kind, "message": self.message});
        if let Some(hint) = &self.hint {
            out["hint"] = json!(hint);
        }
        out
    }
}

impl From<Refusal> for CliError {
    fn from(refusal: Refusal) -> Self {
        CliError {
            kind: refusal.kind,
            message: refusal.message,
            code: if matches!(refusal.kind, "model_not_found" | "internal") {
                1
            } else {
                2
            },
            hint: refusal.hint,
        }
    }
}

const fn flag(name: &'static str, takes_value: bool) -> FlagSpec {
    FlagSpec { name, takes_value }
}

const REQUEST_FLAGS: &[FlagSpec] = &[
    flag("--metric", true),
    flag("--by", true),
    flag("--where", true),
    flag("--order-by", true),
    flag("--limit", true),
];

const QUERY_FLAGS: &[FlagSpec] = &[
    flag("--metric", true),
    flag("--by", true),
    flag("--where", true),
    flag("--order-by", true),
    flag("--limit", true),
    flag("--views", true),
    flag("--database", true),
    flag("--format", true),
    flag("--restricted", false),
    flag("--allow-read", true),
    flag("--timeout", true),
    flag("--memory-limit", true),
];

pub(crate) fn run(args: &[OsString]) -> Result<String, CliError> {
    let sub = args.first().and_then(|a| a.to_str());
    let rest = || &args[1..];
    if matches!(sub, Some("compile" | "query")) && rest().iter().any(|a| a == "--help") {
        return Ok(HELP.to_string());
    }
    match sub {
        Some("compile") => compile_command(rest()),
        Some("query") => query_command(rest()),
        _ => vocabulary_command(args),
    }
}

/// A comma-separated, repeatable flag's values.
fn list(value: &OsString, into: &mut Vec<String>) {
    into.extend(
        value
            .to_string_lossy()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    );
}

/// The model, and whether the flag was a request flag `take` used.
fn request_flag(arg: &Arg, request: &mut Request) -> Result<bool, CliError> {
    match arg {
        Arg::Flag("--metric", Some(value)) => list(value, &mut request.metrics),
        Arg::Flag("--by", Some(value)) => list(value, &mut request.by),
        Arg::Flag("--where", Some(value)) => request.filters.push(
            value
                .to_str()
                .ok_or_else(|| CliError::argument("--where must be UTF-8"))?
                .to_string(),
        ),
        Arg::Flag("--order-by", Some(value)) => {
            let mut names = Vec::new();
            list(value, &mut names);
            for name in names {
                let (name, descending) = match name.rsplit_once(':') {
                    Some((name, dir)) if dir.eq_ignore_ascii_case("desc") => (name, true),
                    Some((name, dir)) if dir.eq_ignore_ascii_case("asc") => (name, false),
                    _ => (name.as_str(), false),
                };
                request.order_by.push(Order {
                    name: name.to_string(),
                    descending,
                });
            }
        }
        Arg::Flag("--limit", Some(value)) => {
            request.limit = Some(crate::cli::row_limit(value)? as u64);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn model_path(model: &mut Option<PathBuf>, value: &OsString) -> Result<(), CliError> {
    if model.is_some() {
        return Err(CliError::argument("Name one model"));
    }
    *model = Some(PathBuf::from(value));
    Ok(())
}

fn unexpected(command: &str) -> CliError {
    CliError::failure(
        "internal",
        format!("the argument walker produced a flag metrics {command} does not declare"),
    )
}

/// Open the parser and load the model.
fn load(path: Option<&Path>) -> Result<(ast::Sql, model::Model), CliError> {
    let sql = ast::Sql::open().map_err(|e| CliError::failure("database", e))?;
    let path = model::locate(path);
    let model = model::load(&path, &sql)?;
    Ok((sql, model))
}

fn model_report(model: &model::Model) -> Value {
    json!({
        "path": model.path.display().to_string(),
        "name": model.name,
        "version": model.version,
        "sha256": model.sha256,
    })
}

fn vocabulary_command(args: &[OsString]) -> Result<String, CliError> {
    let mut path = None;
    for arg in parse_args("metrics", args, &[], &[])? {
        match arg {
            Arg::Positional(value) => model_path(&mut path, &value)?,
            _ => return Err(unexpected("")),
        }
    }
    let (sql, model) = load(path.as_deref())?;
    Ok(vocabulary(&model, &sql).to_string())
}

fn vocabulary(model: &model::Model, sql: &ast::Sql) -> Value {
    let datasets: Vec<Value> = model
        .datasets
        .iter()
        .map(|d| {
            let fields: Vec<Value> = d
                .fields
                .iter()
                .map(|f| {
                    let mut out = json!({
                        "name": f.name,
                        "reference": format!("{}.{}", d.name, f.name),
                        "is_time": f.is_time,
                        "description": f.description,
                        "label": f.label,
                        "synonyms": model::synonyms(&f.ai_context),
                        "expression": f.expression.sql,
                        "dialect": f.expression.dialect,
                    });
                    let usable = compile::check_field(model, sql, d, f);
                    out["usable"] = json!(usable.is_ok());
                    if let Err(refusal) = usable {
                        out["problem"] = refusal.report();
                    }
                    out
                })
                .collect();
            json!({
                "name": d.name,
                "source": d.source,
                "primary_key": d.primary_key,
                "description": d.description,
                "synonyms": model::synonyms(&d.ai_context),
                "fields": fields,
            })
        })
        .collect();
    let metrics: Vec<Value> = model
        .metrics
        .iter()
        .map(|m| {
            let mut out = json!({
                "name": m.name,
                "description": m.description,
                "label": m.label,
                "synonyms": model::synonyms(&m.ai_context),
                "expression": m.expression.sql,
                "dialect": m.expression.dialect,
            });
            let request = Request {
                metrics: vec![m.name.clone()],
                ..Default::default()
            };
            match compile::compile(model, sql, &request) {
                Ok(compiled) => {
                    out["queryable"] = json!(true);
                    out["base"] = json!(compiled.base);
                }
                Err(refusal) => {
                    out["queryable"] = json!(false);
                    out["problem"] = refusal.report();
                }
            }
            out
        })
        .collect();
    let relationships: Vec<Value> = model
        .relationships
        .iter()
        .map(|r| {
            json!({
                "name": r.name,
                "from": r.from,
                "to": r.to,
                "from_columns": r.from_columns,
                "to_columns": r.to_columns,
                "cardinality": if r.many_to_one { "many_to_one" } else { "unknown" },
            })
        })
        .collect();
    let mut model_out = model_report(model);
    model_out["description"] = json!(model.description);
    model_out["ai_context"] = json!(model.ai_context);
    json!({
        "model": model_out,
        "metrics": metrics,
        "datasets": datasets,
        "relationships": relationships,
        "time_grains": compile::GRAINS,
        "dialects": model::DIALECTS,
    })
}

/// A compiled request, as `compile` prints it and `query` reports it.
fn compiled_report(
    model: &model::Model,
    sql: &ast::Sql,
    request: &Request,
    compiled: &compile::Compiled,
) -> Value {
    json!({
        "sql": compiled.sql,
        "columns": compiled.columns,
        "base": compiled.base,
        "joins": compiled.joins,
        "request": request,
        "model": model_report(model),
        "compiler": {
            "name": "ducklocal",
            "version": env!("CARGO_PKG_VERSION"),
            "duckdb": sql.version(),
        },
        "sql_sha256": format!("{:x}", Sha256::digest(compiled.sql.as_bytes())),
    })
}

fn compile_command(args: &[OsString]) -> Result<String, CliError> {
    let mut path = None;
    let mut request = Request::default();
    for arg in parse_args("metrics compile", args, REQUEST_FLAGS, &[])? {
        if request_flag(&arg, &mut request)? {
            continue;
        }
        match arg {
            Arg::Positional(value) => model_path(&mut path, &value)?,
            _ => return Err(unexpected("compile")),
        }
    }
    let (sql, model) = load(path.as_deref())?;
    let compiled = compile::compile(&model, &sql, &request)?;
    Ok(compiled_report(&model, &sql, &request, &compiled).to_string())
}

fn query_command(args: &[OsString]) -> Result<String, CliError> {
    let mut path = None;
    let mut request = Request::default();
    let mut views = None;
    let mut database = None;
    let mut format = Format::Json;
    let mut limits = crate::restricted::Flags::default();
    for arg in parse_args("metrics query", args, QUERY_FLAGS, &[])? {
        if request_flag(&arg, &mut request)? {
            continue;
        }
        let Some(arg) = limits.take(arg)? else {
            continue;
        };
        match arg {
            Arg::Positional(value) => model_path(&mut path, &value)?,
            Arg::Flag("--views", Some(value)) => views = Some(PathBuf::from(value)),
            Arg::Flag("--database", Some(value)) => {
                database = Some(crate::cli::database_path(&value)?);
            }
            Arg::Flag("--format", Some(value)) => format = Format::parse(&value)?,
            _ => return Err(unexpected("query")),
        }
    }
    limits.validate()?;
    let (sql, model) = load(path.as_deref())?;
    let compiled = compile::compile(&model, &sql, &request)?;
    let report = compiled_report(&model, &sql, &request, &compiled);
    drop(sql);
    let text = match &views {
        Some(file) => with_views(&compiled.sql, file)?,
        None => compiled.sql.clone(),
    };
    let rows = request
        .limit
        .map(|n| n as usize)
        .unwrap_or(1000)
        .min(crate::query::MAX_ROWS);
    let (result, policy) = crate::cli::execute(&text, database, false, rows, &limits)?;
    crate::cli::write_result(
        result,
        policy.as_ref(),
        format,
        &text,
        Some(("semantic", report)),
    )
}

/// `sql` with the source and view blocks of the dashboard at `file` in front
/// of it, as the dashboard's own queries get them.
fn with_views(sql: &str, file: &Path) -> Result<String, CliError> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| CliError::failure("io", format!("{}: {e}", file.display())))?;
    let parsed = crate::spec::model::parse(&text)
        .map_err(|e| CliError::argument(format!("{}:{}: {}", file.display(), e.line, e.message)))?;
    let spec = crate::spec::model::validate(&parsed).map_err(|diagnostics| {
        let first = diagnostics
            .first()
            .map(|d| format!("{}:{}: {}", file.display(), d.line, d.message))
            .unwrap_or_else(|| format!("{} is not a valid dashboard", file.display()));
        CliError::argument(first)
    })?;
    if let Some((view, message)) = crate::spec::source::unsound_view(&spec.views) {
        return Err(CliError::argument(format!(
            "{}: view {:?}: {message}",
            file.display(),
            view.name
        )));
    }
    let base = crate::spec::source::base_of(file);
    Ok(crate::spec::source::expand(
        sql,
        &spec.sources,
        &spec.views,
        &base,
    ))
}

#[cfg(test)]
mod tests;
