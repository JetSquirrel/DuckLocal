use std::path::Path;

use duckdb::Connection;

use super::ast::Sql;
use super::compile::{compile, Compiled, Order, Request};
use super::model::{self, Model};
use super::Refusal;

fn exp(sql: &str) -> String {
    format!("{{dialects: [{{dialect: ANSI_SQL, expression: \"{sql}\"}}]}}")
}

fn field(name: &str) -> String {
    format!("  - {{name: {name}, expression: {}}}\n", exp(name))
}

fn metric(name: &str, sql: &str) -> String {
    format!("- {{name: {name}, expression: {}}}\n", exp(sql))
}

/// Three datasets, many events to one user to one team, and a metric for
/// every way a request can go wrong.
fn model_text(extra_relationships: &str) -> String {
    let mut text = String::from("version: 0.2.0.dev0\nname: probe\ndatasets:\n");
    text.push_str("- name: events\n  source: events\n  primary_key: [id]\n  fields:\n");
    for name in ["id", "user_name", "target", "code", "amount", "params"] {
        text.push_str(&field(name));
    }
    text.push_str(&format!(
        "  - {{name: seen, expression: {}, dimension: {{is_time: true}}}}\n",
        exp("seen")
    ));
    text.push_str(
        "  - {name: snow, expression: {dialects: [{dialect: SNOWFLAKE, expression: \"IFF(a, b, c)\"}]}}\n",
    );
    text.push_str(&format!(
        "  metrics:\n  - {{name: big, expression: {}}}\n",
        exp("COUNT(id) FILTER (WHERE amount > 10)")
    ));
    text.push_str("- name: users\n  source: main.users\n  primary_key: [user_name]\n  fields:\n");
    text.push_str(&field("user_name"));
    text.push_str(&field("team"));
    text.push_str("- name: teams\n  source: teams\n  primary_key: [team]\n  fields:\n");
    text.push_str(&field("team"));
    text.push_str(&field("budget"));
    text.push_str("relationships:\n");
    text.push_str("- {name: events_user, from: events, to: users, from_columns: [user_name], to_columns: [user_name]}\n");
    text.push_str(
        "- {name: users_team, from: users, to: teams, from_columns: [team], to_columns: [team]}\n",
    );
    text.push_str(extra_relationships);
    text.push_str("metrics:\n");
    for (name, sql) in [
        ("event_count", "COUNT(events.id)"),
        ("errors", "COUNT(events.code)"),
        ("error_rate", "errors * 1.0 / NULLIF(event_count, 0)"),
        (
            "p90",
            "PERCENTILE_CONT(0.9) WITHIN GROUP (ORDER BY events.amount)",
        ),
        ("total", "SUM(events.amount)"),
        ("user_count", "COUNT(users.user_name)"),
        (
            "running",
            "SUM(COUNT(events.id)) OVER (ORDER BY events.seen)",
        ),
        ("mixed", "SUM(events.amount * teams.budget)"),
        ("bare", "events.amount"),
        ("nested", "SUM(event_count)"),
        ("loop_a", "loop_b + 1"),
        ("loop_b", "loop_a + 1"),
        ("star", "COUNT(*)"),
        ("bad_field", "COUNT(events.snow)"),
    ] {
        text.push_str(&metric(name, sql));
    }
    text.push_str(
        "- {name: snow_only, expression: {dialects: [{dialect: SNOWFLAKE, expression: \"COUNT_IF(x)\"}]}}\n",
    );
    text
}

const DATA: &str = "
CREATE TABLE events AS SELECT * FROM (VALUES
    (1, 'alice', NULL, TIMESTAMP '2026-01-01 10:15', NULL, 5, {'name': 'a'}),
    (2, 'alice', 'bob', TIMESTAMP '2026-01-01 10:45', 'AccessDenied', 20, {'name': 'b'}),
    (3, 'bob', NULL, TIMESTAMP '2026-01-01 11:05', NULL, 30, {'name': 'a'}),
    (4, 'mallory', NULL, TIMESTAMP '2026-01-02 09:00', 'AccessDenied', 40, {'name': 'c'})
) t(id, user_name, target, seen, code, amount, params);
CREATE TABLE users AS SELECT * FROM (VALUES ('alice', 'eng'), ('bob', 'ops'), ('carol', 'eng')) t(user_name, team);
CREATE TABLE teams AS SELECT * FROM (VALUES ('eng', 100), ('ops', 50)) t(team, budget);
";

fn setup(extra: &str) -> (Sql, Model) {
    let sql = Sql::open().unwrap();
    let model = model::from_str(&model_text(extra), &sql).unwrap();
    (sql, model)
}

fn request(metrics: &[&str], by: &[&str]) -> Request {
    Request {
        metrics: metrics.iter().map(|s| s.to_string()).collect(),
        by: by.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn ok(sql: &Sql, model: &Model, request: &Request) -> Compiled {
    compile(model, sql, request).unwrap_or_else(|r| panic!("{}: {}", r.kind, r.message))
}

fn refused(sql: &Sql, model: &Model, request: &Request) -> Refusal {
    match compile(model, sql, request) {
        Ok(compiled) => panic!("compiled, but should not have:\n{}", compiled.sql),
        Err(refusal) => refusal,
    }
}

/// Every row of `sql`, each value as text.
fn rows(sql: &str) -> Vec<Vec<String>> {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(DATA).unwrap();
    rows_on(&conn, sql)
}

fn rows_on(conn: &Connection, sql: &str) -> Vec<Vec<String>> {
    let mut statement = conn
        .prepare(&format!("SELECT CAST(COLUMNS(*) AS VARCHAR) FROM ({sql})"))
        .unwrap_or_else(|e| panic!("{e}\n{sql}"));
    let mut out = Vec::new();
    let mut result = statement.query([]).unwrap();
    while let Some(row) = result.next().unwrap() {
        let count = row.as_ref().column_count();
        out.push(
            (0..count)
                .map(|i| {
                    row.get::<_, Option<String>>(i)
                        .unwrap()
                        .unwrap_or_else(|| "NULL".into())
                })
                .collect(),
        );
    }
    out
}

fn table(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| row.iter().map(|v| v.to_string()).collect())
        .collect()
}

#[test]
fn one_dataset_compiles_to_one_plain_select() {
    let (sql, model) = setup("");
    let compiled = ok(
        &sql,
        &model,
        &request(&["event_count"], &["events.user_name"]),
    );
    assert_eq!(
        compiled.sql,
        "SELECT events.user_name AS \"user_name\", count(events.id) AS \"event_count\"\n\
         FROM \"events\" AS \"events\"\nGROUP BY 1\nORDER BY 1"
    );
    assert_eq!(compiled.base, "events");
    assert!(compiled.joins.is_empty());
    assert_eq!(
        rows(&compiled.sql),
        table(&[&["alice", "2"], &["bob", "1"], &["mallory", "1"]])
    );
}

#[test]
fn a_row_with_no_match_keeps_counting_under_null() {
    let (sql, model) = setup("");
    let compiled = ok(
        &sql,
        &model,
        &request(&["event_count", "total"], &["users.team"]),
    );
    assert_eq!(compiled.joins, vec!["events_user"]);
    assert!(compiled
        .sql
        .contains("LEFT JOIN \"main\".\"users\" AS \"users\""));
    assert_eq!(
        rows(&compiled.sql),
        table(&[
            &["eng", "2", "25"],
            &["ops", "1", "30"],
            &["NULL", "1", "40"]
        ])
    );
    // Two hops: events > users > teams.
    let compiled = ok(&sql, &model, &request(&["event_count"], &["teams.budget"]));
    assert_eq!(compiled.joins, vec!["events_user", "users_team"]);
    assert_eq!(
        rows(&compiled.sql),
        table(&[&["50", "1"], &["100", "2"], &["NULL", "1"]])
    );
}

#[test]
fn a_metric_of_metrics_is_computed_from_their_definitions() {
    let (sql, model) = setup("");
    let compiled = ok(
        &sql,
        &model,
        &request(&["error_rate"], &["events.user_name"]),
    );
    assert_eq!(
        rows(&compiled.sql),
        table(&[&["alice", "0.5"], &["bob", "0.0"], &["mallory", "1.0"]])
    );
}

#[test]
fn ordered_aggregates_and_dataset_metrics_compile() {
    let (sql, model) = setup("");
    let compiled = ok(&sql, &model, &request(&["p90", "events.big"], &[]));
    assert!(compiled
        .sql
        .contains("quantile_cont(0.9 ORDER BY events.amount)"));
    assert_eq!(rows(&compiled.sql), table(&[&["37.0", "3"]]));
}

#[test]
fn a_time_field_is_truncated_to_its_grain() {
    let (sql, model) = setup("");
    let compiled = ok(
        &sql,
        &model,
        &request(&["event_count"], &["events.seen:hour"]),
    );
    assert_eq!(compiled.columns[0].name, "at_hour");
    assert_eq!(compiled.columns[0].grain.as_deref(), Some("hour"));
    assert_eq!(
        rows(&compiled.sql),
        table(&[
            &["2026-01-01 10:00:00", "2"],
            &["2026-01-01 11:00:00", "1"],
            &["2026-01-02 09:00:00", "1"],
        ])
    );
    let refusal = refused(
        &sql,
        &model,
        &request(&["event_count"], &["events.code:day"]),
    );
    assert_eq!(refusal.kind, "metric_bad_grain");
}

#[test]
fn filters_go_to_where_or_having_by_what_they_read() {
    let (sql, model) = setup("");
    let mut req = request(&["event_count"], &["events.user_name"]);
    req.filters = vec![
        "events.amount >= 20".to_string(),
        "events.params.name <> 'c'".to_string(),
        "event_count >= 1".to_string(),
    ];
    let compiled = ok(&sql, &model, &req);
    assert!(compiled.sql.contains("WHERE ((events.amount >= 20)) AND"));
    assert!(compiled.sql.contains("HAVING ((count(events.id) >= 1))"));
    assert_eq!(
        rows(&compiled.sql),
        table(&[&["alice", "1"], &["bob", "1"]])
    );

    // A filter on a joined dataset brings the join with it.
    let mut req = request(&["event_count"], &[]);
    req.filters = vec!["users.team = 'eng'".to_string()];
    let compiled = ok(&sql, &model, &req);
    assert_eq!(compiled.joins, vec!["events_user"]);
    assert_eq!(rows(&compiled.sql), table(&[&["2"]]));

    for filter in [
        "events.id IN (SELECT 1)",
        "1); DROP TABLE events; --",
        "events.id = 1 UNION SELECT 2",
        "",
    ] {
        let mut req = request(&["event_count"], &[]);
        req.filters = vec![filter.to_string()];
        assert_eq!(
            refused(&sql, &model, &req).kind,
            "metric_filter_refused",
            "{filter:?}"
        );
    }
}

#[test]
fn order_and_limit_name_output_columns() {
    let (sql, model) = setup("");
    let mut req = request(&["event_count"], &["events.user_name"]);
    req.order_by = vec![
        Order {
            name: "event_count".to_string(),
            descending: true,
        },
        Order {
            name: "events.user_name".to_string(),
            descending: false,
        },
    ];
    req.limit = Some(2);
    let compiled = ok(&sql, &model, &req);
    assert!(compiled
        .sql
        .ends_with("ORDER BY \"event_count\" DESC, \"user_name\"\nLIMIT 2"));
    assert_eq!(
        rows(&compiled.sql),
        table(&[&["alice", "2"], &["bob", "1"]])
    );

    req.order_by = vec![Order {
        name: "nope".to_string(),
        descending: false,
    }];
    assert_eq!(refused(&sql, &model, &req).kind, "metric_unknown_name");
}

#[test]
fn a_column_name_two_dimensions_share_is_qualified() {
    let (sql, model) = setup("");
    let compiled = ok(
        &sql,
        &model,
        &request(&["event_count"], &["events.user_name", "users.user_name"]),
    );
    let names: Vec<&str> = compiled.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        ["events.user_name", "users.user_name", "event_count"]
    );
}

#[test]
fn what_cannot_be_answered_exactly_is_refused_by_kind() {
    let (sql, model) = setup("");
    let cases: &[(&[&str], &[&str], &str)] = &[
        (&["user_count"], &["events.user_name"], "metric_fanout"),
        (&["event_count", "user_count"], &[], "metric_multi_grain"),
        (&["mixed"], &[], "metric_multi_grain"),
        (&["running"], &[], "metric_window_unsupported"),
        (&["bare"], &[], "metric_not_aggregate"),
        (&["nested"], &[], "metric_nested_aggregate"),
        (&["loop_a"], &[], "model_invalid"),
        (&["snow_only"], &[], "metric_no_dialect"),
        (&["bad_field"], &[], "metric_no_dialect"),
        (&["star"], &[], "metric_no_dataset"),
        (&["evnt_count"], &[], "metric_unknown_name"),
        (&["event_count"], &["events.nope"], "metric_unknown_name"),
        (
            &["event_count"],
            &["nowhere.user_name"],
            "metric_unknown_name",
        ),
        (&["event_count"], &["user_name"], "metric_unknown_name"),
        (&[], &["events.user_name"], "metric_missing"),
    ];
    for (metrics, by, kind) in cases {
        let refusal = refused(&sql, &model, &request(metrics, by));
        assert_eq!(
            refusal.kind, *kind,
            "{metrics:?} by {by:?}: {}",
            refusal.message
        );
    }
    let typo = refused(&sql, &model, &request(&["evnt_count"], &[]));
    assert_eq!(typo.hint.as_deref(), Some("Did you mean event_count?"));
    let bare = refused(&sql, &model, &request(&["event_count"], &["user_name"]));
    assert_eq!(
        bare.hint.as_deref(),
        Some("Write events.user_name or users.user_name")
    );
    // A metric with no column of its own takes the others' dataset.
    ok(
        &sql,
        &model,
        &request(&["star", "event_count"], &["events.code"]),
    );
}

#[test]
fn two_ways_to_the_same_dataset_are_refused() {
    let second = "- {name: events_target, from: events, to: users, from_columns: [target], to_columns: [user_name]}\n";
    let (sql, model) = setup(second);
    let refusal = refused(&sql, &model, &request(&["event_count"], &["users.team"]));
    assert_eq!(refusal.kind, "metric_ambiguous_path");
    assert!(refusal.message.contains("events_user"));
    assert!(refusal.message.contains("events_target"));
}

#[test]
fn a_join_to_a_side_without_a_key_is_never_taken() {
    let (sql, mut model) = setup("");
    for r in &mut model.relationships {
        r.many_to_one = false;
    }
    let refusal = refused(&sql, &model, &request(&["event_count"], &["users.team"]));
    assert_eq!(refusal.kind, "metric_fanout");
}

#[test]
fn a_structural_mistake_fails_the_whole_model() {
    let sql = Sql::open().unwrap();
    let base = model_text("");
    let cases = [
        base.replace("- name: teams", "- name: users"),
        base.replace("to: teams", "to: nowhere"),
        base.replace("source: teams", "source: \"read_csv('x.csv')\""),
        base.replace("source: teams", "source: \"'x.csv'\""),
        base.replace("name: probe", "name: [not, a, name]"),
        format!("{base}{}", metric("event_count", "COUNT(events.id)")),
    ];
    for text in cases {
        let error = model::from_str(&text, &sql).err().expect("loaded");
        assert_eq!(error.kind, "model_invalid", "{}", error.message);
    }
}

#[test]
fn the_same_request_compiles_to_the_same_sql() {
    let (sql, model) = setup("");
    let req = request(&["error_rate", "p90"], &["users.team", "events.seen:day"]);
    let first = ok(&sql, &model, &req).sql;
    let (sql, model) = setup("");
    assert_eq!(first, ok(&sql, &model, &req).sql);
}

#[test]
fn the_vocabulary_says_what_can_be_asked() {
    let (sql, model) = setup("");
    let vocabulary = super::vocabulary(&model, &sql);
    let metric = |name: &str| {
        vocabulary["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(metric("event_count")["queryable"], true);
    assert_eq!(metric("event_count")["base"], "events");
    assert_eq!(metric("user_count")["base"], "users");
    assert_eq!(metric("running")["queryable"], false);
    assert_eq!(
        metric("running")["problem"]["kind"],
        "metric_window_unsupported"
    );
    assert_eq!(metric("events.big")["queryable"], true);
    let events = &vocabulary["datasets"][0];
    let snow = events["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "snow")
        .unwrap();
    assert_eq!(snow["usable"], false);
    assert_eq!(vocabulary["relationships"][0]["cardinality"], "many_to_one");
}

/// The example folder's model, run over its own data through its
/// dashboard's views, against the dashboard's hand-written query.
#[test]
fn the_cloudtrail_example_answers_as_its_dashboard_does() {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/cloudtrail_investigation");
    let sql = Sql::open().unwrap();
    let model = model::load(&folder.join("model.ossie.yaml"), &sql).unwrap();
    let mut req = request(&["event_count", "error_count"], &["events.ip"]);
    req.filters = vec!["events.user_name = 'ci-deploy'".to_string()];
    let compiled = ok(&sql, &model, &req);
    let views = folder.join("reports/investigation.dash");
    let semantic = super::with_views(&compiled.sql, &views).unwrap();
    let written = super::with_views(
        "SELECT ip, count(*) AS events, count(error_code) AS denied \
         FROM events WHERE user_name = 'ci-deploy' GROUP BY ip",
        &views,
    )
    .unwrap();
    let conn = Connection::open_in_memory().unwrap();
    crate::db::preload_extensions(&conn);
    let mut got = rows_on(&conn, &semantic);
    let mut want = rows_on(&conn, &written);
    got.sort();
    want.sort();
    assert!(!got.is_empty());
    assert_eq!(got, want);

    // Every metric in the example can be asked for.
    let vocabulary = super::vocabulary(&model, &sql);
    for metric in vocabulary["metrics"].as_array().unwrap() {
        assert_eq!(metric["queryable"], true, "{metric}");
    }
}
