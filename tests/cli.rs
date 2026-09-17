use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/cli-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(path.join("home")).unwrap();
        Self(path)
    }

    fn run(&self, args: &[&str], input: Option<&[u8]>) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ducklocal"))
            .args(args)
            .current_dir(&self.0)
            .env("HOME", self.0.join("home"))
            .env("XDG_CONFIG_HOME", self.0.join("home"))
            .env("XDG_DATA_HOME", self.0.join("home"))
            .env("RUST_LOG", "trace")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child.stdin.take().unwrap().write_all(input).unwrap();
        }
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            std::fs::read_dir(self.0.join("home")).unwrap().count(),
            0,
            "CLI wrote GUI state or extensions"
        );
        output
    }

    fn success(&self, args: &[&str]) -> Value {
        self.decode_success(self.run(args, None))
    }

    fn decode_success(&self, output: Output) -> Value {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(value.is_object());
        assert_eq!(
            value["row_count"].as_u64().unwrap() as usize,
            value["rows"].as_array().unwrap().len()
        );
        assert!(value["elapsed_ms"].is_number());
        value
    }

    fn error(&self, args: &[&str], code: i32, kind: &str) -> Value {
        let output = self.run(args, None);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        let value: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["error"]["kind"], kind);
        assert!(!value["error"]["message"].as_str().unwrap().is_empty());
        value
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn help_version_and_argument_errors_do_not_initialize_gui() {
    let s = Sandbox::new();
    for args in [vec!["--help"], vec!["query", "--help"], vec!["--version"]] {
        let output = s.run(&args, None);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("DuckLocal") || text.contains(env!("CARGO_PKG_VERSION")));
    }
    for args in [
        vec!["--wat"],
        vec!["--help", "extra"],
        vec!["--version", "extra"],
        vec!["query"],
        vec!["query", "--sql"],
        vec!["query", "--sql", "--limit", "2"],
        vec!["query", "--sql", ""],
        vec!["query", "--sql", "  "],
        vec!["query", "--sql", "-- only a comment"],
        vec!["query", "--sql", "SELECT 1", "--sql", "SELECT 2"],
        vec!["query", "--sql", "SELECT 1", "--sql-file", "x.sql"],
        vec!["query", "--sql", "SELECT 1", "--read-write"],
        vec!["query", "--sql", "SELECT 1", "--database", ":memory:"],
        vec!["query", "--sql", "SELECT 1", "--wat"],
        vec!["query", "--sql", "SELECT 1", "--limit"],
        vec!["query", "--sql", "SELECT 1", "--limit", "2", "--limit", "3"],
        vec![
            "query",
            "--sql",
            "SELECT 1",
            "--database",
            "a",
            "--database",
            "b",
        ],
        vec![
            "query",
            "--sql",
            "SELECT 1",
            "--database",
            "a",
            "--read-write",
            "--read-write",
        ],
    ] {
        s.error(&args, 2, "argument");
    }
    for limit in ["0", "-1", "+1", "1.5", "abc", "184467440737095516160", ""] {
        s.error(
            &["query", "--sql", "SELECT 1", "--limit", limit],
            2,
            "argument",
        );
    }
    s.error(&["query", "--sql-file", "missing.sql"], 1, "io");
}

#[test]
fn scalar_values_preserve_types_precision_and_duplicate_names() {
    let s = Sandbox::new();
    let out = s.success(&["query", "--sql", "SELECT NULL AS x, 'NULL' AS x, true AS b, false AS f, 1 AS n, 2.5::DOUBLE AS d, 9007199254740991::BIGINT AS safe, 9007199254740992::BIGINT AS large, -9007199254740992::BIGINT AS negative, 18446744073709551615::UBIGINT AS u, 170141183460469231731687303715884105727::HUGEINT AS h, 340282366920938463463374607431768211455::UHUGEINT AS uh, 123456789012345678901234567890.12345678::DECIMAL(38,8) AS dec, 3::DECIMAL(38,0) AS d0"]);
    assert_eq!(out["columns"][0]["name"], "x");
    assert_eq!(out["columns"][1]["name"], "x");
    assert_eq!(out["columns"][4]["type"], "Int32");
    assert_eq!(
        out["rows"][0],
        json!([null, "NULL", true, false, 1, 2.5, 9007199254740991i64,
            {"encoding":"integer","value":"9007199254740992"},
            {"encoding":"integer","value":"-9007199254740992"},
            {"encoding":"integer","value":"18446744073709551615"},
            {"encoding":"integer","value":"170141183460469231731687303715884105727"},
            {"encoding":"integer","value":"340282366920938463463374607431768211455"},
            {"encoding":"decimal","value":"123456789012345678901234567890.12345678"},
            {"encoding":"decimal","value":"3"}
        ])
    );
}

#[test]
fn temporal_binary_and_nested_encodings_are_unambiguous() {
    let s = Sandbox::new();
    let out = s.success(&["query", "--sql", "SELECT DATE '1970-01-02', TIMESTAMP_NS '1970-01-01 00:00:00.123456789', TIME '00:00:00.123456', INTERVAL '1 month 2 days 3 microseconds', '\\x00\\xFF'::BLOB, [NULL, 'NULL'], {'a': NULL, 'b': 'NULL'}, MAP([1,2], [NULL,'NULL']), [1,2]::INTEGER[2], union_value(a := 'NULL'), 'NaN'::DOUBLE, 'Infinity'::DOUBLE, '-Infinity'::DOUBLE, 'red'::ENUM('red','blue')"]);
    assert_eq!(
        out["rows"][0],
        json!([
            {"encoding":"date","unit":"Day","value":"1"},
            {"encoding":"timestamp","unit":"Nanosecond","value":"123456789"},
            {"encoding":"time","unit":"Microsecond","value":"123456"},
            {"encoding":"interval","months":1,"days":2,"nanos":"3000"},
            {"encoding":"hex","value":"00ff"},
            [null,"NULL"], {"encoding":"struct","fields":[["a",null],["b","NULL"]]},
            {"encoding":"map","entries":[[1,null],[2,"NULL"]]}, [1,2],
            {"encoding":"union-value","value":"NULL"},
            {"encoding":"float","value":"NaN"}, {"encoding":"float","value":"inf"},
            {"encoding":"float","value":"-inf"}, "red"
        ])
    );
    let error = s.error(
        &[
            "query",
            "--sql",
            "SELECT [340282366920938463463374607431768211455::UHUGEINT]",
        ],
        1,
        "sql",
    );
    assert!(error["error"]["message"].as_str().unwrap().contains("CAST"));
}

#[test]
fn limits_zero_rows_and_actual_ddl_dml_results() {
    let s = Sandbox::new();
    for (count, limit, expected, truncated) in [(0, 2, 0, false), (2, 2, 2, false), (3, 2, 2, true)]
    {
        let out = s.success(&[
            "query",
            "--sql",
            &format!("SELECT * FROM range({count})"),
            "--limit",
            &limit.to_string(),
        ]);
        assert_eq!(out["columns"].as_array().unwrap().len(), 1);
        assert_eq!(out["row_count"], expected);
        assert_eq!(out["truncated"], truncated);
    }
    let out = s.success(&["query", "--sql", "SELECT * FROM range(1001)"]);
    assert_eq!(out["row_count"], 1000);
    assert_eq!(out["truncated"], true);
    let cols = (0..400)
        .map(|i| format!("i AS c{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let wide = s.success(&[
        "query",
        "--sql",
        &format!("SELECT {cols} FROM range(6000) t(i)"),
        "--limit",
        "100000",
    ]);
    assert_eq!(wide["row_count"], 5000);
    assert_eq!(wide["truncated"], true);
    let ddl = s.success(&["query", "--sql", "/* not a SELECT */ CREATE TABLE t(a INT)"]);
    assert_eq!(ddl["columns"][0]["name"], "Count");
    assert_eq!(ddl["row_count"], 0);
    s.success(&[
        "query",
        "--database",
        "write.duckdb",
        "--read-write",
        "--sql",
        "CREATE TABLE t(a INT)",
    ]);
    let dml = s.success(&[
        "query",
        "--database",
        "write.duckdb",
        "--read-write",
        "--sql",
        "INSERT INTO t VALUES (1),(2)",
    ]);
    assert_eq!(dml["rows"], json!([[2]]));
}

#[test]
fn parser_rejects_scripts_before_any_side_effect() {
    let s = Sandbox::new();
    for sql in [
        "SELECT 1; SELECT 2",
        "COPY (SELECT 1) TO 'forbidden.csv'; SELECT 2",
        "/* x */ CREATE TABLE t(a INT); -- y\n INSERT INTO t VALUES(1)",
    ] {
        s.error(
            &[
                "query",
                "--database",
                "never.duckdb",
                "--read-write",
                "--sql",
                sql,
            ],
            2,
            "argument",
        );
        assert!(!s.0.join("never.duckdb").exists());
        assert!(!s.0.join("forbidden.csv").exists());
    }
    for sql in ["SELECT (", "/* unterminated", "SELECT * FROM nonexistent"] {
        s.error(&["query", "--sql", sql], 1, "sql");
    }
    for sql in [
        "/* outer /* nested ; */ comment */ SELECT ';' AS s; -- end ;",
        "SELECT $$;$$ AS s",
        ";; SELECT ';' AS s;;;",
        "-- leading\nSELECT ';' AS s",
    ] {
        assert_eq!(s.success(&["query", "--sql", sql])["rows"], json!([[";"]]));
    }
}

#[test]
fn database_is_read_only_by_default_and_copy_is_not_sandboxed() {
    let s = Sandbox::new();
    s.error(
        &["query", "--database", "missing.duckdb", "--sql", "SELECT 1"],
        1,
        "database",
    );
    assert!(!s.0.join("missing.duckdb").exists());
    std::fs::write(s.0.join("bad.duckdb"), "not a database").unwrap();
    s.error(
        &["query", "--database", "bad.duckdb", "--sql", "SELECT 1"],
        1,
        "database",
    );
    s.error(
        &["query", "--database", "home", "--sql", "SELECT 1"],
        1,
        "database",
    );
    s.success(&[
        "query",
        "--database",
        "data.duckdb",
        "--read-write",
        "--sql",
        "CREATE TABLE t AS SELECT 7 AS n",
    ]);
    let before = std::fs::read(s.0.join("data.duckdb")).unwrap();
    s.error(
        &[
            "query",
            "--database",
            "data.duckdb",
            "--sql",
            "INSERT INTO t VALUES (8)",
        ],
        1,
        "sql",
    );
    assert_eq!(
        s.success(&[
            "query",
            "--database",
            "data.duckdb",
            "--sql",
            "SELECT * FROM t"
        ])["rows"],
        json!([[7]])
    );
    assert_eq!(std::fs::read(s.0.join("data.duckdb")).unwrap(), before);
    let copy = s.success(&[
        "query",
        "--database",
        "data.duckdb",
        "--sql",
        "COPY t TO 'export.csv' (HEADER true)",
    ]);
    assert_eq!(copy["rows"], json!([[1]]));
    assert!(s.0.join("export.csv").exists());
    s.success(&[
        "query",
        "--database",
        "data.duckdb",
        "--read-write",
        "--sql",
        "INSERT INTO t VALUES (8)",
    ]);
    assert_eq!(
        s.success(&[
            "query",
            "--database",
            "data.duckdb",
            "--sql",
            "SELECT sum(n) FROM t"
        ])["rows"][0][0],
        15
    );
    let conn = duckdb::Connection::open(s.0.join("data.duckdb")).unwrap();
    s.error(
        &["query", "--database", "data.duckdb", "--sql", "SELECT 1"],
        1,
        "database",
    );
    drop(conn);
}

#[test]
fn skill_file_workflow_sql_files_stdin_and_conversion() {
    let s = Sandbox::new();
    std::fs::write(
        s.0.join("销售 O'Brien.csv"),
        "city,amount\n北京,10\n上海,20\n",
    )
    .unwrap();
    let source = "'销售 O''Brien.csv'";
    let schema = s.success(&[
        "query",
        "--sql",
        &format!("DESCRIBE SELECT * FROM {source}"),
    ]);
    assert_eq!(schema["rows"][0][0], "city");
    assert_eq!(schema["rows"][1][0], "amount");
    let preview = s.success(&[
        "query",
        "--sql",
        &format!("SELECT * FROM {source}"),
        "--limit",
        "1",
    ]);
    assert_eq!(preview["truncated"], true);
    assert_eq!(preview["rows"][0], json!(["北京", 10]));
    std::fs::write(
        s.0.join("analysis.sql"),
        format!("SELECT sum(amount) AS total FROM {source}"),
    )
    .unwrap();
    assert_eq!(
        s.success(&["query", "--sql-file", "analysis.sql"])["rows"],
        json!([[30]])
    );
    let out = s.decode_success(s.run(
        &["query", "--sql-file", "-"],
        Some(b"SELECT 'stdin' AS source"),
    ));
    assert_eq!(out["rows"], json!([["stdin"]]));
    let output = s.run(&["query", "--sql-file", "-"], Some(b"SELECT 1\0; SELECT 2"));
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let _: Value = serde_json::from_slice(&output.stderr).unwrap();
    std::fs::write(s.0.join("invalid.sql"), [0xff, 0xfe]).unwrap();
    s.error(&["query", "--sql-file", "invalid.sql"], 1, "io");
    let copy = s.success(&[
        "query",
        "--sql",
        &format!("COPY (SELECT * FROM {source}) TO '销售 report.parquet' (FORMAT PARQUET)"),
    ]);
    assert_eq!(copy["rows"], json!([[2]]));
    assert_eq!(
        s.success(&[
            "query",
            "--sql",
            "SELECT count(*), sum(amount) FROM '销售 report.parquet'"
        ])["rows"],
        json!([[2, 30]])
    );
    std::fs::write(
        s.0.join("events.json"),
        "[{\"event\":\"open\",\"count\":2}]",
    )
    .unwrap();
    assert_eq!(
        s.success(&["query", "--sql", "SELECT event, count FROM 'events.json'"])["rows"],
        json!([["open", 2]])
    );
    assert_eq!(
        s.success(&[
            "query",
            "--sql",
            "SELECT current_setting('autoinstall_known_extensions')"
        ])["rows"],
        json!([[false]])
    );
}
