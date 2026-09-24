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

    /// A profile is an object too, but it describes columns rather than
    /// returning rows, so it does not answer the row/`row_count` assertion
    /// `decode_success` makes of a query.
    fn profile(&self, args: &[&str]) -> Value {
        let output = self.run(args, None);
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
        assert!(value["elapsed_ms"].is_number());
        value
    }

    /// A run whose one JSON object has a shape of its own — neither a query
    /// result nor a profile.
    fn object(&self, args: &[&str]) -> Value {
        let output = self.run(args, None);
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
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// A run whose stdout is a document, not a JSON object.
    fn text(&self, args: &[&str]) -> String {
        let output = self.run(args, None);
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
        String::from_utf8(output.stdout).unwrap()
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

/// `profile` exists to answer the questions that decide a chart before one is
/// drawn: how much time the data covers and whether it is continuous, how far
/// apart the values are, and how many decimals a number really uses. Those
/// three answers are what this asserts; the rest of the shape comes along.
#[test]
fn profile_reports_the_shape_that_decides_a_chart() {
    let s = Sandbox::new();
    // A gap in the middle of the range, a column spanning three orders of
    // magnitude, money at two decimals, and a column with a hole in it.
    std::fs::write(
        s.0.join("usage.csv"),
        "day,kind,amount,note\n         2026-09-01,hit,2497.69,a\n         2026-09-01,miss,6108704.00,b\n         2026-09-13,out,5229204.00,\n         2026-09-15,req,4994.00,d\n",
    )
    .unwrap();

    let profile = s.profile(&["profile", "usage.csv"]);
    assert_eq!(profile["target"], "usage.csv");
    assert_eq!(profile["row_count"], 4);
    let columns = profile["columns"].as_array().unwrap();
    assert_eq!(columns.len(), 4);

    // Column order is the relation's, not whatever the union came back in.
    let names: Vec<&str> = columns
        .iter()
        .map(|column| column["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["day", "kind", "amount", "note"]);

    let day = &columns[0];
    assert_eq!(day["type"], "DATE");
    assert_eq!(day["min"], "2026-09-01");
    assert_eq!(day["max"], "2026-09-15");
    // Three days named across a fifteen-day span: twelve days a line would
    // draw straight through.
    assert_eq!(day["covered_days"], 3);
    assert_eq!(day["span_days"], 15);
    assert_eq!(day["missing_days"], 12);

    let amount = &columns[2];
    // Two decimals used, whatever the reader inferred for the type.
    assert_eq!(amount["decimals"], 2);
    assert_eq!(amount["min"], "2497.69");
    assert_eq!(amount["max"], "6108704.0");
    // The middle value is one the column holds, not an interpolation between
    // two of them, so it carries no invented digits.
    assert_eq!(amount["median"], "4994.0");
    // Three orders of magnitude between the middle and the largest: a linear
    // axis would round the small values to nothing.
    assert!(
        amount["max_over_median"].as_f64().unwrap() > 1000.0,
        "{amount}"
    );
    assert_eq!(amount["nulls"], 0);
    assert_eq!(amount["unique"], true);

    let note = &columns[3];
    assert_eq!(note["nulls"], 1);
    assert_eq!(note["distinct"], 3);
    // Nothing numeric or temporal is claimed about text.
    assert!(note.get("decimals").is_none());
    assert!(note.get("covered_days").is_none());
    assert!(note.get("unique").is_none());

    // A table in a database, named so that quoting is the only thing that
    // makes it readable.
    s.success(&[
        "query",
        "--database",
        "orders.duckdb",
        "--read-write",
        "--sql",
        "CREATE TABLE \"my orders\" AS SELECT * FROM read_csv_auto('usage.csv')",
    ]);
    let table = s.profile(&["profile", "my orders", "--database", "orders.duckdb"]);
    assert_eq!(table["row_count"], 4);
    assert_eq!(table["relation"], "\"my orders\"");

    // A target that names nothing is a mistake in the command line, not a SQL
    // failure, and the exit code says which.
    s.error(&["profile", "no-such-file.csv"], 2, "argument");
    s.error(&["profile"], 2, "argument");
    s.error(&["profile", "--database", "orders.duckdb"], 2, "argument");
    s.error(&["profile", "usage.csv", "--rows"], 2, "argument");
    s.error(
        &["profile", "absent", "--database", "orders.duckdb"],
        1,
        "sql",
    );
}

/// A workbook target has no table function; `profile` imports its first sheet
/// as a TEMP table — which even a read-only connection allows — and profiles
/// that.
#[test]
fn profile_imports_a_workbooks_first_sheet() {
    let s = Sandbox::new();
    let mut book = rust_xlsxwriter::Workbook::new();
    let sheet = book.add_worksheet().set_name("Orders").unwrap();
    for (col, header) in ["city", "amount"].iter().enumerate() {
        sheet.write_string(0, col as u16, *header).unwrap();
    }
    sheet.write_string(1, 0, "北京").unwrap();
    sheet.write_number(1, 1, 10).unwrap();
    sheet.write_string(2, 0, "上海").unwrap();
    sheet.write_number(2, 1, 20).unwrap();
    book.save(s.0.join("sales.xlsx")).unwrap();

    let profile = s.profile(&["profile", "sales.xlsx"]);
    assert_eq!(profile["target"], "sales.xlsx");
    assert_eq!(profile["relation"], "\"sales\"");
    assert_eq!(profile["row_count"], 2);
    let columns = profile["columns"].as_array().unwrap();
    assert_eq!(columns[0]["name"], "city");
    assert_eq!(columns[0]["type"], "VARCHAR");
    assert_eq!(columns[1]["name"], "amount");
    assert_eq!(columns[1]["type"], "BIGINT");
    assert_eq!(columns[1]["max"], "20");

    s.error(&["profile", "no-such-file.xlsx"], 2, "argument");
}

/// The Markdown format is for reading, so it has to be readable: the values
/// are the grid's, a cell cannot break the table it is in, and a result that is
/// not the whole answer says so.
#[test]
fn markdown_format_renders_a_table_a_reader_can_trust() {
    let s = Sandbox::new();
    let sql = "SELECT 'x|y' AS \"a|b\",
                      'one' || chr(10) || 'two' AS b,
                      NULL AS c,
                      DATE '2024-03-05' AS d,
                      CAST(1.25 AS DECIMAL(5,2)) AS e,
                      CAST(9007199254740993 AS BIGINT) AS f";

    let text = s.text(&["query", "--sql", sql, "--format", "md"]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        [
            "| a\\|b | b | c | d | e | f |",
            "| --- | --- | --- | --- | --- | --- |",
            "| x\\|y | one<br>two | NULL | 2024-03-05 | 1.25 | 9007199254740993 |",
        ]
    );
    // The document is one line ending, exactly like every other output.
    assert!(text.ends_with("|\n"), "{text:?}");
    assert!(!text.ends_with("\n\n"), "{text:?}");

    // `json` is still what it was, so a caller that parses is unaffected.
    let json = s.success(&["query", "--sql", sql]);
    assert_eq!(json["rows"][0][0], "x|y");
    assert_eq!(json["rows"][0][3]["value"], "19787");
    assert_eq!(json["rows"][0][5]["value"], "9007199254740993");

    // A limit is a preview, and the format says which part of the answer the
    // reader is holding.
    let truncated = s.text(&[
        "query",
        "--sql",
        "SELECT * FROM range(5) t(n)",
        "--limit",
        "2",
        "--format",
        "md",
    ]);
    assert!(truncated.contains("| 0 |\n| 1 |\n"), "{truncated}");
    assert!(truncated.contains("_Truncated at 2 rows"), "{truncated}");

    let empty = s.text(&[
        "query",
        "--sql",
        "SELECT 1 AS n WHERE false",
        "--format",
        "md",
    ]);
    assert_eq!(empty, "| n |\n| --- |\n_0 rows._\n");

    let ddl = s.text(&[
        "query",
        "--sql",
        "CREATE TABLE t(a INTEGER)",
        "--format",
        "md",
    ]);
    assert!(ddl.contains('|'), "{ddl}");

    // A format nothing can render, and a flag given twice, are command-line
    // mistakes — not something to guess at.
    s.error(
        &["query", "--sql", "SELECT 1", "--format", "yaml"],
        2,
        "argument",
    );
    s.error(
        &[
            "query", "--sql", "SELECT 1", "--format", "md", "--format", "md",
        ],
        2,
        "argument",
    );
    s.error(&["query", "--sql", "SELECT 1", "--format"], 2, "argument");
}

/// `export` answers "what was this app showing?" without DuckLocal, a
/// database, or a browser extension: it runs the app once and writes down
/// what it asked. The app here is a real one — it builds SQL from
/// `appDir()`, which only answers while an app is loading.
///
/// This is the one test that starts the window platform (hidden, for one
/// frame). It is also the reason the export has a settle deadline: an app's
/// statements arrive asynchronously, and waiting for them is the whole job.
#[test]
fn export_writes_a_report_that_needs_nothing_to_open() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.0.join("app")).unwrap();
    std::fs::write(
        s.0.join("app/orders.csv"),
        "channel,amount\n手机银行,10\n柜面,20\n手机银行,5\n",
    )
    .unwrap();
    std::fs::write(
        s.0.join("app/main.js"),
        r#"
import { View, div } from "gpui-kit";
import { appDir, query, sqlLiteral } from "ducklocal";

export default class App extends View {
  init(_props, cx) {
    this.rows = 0;
    cx.spawn(async (cx) => {
      const source = sqlLiteral(appDir() + "/orders.csv");
      const result = await query(
        `SELECT channel, sum(amount) AS total FROM ${source} GROUP BY 1 ORDER BY 2 DESC`,
        50,
      );
      this.rows = result.rows.length;
      // A statement that fails is part of what an app did, and the report
      // says so rather than dropping it.
      await query("SELECT * FROM absent_table").catch(() => {});
      cx.notify();
    });
  }
  render() { return div().child(String(this.rows)); }
}
"#,
    )
    .unwrap();

    let report = s.object(&["export", "--html", "app", "--out", "report.html"]);
    assert_eq!(report["queries"], 2);
    assert_eq!(report["rows"], 2);
    assert_eq!(report["app_errors"], 0);
    assert_eq!(report["stop_reason"], "settled");
    assert_eq!(
        report["app"],
        s.0.join("app").canonicalize().unwrap().to_str().unwrap()
    );

    let html = std::fs::read_to_string(s.0.join("report.html")).unwrap();
    assert!(
        html.contains("SELECT channel, sum(amount) AS total"),
        "{html}"
    );
    assert!(html.contains("手机银行"), "{html}");
    assert!(html.contains("This statement failed"), "{html}");
    // Nothing to fetch, nothing to run: the file is the whole document.
    assert!(!html.contains("<script"), "{html}");
    assert!(!html.contains("http://"), "{html}");
    assert!(
        html.contains("<svg"),
        "a name and a number per row is a bar chart: {html}"
    );

    // An existing report is not overwritten by a command that was not asked to
    // replace it, and the refusal costs nothing because it happens first.
    s.error(
        &["export", "--html", "app", "--out", "report.html"],
        2,
        "argument",
    );
    let replaced = s.object(&[
        "export",
        "--html",
        "app",
        "--out",
        "report.html",
        "--force",
    ]);
    assert_eq!(replaced["rows"], 2);
}

/// `check` validates a dashboard spec the way the CLI validates everything:
/// statically first — no database, nothing executes — and, when `--database`
/// is given, against the columns the queries really return.
#[test]
fn check_validates_a_dash_spec_with_and_without_a_database() {
    let s = Sandbox::new();
    std::fs::write(
        s.0.join("dashboard.dash"),
        r#"
// Revenue by channel, declared rather than scripted.
query "revenue" {
  sql = <<SQL
    SELECT channel, sum(amount) AS total
    FROM read_csv_auto('usage.csv')
    GROUP BY channel
    ORDER BY total DESC
  SQL
}

plot "revenue" {
  type  = "bar"
  query = query.revenue
  x     = channel
  y     = total
  title = "Revenue by channel"
}

plot "raw" {
  type  = "table"
  query = query.revenue
  x     = channel
}
"#,
    )
    .unwrap();
    std::fs::write(
        s.0.join("usage.csv"),
        "channel,amount\n手机银行,10\n柜面,20\n",
    )
    .unwrap();

    // Static check: no database, and the CSV need not even be readable — the
    // SQL is parsed, never executed.
    let out = s.object(&["check", "dashboard.dash"]);
    assert_eq!(out["queries"].as_array().unwrap().len(), 1);
    assert_eq!(out["plots"].as_array().unwrap().len(), 2);
    assert_eq!(out["plots"][0]["type"], "bar");
    assert_eq!(out["plots"][0]["x"], "channel");
    assert_eq!(out["plots"][1]["y"], Value::Null, "a table needs no y");
    assert_eq!(out["database"], Value::Null);

    // Against a real database the columns are checked too.
    s.success(&[
        "query",
        "--database",
        "data.duckdb",
        "--read-write",
        "--sql",
        "CREATE TABLE usage AS SELECT * FROM read_csv_auto('usage.csv')",
    ]);
    std::fs::write(
        s.0.join("db.dash"),
        r#"
query "revenue" {
  sql = <<SQL
    SELECT channel, sum(amount) AS total FROM usage GROUP BY channel
  SQL
}
plot "revenue" { type = "line" query = query.revenue x = channel y = total }
"#,
    )
    .unwrap();
    let out = s.object(&["check", "db.dash", "--database", "data.duckdb"]);
    assert_eq!(out["queries"][0]["columns"][0]["name"], "channel");
    assert_eq!(out["queries"][0]["columns"][1]["type"], "HUGEINT");

    // A column the query does not return, and a y that is not numeric, are
    // spec mistakes — said with their lines, all in one pass.
    std::fs::write(
        s.0.join("bad.dash"),
        r#"
query "revenue" {
  sql = "SELECT channel, sum(amount) AS total FROM usage GROUP BY channel"
}
plot "a" { type = "bar" query = query.revenue x = nope y = total }
plot "b" { type = "line" query = query.revenue x = channel y = channel }
"#,
    )
    .unwrap();
    let error = s.error(&["check", "bad.dash", "--database", "data.duckdb"], 2, "spec");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("\"nope\""), "{message}");
    assert!(message.contains("not numeric"), "{message}");
    assert!(message.contains("bad.dash:5"), "{message}");
    assert!(message.contains("bad.dash:6"), "{message}");

    // Everything wrong before a database opens is said without opening one:
    // syntax, unknown blocks, dangling references, multi-statement SQL.
    s.error(&["check"], 2, "argument");
    s.error(&["check", "missing.dash"], 1, "io");
    s.error(&["check", "dashboard.dash", "extra.dash"], 2, "argument");
    std::fs::write(s.0.join("syntax.dash"), "plot {\n").unwrap();
    s.error(&["check", "syntax.dash"], 2, "spec");
    std::fs::write(s.0.join("dangling.dash"), "plot \"p\" { type = \"line\" query = query.ghost x = a y = b }").unwrap();
    let error = s.error(&["check", "dangling.dash"], 2, "spec");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("does not define"));
    std::fs::write(
        s.0.join("script.dash"),
        "query \"q\" { sql = \"SELECT 1; SELECT 2\" }",
    )
    .unwrap();
    let error = s.error(&["check", "script.dash"], 2, "spec");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("exactly one SQL statement"));
    // A query whose table does not exist describes nothing: a database
    // failure, not a spec mistake.
    std::fs::write(s.0.join("absent.dash"), "query \"q\" { sql = \"SELECT * FROM absent\" }").unwrap();
    s.error(&["check", "absent.dash", "--database", "data.duckdb"], 1, "sql");

    // A query that plans but fails once rows are read — here a cast that the
    // data cannot satisfy — fails the check the way it would fail the
    // dashboard. DESCRIBE alone passes both; every failing query is named.
    std::fs::write(
        s.0.join("runtime.dash"),
        r#"
query "ok" {
  sql = "SELECT channel, sum(amount) AS total FROM usage GROUP BY channel"
}
query "cast" {
  sql = "SELECT channel, CAST(channel AS INTEGER) AS n FROM usage"
}
query "cast_again" {
  sql = "SELECT channel, CAST(channel AS INTEGER) + 1 AS n FROM usage"
}
plot "ok" { type = "bar" query = query.ok x = channel y = total }
plot "cast" { type = "bar" query = query.cast x = channel y = n }
plot "cast_again" { type = "bar" query = query.cast_again x = channel y = n }
"#,
    )
    .unwrap();
    let error = s.error(&["check", "runtime.dash", "--database", "data.duckdb"], 1, "sql");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("runtime.dash:5: query \"cast\""), "{message}");
    assert!(message.contains("runtime.dash:8: query \"cast_again\""), "{message}");
    assert!(message.contains("Conversion Error"), "{message}");
    assert!(!message.contains("query \"ok\""), "{message}");
    // Without a database nothing runs, so the same file checks clean.
    s.object(&["check", "runtime.dash"]);

    // Help is help, and it is not a spec.
    let help = s.text(&["check", "--help"]);
    assert!(help.contains(".dash"), "{help}");
}

/// Everything wrong with an export that can be said before running one is
/// said without running one: no window opens for a missing flag.
#[test]
fn export_argument_errors_never_start_an_app() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.0.join("app")).unwrap();
    std::fs::write(s.0.join("app/main.js"), "export default class App {}").unwrap();

    s.error(&["export"], 2, "argument");
    s.error(&["export", "nope"], 2, "argument");
    s.error(&["export", "--html"], 2, "argument");
    s.error(
        &["export", "--html", "--out", "x.html"],
        2,
        "argument",
    );
    s.error(
        &["export", "--html", "app", "--format", "md"],
        2,
        "argument",
    );
    s.error(
        &["export", "--html", "app", "--read-write"],
        2,
        "argument",
    );
    s.error(
        &[
            "export", "--html", "app", "--out", "a.html", "--out", "b.html",
        ],
        2,
        "argument",
    );
    s.error(
        &["export", "--html", "app", "another"],
        2,
        "argument",
    );
    s.error(&["export", "--html", "absent"], 2, "argument");

    // The pre-rename command says where it went instead of falling through to
    // the GUI on a path named "dash".
    let moved = s.error(&["dash", "export", "--html", "app"], 2, "argument");
    assert!(
        moved["error"]["message"].as_str().unwrap().contains("`export`"),
        "{moved}"
    );
    s.error(
        &["export", "--html", "app", "--timeout"],
        2,
        "argument",
    );
    for bad in ["abc", "0", "-5", "1.5"] {
        s.error(
            &["export", "--html", "app", "--timeout", bad],
            2,
            "argument",
        );
    }
    s.error(
        &[
            "export",
            "--html",
            "app/main.js",
            "--database",
            ":memory:",
        ],
        2,
        "argument",
    );

    // A folder that is not an app is refused by name, not loaded and failed.
    std::fs::create_dir_all(s.0.join("empty")).unwrap();
    let error = s.error(&["export", "--html", "empty"], 2, "argument");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("main.js"));

    // Help is help, and it is not an app.
    let help = s.text(&["export", "--help"]);
    assert!(help.contains("export --html"), "{help}");
}
