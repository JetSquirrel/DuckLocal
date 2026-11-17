//! A metric request, compiled to one SELECT.
//!
//! The query this writes has one shape:
//!
//! ```text
//! SELECT <dimensions>, <metrics>
//! FROM <base> LEFT JOIN <one side> ON <keys> ...
//! WHERE <row filters> GROUP BY <dimensions> HAVING <metric filters>
//! ORDER BY ... LIMIT ...
//! ```
//!
//! The base is the one dataset every requested metric aggregates. Every other
//! dataset the request names must be reachable from it along relationships
//! whose far side is a key — many rows to one — so a join can never repeat a
//! base row and inflate an aggregate. The joins are LEFT joins: a base row
//! with no match still counts, under a NULL dimension, rather than dropping
//! out of the totals. A request that cannot be answered this way — metrics of
//! two grains, a dimension on the many side, two join paths — is refused with
//! a kind that says which, not answered approximately.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;

use super::ast::{self, Sql};
use super::model::{closest, Dataset, Field, Metric, Model, Relationship};
use super::Refusal;

/// What `date_trunc` takes, as a time dimension's grain.
pub(crate) const GRAINS: &[&str] = &[
    "second", "minute", "hour", "day", "week", "month", "quarter", "year",
];

/// A request in DuckLocal's own words.
#[derive(Debug, Default, Clone, Serialize)]
pub(crate) struct Request {
    pub metrics: Vec<String>,
    /// `dataset.field`, or `dataset.field:grain` for a time field.
    pub by: Vec<String>,
    /// SQL conditions over `dataset.field` (rows) or metric names (groups).
    #[serde(rename = "where")]
    pub filters: Vec<String>,
    pub order_by: Vec<Order>,
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Order {
    pub name: String,
    pub descending: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct Column {
    pub name: String,
    /// `dimension` or `metric`.
    pub role: &'static str,
    /// The model's name for it: `events.user_name`, `event_count`.
    pub reference: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grain: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Compiled {
    pub sql: String,
    pub columns: Vec<Column>,
    /// The dataset whose rows the metrics aggregate.
    pub base: String,
    /// The relationships joined, in join order.
    pub joins: Vec<String>,
}

/// Where a reference is being resolved: what it may name.
#[derive(Clone, Copy)]
struct Scope<'a> {
    /// The dataset a dataset-level metric belongs to: its bare names are
    /// that dataset's fields.
    home: Option<&'a str>,
    metrics: bool,
}

struct Resolver<'a> {
    model: &'a Model,
    sql: &'a Sql,
}

fn unknown(message: String) -> Refusal {
    Refusal::new("metric_unknown_name", message)
}

impl<'a> Resolver<'a> {
    /// A field's expression with every column read from its dataset.
    fn field(&self, dataset: &Dataset, field: &Field) -> Result<Value, Refusal> {
        let mut tree = field.expression.tree.clone().map_err(|r| {
            r.reworded(format!(
                "field {}.{} cannot be used: {}",
                dataset.name, field.name, r.message
            ))
        })?;
        ast::replace_columns::<Refusal>(&mut tree, &mut |parts| {
            let mut qualified = vec![dataset.name.clone()];
            match parts.split_first() {
                Some((first, rest)) if !rest.is_empty() && first.eq_ignore_ascii_case(&dataset.name) => {
                    qualified.extend(rest.iter().cloned())
                }
                _ => qualified.extend(parts.iter().cloned()),
            }
            Ok(ast::column(&qualified))
        })?;
        let shape = self.sql.shape(&tree);
        if shape.aggregates || shape.window.is_some() {
            return Err(Refusal::new(
                "model_invalid",
                format!(
                    "field {}.{} aggregates; an aggregate belongs in a metric",
                    dataset.name, field.name
                ),
            ));
        }
        Ok(tree)
    }

    /// A metric's expression with every field and metric it names in place.
    fn metric(&self, metric: &Metric, stack: &mut Vec<String>) -> Result<Value, Refusal> {
        if stack.iter().any(|name| name.eq_ignore_ascii_case(&metric.name)) {
            stack.push(metric.name.clone());
            return Err(Refusal::new(
                "model_invalid",
                format!("metrics refer to each other in a circle: {}", stack.join(" -> ")),
            ));
        }
        let mut tree = metric.expression.tree.clone().map_err(|r| {
            r.reworded(format!("metric {:?} cannot be used: {}", metric.name, r.message))
        })?;
        stack.push(metric.name.clone());
        let scope = Scope {
            home: metric.dataset.as_deref(),
            metrics: true,
        };
        let result = ast::replace_columns(&mut tree, &mut |parts| self.reference(parts, scope, stack));
        stack.pop();
        result.map_err(|r| {
            if r.kind == "metric_unknown_name" && stack.is_empty() {
                r.reworded(format!("metric {:?}: {}", metric.name, r.message))
            } else {
                r
            }
        })?;
        Ok(tree)
    }

    /// What a column reference in a metric or filter stands for.
    fn reference(
        &self,
        parts: &[String],
        scope: Scope<'_>,
        stack: &mut Vec<String>,
    ) -> Result<Value, Refusal> {
        let written = parts.join(".");
        if parts.len() == 1 {
            let name = &parts[0];
            if let Some(home) = scope.home.and_then(|home| self.model.dataset(home)) {
                if let Some(field) = home.field(name) {
                    return self.field(home, field);
                }
                if let Some(metric) = self.model.metric(&format!("{}.{name}", home.name)) {
                    return self.metric(metric, stack);
                }
            }
            if scope.metrics {
                if let Some(metric) = self.model.metric(name).filter(|m| m.dataset.is_none()) {
                    return self.metric(metric, stack);
                }
            }
            let owners: Vec<String> = self
                .model
                .datasets
                .iter()
                .filter(|d| d.field(name).is_some())
                .map(|d| format!("{}.{}", d.name, name))
                .collect();
            let mut error = unknown(format!("{written:?} is not a metric of the model"));
            if !owners.is_empty() {
                error.message = format!("{written:?} must name its dataset");
                error.hint = Some(format!("Write {}", owners.join(" or ")));
            } else if let Some(near) = closest(name, self.model.metrics.iter().map(|m| m.name.as_str())) {
                error.hint = Some(format!("Did you mean {near}?"));
            }
            return Err(error);
        }
        let Some(dataset) = self.model.dataset(&parts[0]) else {
            let mut error = unknown(format!(
                "{written:?}: the model has no dataset named {:?}",
                parts[0]
            ));
            error.hint = closest(&parts[0], self.model.datasets.iter().map(|d| d.name.as_str()))
                .map(|near| format!("Did you mean {near}.{}?", parts[1..].join(".")));
            return Err(error);
        };
        if let Some(field) = dataset.field(&parts[1]) {
            let mut tree = self.field(dataset, field)?;
            if parts.len() > 2 {
                // `events.params.userName`: a member of a struct-valued field,
                // which DuckDB reads off the column reference itself.
                let names = if tree["class"] == "COLUMN_REF" {
                    tree.get_mut("column_names").and_then(Value::as_array_mut)
                } else {
                    None
                };
                let Some(names) = names else {
                    return Err(unknown(format!(
                        "{written:?}: {}.{} is an expression, not a column to read a member of",
                        dataset.name, field.name
                    )));
                };
                names.extend(parts[2..].iter().cloned().map(Value::String));
            }
            return Ok(tree);
        }
        if scope.metrics && parts.len() == 2 {
            if let Some(metric) = self.model.metric(&format!("{}.{}", dataset.name, parts[1])) {
                return self.metric(metric, stack);
            }
        }
        let mut error = unknown(format!(
            "dataset {:?} has no field {:?}",
            dataset.name, parts[1]
        ));
        error.hint = closest(&parts[1], dataset.fields.iter().map(|f| f.name.as_str()))
            .map(|near| format!("Did you mean {}.{near}?", dataset.name));
        Err(error)
    }
}

/// The datasets whose columns `parts` read: the first part of each.
fn datasets_of(parts: &[Vec<String>]) -> BTreeSet<String> {
    parts.iter().filter_map(|p| p.first().cloned()).collect()
}

/// A dimension as requested: its reference and grain.
fn split_grain(text: &str) -> (&str, Option<String>) {
    if let Some((reference, grain)) = text.rsplit_once(':') {
        let grain = grain.trim().to_lowercase();
        if GRAINS.contains(&grain.as_str()) {
            return (reference.trim(), Some(grain));
        }
    }
    (text.trim(), None)
}

pub(crate) fn compile(model: &Model, sql: &Sql, request: &Request) -> Result<Compiled, Refusal> {
    let resolver = Resolver { model, sql };
    if request.metrics.is_empty() {
        return Err(Refusal::new("metric_missing", "Name at least one metric")
            .with_hint("ducklocal metrics lists them"));
    }

    // Metrics: each one's tree, and the dataset it aggregates.
    let mut selected: Vec<(String, String, Value)> = Vec::new(); // (output, reference, tree)
    let mut grain: BTreeSet<String> = BTreeSet::new();
    for name in &request.metrics {
        let Some(metric) = model.metric(name) else {
            let mut error = unknown(format!("model {:?} has no metric named {name:?}", model.name));
            error.hint = closest(name, model.metrics.iter().map(|m| m.name.as_str()))
                .map(|near| format!("Did you mean {near}?"));
            return Err(error);
        };
        if selected.iter().any(|(_, reference, _)| reference == &metric.name) {
            continue;
        }
        let tree = resolver.metric(metric, &mut Vec::new())?;
        let shape = sql.shape(&tree);
        if let Some(function) = shape.window {
            return Err(Refusal::new(
                "metric_window_unsupported",
                format!(
                    "metric {:?} uses the window function {function}, whose rows depend on the \
                     request; window metrics are not supported yet",
                    metric.name
                ),
            ));
        }
        if !shape.aggregates {
            return Err(Refusal::new(
                "metric_not_aggregate",
                format!("metric {:?} has no aggregate, so it has nothing to group", metric.name),
            ));
        }
        if let Some(bare) = shape.bare.first() {
            return Err(Refusal::new(
                "metric_not_aggregate",
                format!(
                    "metric {:?} reads {} outside an aggregate",
                    metric.name,
                    bare.join(".")
                ),
            ));
        }
        if shape.nested {
            return Err(Refusal::new(
                "metric_nested_aggregate",
                format!("metric {:?} puts an aggregate inside an aggregate", metric.name),
            ));
        }
        let own = datasets_of(&shape.aggregated);
        if own.len() > 1 {
            return Err(Refusal::new(
                "metric_multi_grain",
                format!(
                    "metric {:?} aggregates columns of {} together; each aggregate must read one dataset",
                    metric.name,
                    own.iter().cloned().collect::<Vec<_>>().join(" and ")
                ),
            ));
        }
        grain.extend(own);
        selected.push((metric.name.clone(), metric.name.clone(), tree));
    }

    // Dimensions.
    let mut dimensions: Vec<(Column, Value, String)> = Vec::new(); // (column, tree, dataset.field)
    let mut needed: BTreeSet<String> = BTreeSet::new();
    for text in &request.by {
        let (reference, wanted_grain) = split_grain(text);
        let (dataset, field) = dimension(model, sql, reference)?;
        let mut tree = resolver.field(dataset, field)?;
        if let Some(grain) = &wanted_grain {
            if !field.is_time {
                return Err(Refusal::new(
                    "metric_bad_grain",
                    format!(
                        "{}.{} is not a time field, so it has no {grain}",
                        dataset.name, field.name
                    ),
                ));
            }
            let mut truncated = sql
                .expression(&format!("date_trunc('{grain}', __ducklocal_value)"))
                .map_err(|e| Refusal::new("internal", e))?;
            ast::replace_columns::<Refusal>(&mut truncated, &mut |_| Ok(tree.clone()))?;
            tree = truncated;
        }
        let reference = format!("{}.{}", dataset.name, field.name);
        if dimensions
            .iter()
            .any(|(c, _, r)| r == &reference && c.grain == wanted_grain)
        {
            continue;
        }
        needed.extend(datasets_of(&sql.shape(&tree).bare));
        dimensions.push((
            Column {
                name: String::new(),
                role: "dimension",
                reference: reference.clone(),
                grain: wanted_grain,
            },
            tree,
            reference,
        ));
    }
    name_dimensions(&mut dimensions, &selected);

    // Filters: on rows before grouping, or on metrics after.
    let mut row_filters = Vec::new();
    let mut group_filters = Vec::new();
    for text in &request.filters {
        let refused = |message: String| {
            Refusal::new("metric_filter_refused", format!("filter {text:?}: {message}"))
        };
        let mut tree = sql.expression(text).map_err(refused)?;
        let scope = Scope {
            home: None,
            metrics: true,
        };
        ast::replace_columns(&mut tree, &mut |parts| {
            resolver.reference(parts, scope, &mut Vec::new())
        })?;
        let shape = sql.shape(&tree);
        if shape.window.is_some() {
            return Err(refused("a window function cannot filter".to_string()));
        }
        if shape.aggregates {
            let own = datasets_of(&shape.aggregated);
            grain.extend(own);
            needed.extend(datasets_of(&shape.bare));
            group_filters.push(tree);
        } else {
            needed.extend(datasets_of(&shape.bare));
            row_filters.push(tree);
        }
    }

    // The base: the one dataset the aggregates read.
    let base = match grain.len() {
        1 => grain.into_iter().next().unwrap_or_default(),
        0 if model.datasets.len() == 1 => model.datasets[0].name.clone(),
        0 => {
            return Err(Refusal::new(
                "metric_no_dataset",
                "no requested metric reads a column, so there is no dataset to count the rows of",
            )
            .with_hint("Qualify the aggregate in the model: COUNT(events.event_id), not COUNT(*)"))
        }
        _ => {
            return Err(Refusal::new(
                "metric_multi_grain",
                format!(
                    "these metrics aggregate different datasets ({}); one query aggregates one",
                    grain.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
            )
            .with_hint("Ask for each dataset's metrics in a query of its own"))
        }
    };
    let base_dataset = model
        .dataset(&base)
        .ok_or_else(|| Refusal::new("internal", format!("no dataset {base:?}")))?;
    needed.remove(&base_dataset.name);

    // Joins: one path, many rows to one, from the base to each dataset named.
    let mut joins: Vec<&Relationship> = Vec::new();
    for target in &needed {
        let paths = paths(model, &base_dataset.name, target);
        match paths.len() {
            1 => {
                for r in &paths[0] {
                    if !joins.iter().any(|j| j.name == r.name) {
                        joins.push(r);
                    }
                }
            }
            0 => {
                let connected = connected(model, &base_dataset.name, target);
                return Err(if connected {
                    Refusal::new(
                        "metric_fanout",
                        format!(
                            "{target} is not on the one side of a relationship from {base}: joining \
                             it would repeat {base} rows and inflate every metric"
                        ),
                    )
                    .with_hint(format!(
                        "Ask for {target}'s own metrics, or by a field of {base} instead"
                    ))
                } else {
                    Refusal::new(
                        "metric_no_path",
                        format!("no relationship joins {base} to {target}"),
                    )
                });
            }
            _ => {
                let routes: Vec<String> = paths
                    .iter()
                    .map(|p| p.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(" > "))
                    .collect();
                return Err(Refusal::new(
                    "metric_ambiguous_path",
                    format!(
                        "{base} reaches {target} more than one way ({}); the model must say which",
                        routes.join("; ")
                    ),
                ));
            }
        }
    }

    // The query.
    let render = |tree: &Value| sql.render(tree).map_err(|e| Refusal::new("internal", e));
    let mut items = Vec::new();
    for (column, tree, _) in &dimensions {
        items.push(format!("{} AS {}", render(tree)?, ast::quote(&column.name)));
    }
    for (name, _, tree) in &selected {
        items.push(format!("{} AS {}", render(tree)?, ast::quote(name)));
    }
    let mut text = format!(
        "SELECT {}\nFROM {} AS {}",
        items.join(", "),
        table(base_dataset),
        ast::quote(&base_dataset.name)
    );
    for r in &joins {
        let (Some(from), Some(to)) = (model.dataset(&r.from), model.dataset(&r.to)) else {
            return Err(Refusal::new("internal", format!("relationship {:?}", r.name)));
        };
        let mut keys = Vec::new();
        for (f, t) in r.from_columns.iter().zip(&r.to_columns) {
            keys.push(format!(
                "({}) = ({})",
                render(&key(&resolver, from, f)?)?,
                render(&key(&resolver, to, t)?)?
            ));
        }
        text.push_str(&format!(
            "\nLEFT JOIN {} AS {} ON {}",
            table(to),
            ast::quote(&to.name),
            keys.join(" AND ")
        ));
    }
    if !row_filters.is_empty() {
        let rendered = row_filters.iter().map(render).collect::<Result<Vec<_>, _>>()?;
        text.push_str(&format!("\nWHERE ({})", rendered.join(") AND (")));
    }
    if !dimensions.is_empty() {
        let positions: Vec<String> = (1..=dimensions.len()).map(|n| n.to_string()).collect();
        text.push_str(&format!("\nGROUP BY {}", positions.join(", ")));
    }
    if !group_filters.is_empty() {
        let rendered = group_filters.iter().map(render).collect::<Result<Vec<_>, _>>()?;
        text.push_str(&format!("\nHAVING ({})", rendered.join(") AND (")));
    }
    let mut columns: Vec<Column> = dimensions.into_iter().map(|(c, _, _)| c).collect();
    columns.extend(selected.into_iter().map(|(name, reference, _)| Column {
        name,
        role: "metric",
        reference,
        grain: None,
    }));
    if request.order_by.is_empty() {
        let dims = columns.iter().filter(|c| c.role == "dimension").count();
        if dims > 0 {
            let positions: Vec<String> = (1..=dims).map(|n| n.to_string()).collect();
            text.push_str(&format!("\nORDER BY {}", positions.join(", ")));
        }
    } else {
        let mut keys = Vec::new();
        for order in &request.order_by {
            let wanted = order.name.trim();
            let found = columns.iter().find(|c| {
                c.name.eq_ignore_ascii_case(wanted)
                    || (c.grain.is_none() && c.reference.eq_ignore_ascii_case(wanted))
            });
            let Some(column) = found else {
                let mut error = unknown(format!(
                    "--order-by {wanted:?} is not a column of the result"
                ));
                error.hint = Some(format!(
                    "Order by one of: {}",
                    columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
                ));
                return Err(error);
            };
            keys.push(format!(
                "{}{}",
                ast::quote(&column.name),
                if order.descending { " DESC" } else { "" }
            ));
        }
        text.push_str(&format!("\nORDER BY {}", keys.join(", ")));
    }
    if let Some(limit) = request.limit {
        text.push_str(&format!("\nLIMIT {limit}"));
    }
    sql.is_one_select(&text)
        .map_err(|e| Refusal::new("internal", format!("{e}: {text}")))?;
    Ok(Compiled {
        sql: text,
        columns,
        base: base_dataset.name.clone(),
        joins: joins.iter().map(|r| r.name.clone()).collect(),
    })
}

/// The dataset and field a dimension names: `dataset.field`, or a bare field
/// name exactly one dataset has.
fn dimension<'m>(model: &'m Model, sql: &Sql, text: &str) -> Result<(&'m Dataset, &'m Field), Refusal> {
    let parts = sql
        .expression(text)
        .ok()
        .and_then(|tree| ast::column_names(&tree))
        .ok_or_else(|| {
            unknown(format!("--by {text:?} is not a field name"))
                .with_hint("Write dataset.field, such as events.user_name")
        })?;
    match parts.as_slice() {
        [name] => {
            let owners: Vec<(&Dataset, &Field)> = model
                .datasets
                .iter()
                .filter_map(|d| d.field(name).map(|f| (d, f)))
                .collect();
            match owners.as_slice() {
                [one] => Ok(*one),
                [] => {
                    let mut error = unknown(format!("no dataset has a field named {name:?}"));
                    let all: Vec<String> = model
                        .datasets
                        .iter()
                        .flat_map(|d| d.fields.iter().map(move |f| format!("{}.{}", d.name, f.name)))
                        .collect();
                    error.hint = closest(name, all.iter().map(|s| s.as_str()))
                        .map(|near| format!("Did you mean {near}?"));
                    Err(error)
                }
                _ => Err(unknown(format!("{name:?} is a field of more than one dataset"))
                    .with_hint(format!(
                        "Write {}",
                        owners
                            .iter()
                            .map(|(d, f)| format!("{}.{}", d.name, f.name))
                            .collect::<Vec<_>>()
                            .join(" or ")
                    ))),
            }
        }
        [dataset, field] => {
            let Some(d) = model.dataset(dataset) else {
                let mut error = unknown(format!("the model has no dataset named {dataset:?}"));
                error.hint = closest(dataset, model.datasets.iter().map(|d| d.name.as_str()))
                    .map(|near| format!("Did you mean {near}.{field}?"));
                return Err(error);
            };
            let Some(f) = d.field(field) else {
                let mut error = unknown(format!("dataset {:?} has no field {field:?}", d.name));
                error.hint = closest(field, d.fields.iter().map(|f| f.name.as_str()))
                    .map(|near| format!("Did you mean {}.{near}?", d.name));
                return Err(error);
            };
            Ok((d, f))
        }
        _ => Err(unknown(format!("--by {text:?}: write dataset.field"))),
    }
}

/// Output names: the field's own, unless two columns would share it.
fn name_dimensions(dimensions: &mut [(Column, Value, String)], metrics: &[(String, String, Value)]) {
    let short = |c: &Column| {
        let field = c.reference.rsplit('.').next().unwrap_or(&c.reference);
        match &c.grain {
            Some(grain) => format!("{field}_{grain}"),
            None => field.to_string(),
        }
    };
    let names: Vec<String> = dimensions.iter().map(|(c, _, _)| short(c)).collect();
    for (i, (column, _, _)) in dimensions.iter_mut().enumerate() {
        let clash = names
            .iter()
            .enumerate()
            .any(|(j, n)| j != i && n.eq_ignore_ascii_case(&names[i]))
            || metrics.iter().any(|(m, _, _)| m.eq_ignore_ascii_case(&names[i]));
        column.name = if clash {
            match &column.grain {
                Some(grain) => format!("{}_{grain}", column.reference),
                None => column.reference.clone(),
            }
        } else {
            names[i].clone()
        };
    }
}

/// A relationship column: the dataset's field of that name, or its column.
fn key(resolver: &Resolver<'_>, dataset: &Dataset, name: &str) -> Result<Value, Refusal> {
    match dataset.field(name) {
        Some(field) => resolver.field(dataset, field),
        None => Ok(ast::column(&[dataset.name.clone(), name.to_string()])),
    }
}

fn table(dataset: &Dataset) -> String {
    dataset
        .table
        .iter()
        .map(|part| ast::quote(part))
        .collect::<Vec<_>>()
        .join(".")
}

/// Every route from `from` to `to` along many-to-one relationships, each
/// dataset at most once.
fn paths<'m>(model: &'m Model, from: &str, to: &str) -> Vec<Vec<&'m Relationship>> {
    fn walk<'m>(
        model: &'m Model,
        at: &str,
        to: &str,
        seen: &mut Vec<String>,
        path: &mut Vec<&'m Relationship>,
        out: &mut Vec<Vec<&'m Relationship>>,
    ) {
        if at.eq_ignore_ascii_case(to) {
            out.push(path.clone());
            return;
        }
        for r in &model.relationships {
            if r.many_to_one
                && r.from.eq_ignore_ascii_case(at)
                && !seen.iter().any(|s| s.eq_ignore_ascii_case(&r.to))
            {
                seen.push(r.to.clone());
                path.push(r);
                walk(model, &r.to, to, seen, path, out);
                path.pop();
                seen.pop();
            }
        }
    }
    let mut out = Vec::new();
    walk(model, from, to, &mut vec![from.to_string()], &mut Vec::new(), &mut out);
    out
}

/// Whether any relationships, in either direction, connect the two.
fn connected(model: &Model, from: &str, to: &str) -> bool {
    let mut reached = vec![from.to_lowercase()];
    let mut i = 0;
    while i < reached.len() {
        let at = reached[i].clone();
        for r in &model.relationships {
            for (a, b) in [(&r.from, &r.to), (&r.to, &r.from)] {
                if a.to_lowercase() == at && !reached.contains(&b.to_lowercase()) {
                    reached.push(b.to_lowercase());
                }
            }
        }
        i += 1;
    }
    reached.contains(&to.to_lowercase())
}

/// Whether a field can be used: its expression parses, reads only its own
/// dataset's columns and does not aggregate.
pub(crate) fn check_field(
    model: &Model,
    sql: &Sql,
    dataset: &Dataset,
    field: &Field,
) -> Result<(), Refusal> {
    Resolver { model, sql }.field(dataset, field).map(|_| ())
}
