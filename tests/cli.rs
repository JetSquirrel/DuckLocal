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
        assert!(
            text.to_lowercase().contains("ducklocal") || text.contains(env!("CARGO_PKG_VERSION"))
        );
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
            {"encoding":"date","unit":"Day","value":"1","iso":"1970-01-02"},
            // `iso` is for reading, to the unit's precision; `value` is exact.
            {"encoding":"timestamp","unit":"Nanosecond","value":"123456789",
             "iso":"1970-01-01T00:00:00.123456789"},
            {"encoding":"time","unit":"Microsecond","value":"123456","iso":"00:00:00.123456"},
            {"encoding":"interval","months":1,"days":2,"nanos":"3000"},
            {"encoding":"hex","value":"00ff"},
            [null,"NULL"], {"encoding":"struct","fields":[["a",null],["b","NULL"]]},
            {"encoding":"map","entries":[[1,null],[2,"NULL"]]}, [1,2],
            {"encoding":"union-value","value":"NULL"},
            {"encoding":"float","value":"NaN"}, {"encoding":"float","value":"inf"},
            {"encoding":"float","value":"-inf"}, "red"
        ])
    );
    // A zoned timestamp is a UTC instant, and says so.
    let out = s.success(&[
        "query",
        "--sql",
        "SELECT TIMESTAMPTZ '2026-10-20 01:02:03+00' AS tz, TIMESTAMP '2026-10-20 01:02:03' AS ts",
    ]);
    assert_eq!(out["rows"][0][0]["iso"], "2026-10-20T01:02:03Z");
    assert_eq!(out["rows"][0][1]["iso"], "2026-10-20T01:02:03");
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
        assert_eq!(out["total_rows"], count);
        assert_eq!(out["total_rows_exact"], true);
    }
    let out = s.success(&["query", "--sql", "SELECT * FROM range(1001)"]);
    assert_eq!(out["row_count"], 1000);
    assert_eq!(out["truncated"], true);
    assert_eq!(out["total_rows"], 1001);
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
    assert_eq!(wide["total_rows"], 6000);
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
    let missing = s.error(
        &["query", "--database", "missing.duckdb", "--sql", "SELECT 1"],
        1,
        "database",
    );
    assert_eq!(
        missing["error"]["message"],
        "No database file at missing.duckdb"
    );
    assert!(missing["error"]["hint"]
        .as_str()
        .unwrap()
        .contains("--read-write"));
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
            // Numbers right-aligned; DuckDB types a bare NULL as INTEGER.
            "| --- | --- | ---: | --- | ---: | ---: |",
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
    assert!(
        truncated.contains("_Truncated at 2 of 5 rows"),
        "{truncated}"
    );

    // The name is the format's, however it is spelled.
    for spelling in ["MD", "markdown"] {
        let same = s.text(&["query", "--sql", "SELECT 1 AS n", "--format", spelling]);
        assert_eq!(
            same,
            "| n |
| ---: |
| 1 |
",
            "{spelling}"
        );
    }
    s.error(
        &["query", "--sql", "SELECT 1", "--format", "csv"],
        2,
        "argument",
    );

    let empty = s.text(&[
        "query",
        "--sql",
        "SELECT 1 AS n WHERE false",
        "--format",
        "md",
    ]);
    assert_eq!(empty, "| n |\n| ---: |\n_0 rows._\n");

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

/// A path written straight into a query's SQL starts from the spec's folder,
/// like a `source` path, so a folder of spec and data checks from anywhere;
/// `--summary` answers in one line; a keyword alias says what to write.
#[test]
fn check_reads_sql_paths_from_the_specs_folder_and_can_summarize() {
    let s = Sandbox::new();
    let proj = s.0.join("proj");
    std::fs::create_dir_all(proj.join("data")).unwrap();
    std::fs::write(proj.join("data/m.csv"), "metric,value\na,1\na,2\n").unwrap();
    let dash = |name: &str, from: &str, alias: &str| {
        std::fs::write(
            proj.join(name),
            format!(
                "query \"q\" {{\n  sql = <<SQL\n    SELECT metric, sum(value) {alias} FROM '{from}' GROUP BY metric\n  SQL\n}}\n\
                 plot \"q\" {{\n  type  = \"bar\"\n  query = query.q\n  x     = metric\n  y     = total\n}}\n"
            ),
        )
        .unwrap();
    };
    dash("d.dash", "data/m.csv", "AS total");

    // From the parent folder, and by an absolute path from a folder that is
    // neither: the same pass, in one line.
    let absolute = proj.join("d.dash");
    for spec in ["proj/d.dash", absolute.to_str().unwrap()] {
        let out = s.object(&["check", spec, "--summary"]);
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["queries"], 1);
        assert_eq!(out["plots"], 1);
        assert_eq!(out["unresolved"], serde_json::json!([]));
    }
    // Without --summary, the full report, as before, with the rows each
    // query returned.
    let full = s.object(&["check", "proj/d.dash"]);
    assert_eq!(full["queries"][0]["columns"][1]["name"], "total");
    assert_eq!(full["queries"][0]["rows"], 1);
    assert_eq!(full["empty"], serde_json::json!([]));

    // A query that runs and matches nothing passes, and says so.
    std::fs::write(
        proj.join("none.dash"),
        "query \"q\" {\n  sql = <<SQL\n    SELECT metric FROM 'data/m.csv' WHERE metric ~ '^zzz'\n  SQL\n}\n\
         plot \"q\" {\n  type  = \"table\"\n  query = query.q\n}\n",
    )
    .unwrap();
    let out = s.object(&["check", "proj/none.dash", "--summary"]);
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["empty"], serde_json::json!(["q"]));
    let full = s.object(&["check", "proj/none.dash"]);
    assert_eq!(full["queries"][0]["rows"], 0);
    assert_eq!(full["empty"][0]["query"], "q");

    // A missing file still fails, with the hint.
    dash("missing.dash", "data/none.csv", "AS total");
    let error = s.error(&["check", "proj/missing.dash", "--summary"], 1, "sql");
    assert!(
        error["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("spec's folder"),
        "{error}"
    );

    // A keyword left as a bare alias names itself and the fix.
    dash("alias.dash", "data/m.csv", "minutes");
    let error = s.error(&["check", "proj/alias.dash"], 2, "spec");
    let hint = error["error"]["hint"].as_str().unwrap();
    assert!(
        hint.contains("\"minutes\"") && hint.contains("AS minutes"),
        "{hint}"
    );
    let error = s.error(&["query", "--sql", "SELECT 1 at"], 1, "sql");
    assert!(
        error["error"]["hint"].as_str().unwrap().contains("AS at"),
        "{error}"
    );
}

/// A `source` names data files beside the spec: its path is the `.dash`
/// file's folder's business, not the working directory's, and every query
/// that names it reads it — a `WITH` query, a `SUMMARIZE`, a plain `SELECT` —
/// with the database's own table of the same name shadowed for that query.
/// Cards and widths come back in the report.
#[test]
fn check_runs_sources_from_the_specs_folder() {
    let s = Sandbox::new();
    let dash = s.0.join("dash");
    std::fs::create_dir_all(dash.join("exports")).unwrap();
    std::fs::write(
        dash.join("exports/orders-1.csv"),
        "channel,amount\nweb,10\nshop,20\n",
    )
    .unwrap();
    std::fs::write(dash.join("exports/orders-2.csv"), "channel,amount\nweb,5\n").unwrap();
    std::fs::write(
        dash.join("board.dash"),
        r#"
source "orders" { path = "exports/orders-*.csv" }

query "total" { sql = "SELECT sum(amount) AS total, count(*) AS n FROM orders" }
query "by_channel" {
  sql = <<SQL
    -- a leading note
    WITH c AS (SELECT channel, amount FROM orders)
    SELECT channel, sum(amount) AS amount FROM c GROUP BY channel ORDER BY amount DESC
  SQL
}
query "shape" { sql = "SUMMARIZE orders;" }

plot "total"  { type = "card" query = query.total value = total title = "Revenue" }
plot "orders" { type = "card" query = query.total value = n }
plot "chart"  { type = "bar" query = query.by_channel x = channel y = amount width = 6 }
plot "stats"  { type = "table" query = query.shape width = 6 }
"#,
    )
    .unwrap();
    // The database has its own `orders`: the source shadows it.
    s.success(&[
        "query",
        "--database",
        "data.duckdb",
        "--read-write",
        "--sql",
        "CREATE TABLE orders AS SELECT 'nope' AS other",
    ]);

    // Run from the sandbox, not the spec's folder: the glob still resolves.
    let out = s.object(&["check", "dash/board.dash", "--database", "data.duckdb"]);
    assert_eq!(out["sources"][0]["name"], "orders");
    assert_eq!(out["sources"][0]["path"], "exports/orders-*.csv");
    let resolved = out["sources"][0]["resolved"].as_str().unwrap();
    // A native path: `\` on Windows.
    assert!(
        resolved
            .replace('\\', "/")
            .ends_with("dash/exports/orders-*.csv"),
        "{resolved}"
    );
    assert!(std::path::Path::new(resolved).is_absolute(), "{resolved}");
    let total = &out["queries"][0]["columns"];
    assert_eq!(total[0]["name"], "total");
    assert_eq!(out["queries"][1]["columns"][0]["name"], "channel");
    assert_eq!(out["queries"][2]["columns"][0]["name"], "column_name");
    assert_eq!(out["plots"][0]["type"], "card");
    assert_eq!(out["plots"][0]["value"], "total");
    assert_eq!(out["plots"][0]["width"], 3);
    assert_eq!(out["plots"][2]["width"], 6);

    // A card's value is checked against the columns like any other.
    std::fs::write(
        dash.join("badcard.dash"),
        "source \"orders\" { path = \"exports/orders-*.csv\" }\nquery \"q\" { sql = \"FROM orders\" }\nplot \"c\" { type = \"card\" query = query.q value = revenue }\n",
    )
    .unwrap();
    let error = s.error(
        &["check", "dash/badcard.dash", "--database", "data.duckdb"],
        2,
        "spec",
    );
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("value = \"revenue\""), "{message}");

    // A path that matches nothing fails when the query runs — in memory as
    // on a database.
    std::fs::write(
        dash.join("gone.dash"),
        "source \"o\" { path = \"missing/*.csv\" }\nquery \"q\" { sql = \"FROM o\" }\n",
    )
    .unwrap();
    s.error(&["check", "dash/gone.dash"], 1, "sql");
    s.error(
        &["check", "dash/gone.dash", "--database", "data.duckdb"],
        1,
        "sql",
    );

    // In memory, the sources are all there is: a query over them runs and its
    // columns are checked, and one over a table no source defines is
    // unresolved — perhaps a database's — rather than failed.
    let out = s.object(&["check", "dash/board.dash"]);
    assert_eq!(out["queries"][1]["columns"][0]["name"], "channel");
    assert_eq!(out["unresolved"].as_array().unwrap().len(), 0);
    std::fs::write(
        dash.join("mixed.dash"),
        "source \"orders\" { path = \"exports/orders-*.csv\" }\nquery \"a\" { sql = \"FROM orders\" }\nquery \"b\" { sql = \"FROM warehouse_sales\" }\nplot \"p\" { type = \"table\" query = query.b }\n",
    )
    .unwrap();
    let out = s.object(&["check", "dash/mixed.dash"]);
    let unresolved = out["unresolved"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0]["query"], "b");
    assert!(out["queries"][1].get("columns").is_none());
    s.error(
        &["check", "dash/mixed.dash", "--database", "data.duckdb"],
        1,
        "sql",
    );

    // Statically: a workbook, a name SQL cannot read bare, a width off the grid.
    std::fs::write(
        dash.join("bad.dash"),
        r#"
source "book" { path = "sales.xlsx" }
source "2026 orders" { path = "o.csv" }
query "q" { sql = "SELECT 1 AS a" }
plot "p" { type = "bar" query = query.q x = a y = a width = 13 }
plot "c" { type = "card" query = query.q x = a }
"#,
    )
    .unwrap();
    let error = s.error(&["check", "dash/bad.dash"], 2, "spec");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("bad.dash:2: A workbook"), "{message}");
    assert!(
        message.contains("bad.dash:3: source \"2026 orders\""),
        "{message}"
    );
    assert!(message.contains("bad.dash:5: width"), "{message}");
    assert!(
        message.contains("bad.dash:6: A card shows one value"),
        "{message}"
    );
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
    let error = s.error(
        &["check", "bad.dash", "--database", "data.duckdb"],
        2,
        "spec",
    );
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("\"nope\""), "{message}");
    assert!(message.contains("not numeric"), "{message}");
    assert!(message.contains("bad.dash:5"), "{message}");
    assert!(message.contains("bad.dash:6"), "{message}");

    // Mistakes of different stages come back together: a `$name` no filter
    // defines, a card written as a block of its own, and a column a sound
    // query does not return — not the first stage alone, run after run.
    std::fs::write(
        s.0.join("mixed.dash"),
        r#"
query "revenue" {
  sql = "SELECT channel, sum(amount) AS total FROM usage WHERE $region GROUP BY channel"
}
plot "a" { type = "bar" query = query.revenue x = channel y = totl }
card "b" { query = query.revenue value = total }
"#,
    )
    .unwrap();
    let error = s.error(
        &["check", "mixed.dash", "--database", "data.duckdb"],
        2,
        "spec",
    );
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("$region"), "{message}");
    assert!(message.contains("\"totl\""), "{message}");
    assert!(
        message.contains(r#"a card is a plot: plot "b" { type = "card""#),
        "{message}"
    );

    // A pie and a map check against the same database: the map's
    // coordinates are found by name, and a text latitude is a mistake.
    s.success(&[
        "query",
        "--database",
        "data.duckdb",
        "--read-write",
        "--sql",
        "CREATE TABLE stations AS SELECT * FROM (VALUES
             ('Utrecht', 'NL', 52.09, 5.11), ('Aachen', 'D', 50.77, 6.09)
         ) t(name, country, geo_lat, geo_lng)",
    ]);
    std::fs::write(
        s.0.join("geo.dash"),
        r#"
query "stations" { sql = "SELECT * FROM stations" }
query "share" {
  sql = "SELECT channel, sum(amount) AS total FROM usage GROUP BY channel"
}
plot "where" { type = "map" query = query.stations color = country }
plot "share" { type = "pie" query = query.share x = channel y = total }
"#,
    )
    .unwrap();
    let out = s.object(&["check", "geo.dash", "--database", "data.duckdb"]);
    assert_eq!(out["plots"][0]["type"], "map");
    assert_eq!(out["plots"][0]["color"], "country");
    assert_eq!(out["plots"][1]["type"], "pie");
    std::fs::write(
        s.0.join("badgeo.dash"),
        r#"
query "stations" { sql = "SELECT * FROM stations" }
plot "where" { type = "map" query = query.stations lat = name }
"#,
    )
    .unwrap();
    let error = s.error(
        &["check", "badgeo.dash", "--database", "data.duckdb"],
        2,
        "spec",
    );
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("lat = \"name\""), "{message}");
    assert!(message.contains("not numeric"), "{message}");

    // Everything wrong before a database opens is said without opening one:
    // syntax, unknown blocks, dangling references, multi-statement SQL.
    s.error(&["check"], 2, "argument");
    s.error(&["check", "missing.dash"], 1, "io");
    s.error(&["check", "dashboard.dash", "extra.dash"], 2, "argument");
    std::fs::write(s.0.join("syntax.dash"), "plot {\n").unwrap();
    s.error(&["check", "syntax.dash"], 2, "spec");
    std::fs::write(
        s.0.join("dangling.dash"),
        "plot \"p\" { type = \"line\" query = query.ghost x = a y = b }",
    )
    .unwrap();
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
    std::fs::write(
        s.0.join("absent.dash"),
        "query \"q\" { sql = \"SELECT * FROM absent\" }",
    )
    .unwrap();
    s.error(
        &["check", "absent.dash", "--database", "data.duckdb"],
        1,
        "sql",
    );

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
    let error = s.error(
        &["check", "runtime.dash", "--database", "data.duckdb"],
        1,
        "sql",
    );
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("runtime.dash:5: query \"cast\""),
        "{message}"
    );
    assert!(
        message.contains("runtime.dash:8: query \"cast_again\""),
        "{message}"
    );
    assert!(message.contains("Conversion Error"), "{message}");
    assert!(!message.contains("query \"ok\""), "{message}");
    // Without a database nothing runs, so the same file checks clean.
    s.object(&["check", "runtime.dash"]);

    // Help is help, and it is not a spec.
    let help = s.text(&["check", "--help"]);
    assert!(help.contains(".dash"), "{help}");
}

#[test]
fn open_checks_its_arguments_and_never_starts_a_window_it_was_told_not_to() {
    let s = Sandbox::new();
    let output = s.run(&["open", "--help"], None);
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("--no-launch"));
    for args in [
        vec!["open"],
        vec!["open", "--no-launch"],
        vec!["open", "--run", "a.csv"],
        vec!["open", "--title", "x", "a.csv"],
        vec!["open", "--sql", "  "],
        vec!["open", "--sql", "SELECT 1", "--sql-file", "x.sql"],
        vec!["open", "--sql", "SELECT 1", "--wat"],
        vec!["open", "--sql", "SELECT 1", "--run", "--run"],
    ] {
        s.error(&args, 2, "argument");
    }
    s.error(&["open", "--sql-file", "missing.sql"], 1, "io");
    // The sandbox's HOME has no endpoint, so no window is reachable — and the
    // command writes nothing there finding that out (`run` checks).
    s.error(
        &["open", "--no-launch", "--sql", "SELECT 1", "--run"],
        1,
        "not_running",
    );
    s.error(&["open", "--no-launch", "sales.dash"], 1, "not_running");
}

#[test]
fn schema_lists_files_and_databases_sized_for_a_prompt() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.0.join("data/nested")).unwrap();
    std::fs::write(
        s.0.join("data/orders.csv"),
        "channel,amount\nweb,10\nshop,20\n",
    )
    .unwrap();
    std::fs::write(s.0.join("data/nested/notes.txt"), "a,b\n1,2\n").unwrap();
    s.success(&[
        "query",
        "--sql",
        "COPY (SELECT range AS id FROM range(7)) TO 'data/ids.parquet'",
    ]);

    // A folder expands as a drop on the window would; every file is whole.
    let out = s.object(&["schema", "data"]);
    assert_eq!(out["detail"], "full");
    let relations = out["relations"].as_array().unwrap();
    // Names are native relative paths, `\`-separated on Windows.
    let names: Vec<String> = relations
        .iter()
        .map(|r| r["name"].as_str().unwrap().replace('\\', "/"))
        .collect();
    assert_eq!(
        names,
        [
            "data/ids.parquet",
            "data/nested/notes.txt",
            "data/orders.csv"
        ]
    );
    // Parquet knows its row count; a CSV would need a scan.
    assert_eq!(relations[0]["rows"], 7);
    assert!(relations[2]["rows"].is_null());
    let from = |ix: usize| relations[ix]["from"].as_str().unwrap().replace('\\', "/");
    assert_eq!(from(2), "'data/orders.csv'");
    assert_eq!(relations[2]["columns"][1]["name"], "amount");
    // A .txt is read as CSV only when asked to.
    assert_eq!(from(1), "read_csv_auto('data/nested/notes.txt')");
    // `from` is SQL that works as given.
    let rows = s.success(&[
        "query",
        "--sql",
        &format!(
            "SELECT count(*) AS n FROM {}",
            relations[1]["from"].as_str().unwrap()
        ),
    ]);
    assert_eq!(rows["rows"][0][0], 1);

    // A database past the threshold keeps the largest few whole.
    let mut ddl = vec!["CREATE TABLE big AS SELECT range AS id FROM range(1000)".to_string()];
    for i in 0..14 {
        ddl.push(format!("CREATE TABLE t{i} (a INTEGER, b VARCHAR)"));
    }
    for sql in &ddl {
        s.success(&[
            "query",
            "--database",
            "w.duckdb",
            "--read-write",
            "--sql",
            sql,
        ]);
    }
    let out = s.object(&["schema", "--database", "w.duckdb"]);
    assert_eq!(out["detail"], "summary");
    assert_eq!(out["without_columns"], 10);
    assert!(out["hint"].as_str().unwrap().contains("--table"));
    let big = out["relations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "big")
        .unwrap();
    assert_eq!(big["columns"][0]["name"], "id");
    // A database named as a PATH is the same request.
    let one = s.object(&["schema", "w.duckdb", "--table", "t3"]);
    assert_eq!(one["relations"][0]["columns"][1]["type"], "VARCHAR");
    assert_eq!(
        s.object(&["schema", "w.duckdb", "--full"])["without_columns"],
        0
    );

    s.error(&["schema"], 2, "argument");
    s.error(&["schema", "missing.csv"], 2, "argument");
    let error = s.error(&["schema", "w.duckdb", "--table", "nope"], 2, "argument");
    assert!(error["error"]["hint"].as_str().is_some());
    // An empty file has no columns, not DuckDB's stand-in column0.
    std::fs::write(s.0.join("empty.csv"), "").unwrap();
    let empty = s.object(&["schema", "empty.csv"]);
    assert_eq!(empty["relations"][0]["rows"], 0);
    assert_eq!(empty["relations"][0]["column_count"], 0);
    assert_eq!(
        empty["relations"][0]["error"],
        "The file is empty (0 bytes)"
    );
    // A slip of a name is answered with the name.
    let error = s.error(&["schema", "w.duckdb", "--table", "bigg"], 2, "argument");
    assert_eq!(error["error"]["hint"], "Did you mean \"big\"?");
}

/// The CLI speaks English whatever language the window is in, including the
/// file problems it shares with the GUI's sidebar.
#[test]
fn errors_are_worded_in_english() {
    let s = Sandbox::new();
    let error = s.error(&["schema", "missing.csv"], 2, "argument");
    assert_eq!(error["error"]["message"], "File not found: missing.csv");
    let error = s.error(
        &["query", "--sql", "SELECT 1", "--limit", "0"],
        2,
        "argument",
    );
    assert_eq!(
        error["error"]["message"],
        "--limit must be a whole number of rows, 1 or more; got \"0\""
    );
}

/// A glob over files of one format is one relation, as a `.dash` source reads
/// it; over mixed formats, its files are listed one by one.
#[test]
fn schema_reads_a_glob_as_one_relation() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.0.join("logs")).unwrap();
    for day in 1..=3 {
        std::fs::write(
            s.0.join(format!("logs/{day}.json")),
            format!("{{\"day\": {day}, \"msg\": \"m\"}}\n"),
        )
        .unwrap();
    }

    let out = s.object(&["schema", "logs/*.json"]);
    let relations = out["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{out}");
    assert_eq!(relations[0]["kind"], "glob");
    assert_eq!(relations[0]["files"], 3);
    assert_eq!(relations[0]["columns"][0]["name"], "day");
    let from = relations[0]["from"].as_str().unwrap();
    let rows = s.success(&["query", "--sql", &format!("SELECT count(*) FROM {from}")]);
    assert_eq!(rows["rows"][0][0], 3);
    let stats = s.object(&["schema", "logs/*.json", "--stats"]);
    assert_eq!(stats["row_count"], 3);

    std::fs::write(s.0.join("logs/extra.csv"), "a\n1\n").unwrap();
    let out = s.object(&["schema", "logs/*"]);
    assert_eq!(out["relations"].as_array().unwrap().len(), 4, "{out}");
    assert_eq!(out["relations"][0]["kind"], "file");
}

#[test]
fn errors_say_what_to_do_next() {
    let s = Sandbox::new();
    let error = s.error(&["query", "--limt", "5"], 2, "argument");
    assert_eq!(error["error"]["hint"], "Did you mean --limit?");
    let error = s.error(&["query", "--sql", "SELECT 1; SELECT 2"], 2, "argument");
    assert!(error["error"]["hint"]
        .as_str()
        .unwrap()
        .contains("one statement"));
    let error = s.error(&["query", "--sql", "SELECT * FROM missing_table"], 1, "sql");
    assert!(error["error"]["hint"]
        .as_str()
        .unwrap()
        .contains("ducklocal schema"));
    s.success(&[
        "query",
        "--database",
        "w.duckdb",
        "--read-write",
        "--sql",
        "CREATE TABLE t(a INT)",
    ]);
    let error = s.error(
        &["query", "--database", "w.duckdb", "--sql", "DROP TABLE t"],
        1,
        "sql",
    );
    assert!(error["error"]["hint"]
        .as_str()
        .unwrap()
        .contains("--read-write"));
    // A syntax error has nothing to add to DuckDB's own message.
    let error = s.error(&["query", "--sql", "SELEC 1"], 1, "sql");
    assert!(error["error"].get("hint").is_none());
}

#[test]
fn presence_names_a_plot_about_to_be_added_only_when_editing() {
    let s = Sandbox::new();
    std::fs::write(
        s.0.join("d.dash"),
        r#"
query "q" { sql = "SELECT 1 AS n" }
plot "revenue" { type = "card" query = query.q }
"#,
    )
    .unwrap();
    // No window: delivered is false, which is not an error.
    let out = s.object(&[
        "presence",
        "d.dash",
        "--doing",
        "editing",
        "--plot",
        "revenue,share",
    ]);
    assert_eq!(out["delivered"], false);
    let error = s.error(
        &[
            "presence", "d.dash", "--doing", "reading", "--plot", "share",
        ],
        2,
        "argument",
    );
    assert!(error["error"]["hint"].as_str().unwrap().contains("revenue"));
}

#[test]
fn open_state_reads_a_running_window_and_never_starts_one() {
    let s = Sandbox::new();
    let output = s.run(&["open", "--help"], None);
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("--state"));
    let error = s.error(&["open", "--state"], 1, "not_running");
    assert!(error["error"]["hint"]
        .as_str()
        .unwrap()
        .contains("ducklocal open"));
    // It reads; mixed with something to open it is a mistake, not a guess.
    s.error(&["open", "--state", "--sql", "SELECT 1"], 2, "argument");
    // One spec narrows what it reads, as does --active; not both, not two.
    s.error(&["open", "--state", "a.dash"], 1, "not_running");
    s.error(&["open", "--state", "--active"], 1, "not_running");
    s.error(&["open", "--state", "--no-launch"], 1, "not_running");
    s.error(&["open", "--state", "a.dash", "b.dash"], 2, "argument");
    s.error(&["open", "--state", "--active", "a.dash"], 2, "argument");
    s.error(&["open", "--active"], 2, "argument");
}

/// Each attempt names what it would touch, and afterwards the files say
/// whether it happened: an exit code alone could not tell a refused write
/// from one that went through and then failed.
#[test]
fn restricted_sql_reaches_only_what_it_was_allowed() {
    let s = Sandbox::new();
    let allowed = s.0.join("allowed");
    let beside = s.0.join("allowed_not");
    std::fs::create_dir_all(&allowed).unwrap();
    std::fs::create_dir_all(&beside).unwrap();
    std::fs::write(allowed.join("in.csv"), "n\n1\n").unwrap();
    std::fs::write(allowed.join("other.csv"), "n\n2\n").unwrap();
    std::fs::write(beside.join("secret.csv"), "n\n99\n").unwrap();

    let query = |sql: &str, allow: &str| -> Vec<String> {
        ["query", "--restricted", "--allow-read", allow, "--sql", sql]
            .map(str::to_string)
            .to_vec()
    };
    let run = |args: Vec<String>| s.run(&args.iter().map(String::as_str).collect::<Vec<_>>(), None);
    let denied = |sql: &str| {
        let output = run(query(sql, "allowed"));
        assert_eq!(output.status.code(), Some(1), "{sql}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["kind"], "policy_denied", "{sql}: {error}");
        assert!(error["error"]["hint"].is_string(), "{sql}: {error}");
        error
    };

    // Allowed: by relative path, by absolute path, by glob, and a shape that
    // only parses as a SELECT once DuckDB has read it.
    let inside = run(query("SELECT n FROM 'allowed/in.csv'", "allowed"));
    let inside = s.decode_success(inside);
    assert_eq!(inside["rows"], json!([[1]]));
    assert_eq!(inside["restricted"]["timeout_seconds"], 60);
    assert_eq!(inside["restricted"]["memory_limit"], "2GB");
    assert_eq!(
        inside["restricted"]["allow_read"].as_array().unwrap().len(),
        1
    );
    let absolute = allowed.join("in.csv");
    let sql = format!("SELECT n FROM '{}'", absolute.to_string_lossy());
    assert_eq!(
        s.decode_success(run(query(&sql, "allowed")))["rows"],
        json!([[1]])
    );
    let glob = s.decode_success(run(query(
        "SELECT count(*) AS c FROM 'allowed/*.csv'",
        "allowed",
    )));
    assert_eq!(glob["rows"], json!([[2]]));
    s.decode_success(run(query(
        "SELECT * FROM (PIVOT 'allowed/in.csv' ON n IN (1) USING count(*))",
        "allowed",
    )));
    // One file allowed is not its folder.
    let one = run(query("SELECT n FROM 'allowed/other.csv'", "allowed/in.csv"));
    assert_eq!(one.status.code(), Some(1));
    s.decode_success(run(query(
        "SELECT n FROM 'allowed/in.csv'",
        "allowed/in.csv",
    )));

    // Reads outside it.
    for sql in [
        "SELECT * FROM 'allowed_not/secret.csv'",
        "SELECT * FROM 'allowed/../allowed_not/secret.csv'",
        "SELECT * FROM '*/secret.csv'",
        "SELECT * FROM read_text('allowed_not/secret.csv')",
        "SELECT * FROM glob('allowed_not/*')",
    ] {
        denied(sql);
    }
    // The network: refused before anything connects, so this needs none.
    denied("SELECT * FROM 'https://example.com/x.csv'");
    denied("SELECT * FROM 's3://example/x.csv'");
    // Extensions, settings, and persistent secrets.
    denied("INSTALL httpfs");
    denied("LOAD httpfs");
    denied("SET enable_external_access = true");
    denied("SET lock_configuration = false");
    denied("SELECT count(*) FROM duckdb_secrets()");
    // Writes, inside what it may read as well as outside.
    let before = std::fs::read(allowed.join("in.csv")).unwrap();
    for sql in [
        "COPY (SELECT 1) TO 'written.csv'",
        "COPY (SELECT 1) TO 'allowed/written.csv'",
        "COPY (SELECT 42 AS n) TO 'allowed/in.csv'",
        "COPY 'allowed/in.csv' TO 'allowed_not/written.csv'",
        "EXPORT DATABASE 'allowed/exported'",
        "ATTACH 'allowed/new.duckdb'",
        "EXPLAIN ANALYZE COPY (SELECT 1) TO 'allowed/written.csv'",
        "CREATE TABLE t AS SELECT 1",
        "PRAGMA version",
    ] {
        denied(sql);
    }
    assert_eq!(std::fs::read(allowed.join("in.csv")).unwrap(), before);
    assert!(!s.0.join("written.csv").exists());
    assert!(!allowed.join("written.csv").exists());
    assert!(!allowed.join("new.duckdb").exists());
    assert!(!allowed.join("exported").exists());
    assert!(!beside.join("written.csv").exists());

    // The same statements run without the flag, which stays the way to do
    // what a person has approved.
    s.decode_success(s.run(
        &["query", "--sql", "SELECT * FROM 'allowed_not/secret.csv'"],
        None,
    ));
}

#[test]
fn restricted_leaves_a_database_file_as_it_was() {
    let s = Sandbox::new();
    let database = s.0.join("app.duckdb");
    let database_text = database.to_string_lossy().to_string();
    s.run(
        &[
            "query",
            "--database",
            &database_text,
            "--read-write",
            "--sql",
            "CREATE TABLE t AS SELECT 1 AS n",
        ],
        None,
    );
    let before = std::fs::read(&database).unwrap();
    let base = [
        "query",
        "--restricted",
        "--database",
        &database_text,
        "--sql",
    ];
    let read = s.run(&[&base[..], &["SELECT n FROM t"]].concat(), None);
    assert_eq!(s.decode_success(read)["rows"], json!([[1]]));
    for sql in ["INSERT INTO t VALUES (2)", "DROP TABLE t", "CHECKPOINT"] {
        let output = s.run(&[&base[..], &[sql]].concat(), None);
        assert_eq!(output.status.code(), Some(1), "{sql}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["kind"], "policy_denied", "{sql}");
    }
    assert_eq!(std::fs::read(&database).unwrap(), before);
}

/// A dashboard under `reports/` over `data/`, with `private/` beside them, and
/// `reports/data/`, which a path in its SQL finds through the search path.
fn restricted_check_folder(s: &Sandbox) {
    for dir in ["reports/data", "data", "private"] {
        std::fs::create_dir_all(s.0.join(dir)).unwrap();
    }
    std::fs::write(s.0.join("data/in.csv"), "n\n1\n2\n").unwrap();
    std::fs::write(s.0.join("private/secret.csv"), "n\n99\n").unwrap();
    std::fs::write(s.0.join("reports/data/beside.csv"), "n\n98\n").unwrap();
}

fn check_report(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn restricted_check_spec(s: &Sandbox, name: &str, sql: &str) -> String {
    let spec = format!(
        r#"
source "numbers" {{ path = "../data/in.csv" }}
query "mine" {{ sql = "SELECT n FROM numbers" }}
query "theirs" {{
  sql = <<SQL
    {sql}
  SQL
}}
plot "a" {{ type = "table" query = query.mine }}
plot "b" {{ type = "table" query = query.theirs }}
"#
    );
    let path = format!("reports/{name}.dash");
    std::fs::write(s.0.join(&path), spec).unwrap();
    path
}

#[test]
fn check_restricted_runs_a_dashboard_inside_what_it_was_allowed() {
    let s = Sandbox::new();
    restricted_check_folder(&s);
    let args = |spec: &str| {
        ["check", spec, "--restricted", "--allow-read", "data"]
            .map(str::to_string)
            .to_vec()
    };
    let run = |args: &[String]| s.run(&args.iter().map(String::as_str).collect::<Vec<_>>(), None);

    // A source from the spec's folder, and a query reading what it can see.
    let spec = restricted_check_spec(&s, "fine", "SELECT n FROM numbers");
    let report = check_report(run(&args(&spec)));
    assert_eq!(report["queries"][0]["rows"], 2);
    assert_eq!(report["queries"][1]["rows"], 2);
    assert_eq!(report["restricted"]["timeout_seconds"], 60);
    assert_eq!(
        report["restricted"]["allow_read"].as_array().unwrap().len(),
        1
    );
    let mut summary = args(&spec);
    summary.push("--summary".to_string());
    let summary = check_report(run(&summary));
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["restricted"]["memory_limit"], "2GB");

    // Nothing is allowed unless named, not even beside the spec: the failure
    // lists both queries, and the check does not run them some other way.
    let error = s.error(&["check", &spec, "--restricted"], 1, "policy_denied");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("\"mine\""), "{message}");
    assert!(message.contains("\"theirs\""), "{message}");
    assert!(error["error"]["hint"].is_string());

    // A path in the SQL starts from the spec's folder, when that is allowed.
    let beside = restricted_check_spec(&s, "beside", "SELECT n FROM 'data/beside.csv'");
    let report = check_report(s.run(
        &[
            "check",
            &beside,
            "--restricted",
            "--allow-read",
            "data",
            "--allow-read",
            "reports/data",
        ],
        None,
    ));
    assert_eq!(report["queries"][1]["rows"], 1);

    // Without the flag it is the check it always was, secret and all.
    let open = restricted_check_spec(&s, "open", "SELECT n FROM '../private/secret.csv'");
    let report = check_report(s.run(&["check", &open], None));
    assert_eq!(report["queries"][1]["rows"], 1);
}

#[test]
fn check_restricted_refuses_what_a_dashboard_should_not_reach() {
    let s = Sandbox::new();
    restricted_check_folder(&s);
    let check = |spec: &str, code: i32, kind: &str| {
        s.error(
            &["check", spec, "--restricted", "--allow-read", "data"],
            code,
            kind,
        )
    };
    for (name, sql) in [
        ("outside", "SELECT n FROM '../private/secret.csv'"),
        // Allowed by its spelling from here; the file is the one the search
        // path finds, and that is the one DuckDB has to judge.
        ("searched", "SELECT n FROM 'data/beside.csv'"),
        ("searched_glob", "SELECT n FROM 'data/bes*.csv'"),
        ("through", "SELECT n FROM '../data/../private/secret.csv'"),
        ("glob", "SELECT * FROM glob('../private/*')"),
        (
            "text",
            "SELECT content FROM read_text('../private/secret.csv')",
        ),
        ("http", "SELECT * FROM 'https://example.com/x.csv'"),
        ("s3", "SELECT * FROM 's3://example/x.csv'"),
        ("secrets", "SELECT * FROM duckdb_secrets()"),
    ] {
        let spec = restricted_check_spec(&s, name, sql);
        let error = check(&spec, 1, "policy_denied");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains("\"theirs\""), "{sql}: {message}");
        // The query that stayed inside is not part of what was refused.
        assert!(!message.contains("\"mine\""), "{sql}: {message}");
    }

    // Refused by the same gate as without the flag, before anything runs: a
    // spec's writes are its mistake, and nothing lands.
    for (name, sql) in [
        ("copy", "COPY (SELECT 1) TO '../data/written.csv'"),
        ("attach", "ATTACH '../data/written.duckdb'"),
        ("install", "INSTALL httpfs"),
        ("load", "LOAD httpfs"),
        ("set", "SET enable_external_access = true"),
    ] {
        let spec = restricted_check_spec(&s, name, sql);
        check(&spec, 2, "spec");
        assert!(!s.0.join("data/written.csv").exists(), "{sql}");
        assert!(!s.0.join("data/written.duckdb").exists(), "{sql}");
    }

    // The window draws a top-level PIVOT; run restricted, it is not a SELECT.
    let pivot = restricted_check_spec(&s, "pivot", "PIVOT '../data/in.csv' ON n USING count(*)");
    let error = check(&pivot, 1, "policy_denied");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("single SELECT"));
}

#[test]
fn check_restricted_leaves_a_database_file_as_it_was() {
    let s = Sandbox::new();
    restricted_check_folder(&s);
    s.success(&[
        "query",
        "--database",
        "new.duckdb",
        "--read-write",
        "--sql",
        "CREATE TABLE t AS SELECT 1 AS n",
    ]);
    let before = std::fs::read(s.0.join("new.duckdb")).unwrap();
    let spec = restricted_check_spec(&s, "db", "SELECT n FROM t");
    let report = check_report(s.run(
        &[
            "check",
            &spec,
            "--database",
            "new.duckdb",
            "--restricted",
            "--allow-read",
            "data",
        ],
        None,
    ));
    assert_eq!(report["queries"][1]["rows"], 1);
    assert_eq!(std::fs::read(s.0.join("new.duckdb")).unwrap(), before);
}

#[test]
fn check_restricted_stops_at_its_limits_and_says_which() {
    let s = Sandbox::new();
    restricted_check_folder(&s);
    let endless = restricted_check_spec(
        &s,
        "endless",
        "SELECT sum(i) AS n FROM range(100000000000000) t(i)",
    );
    let error = s.error(
        &[
            "check",
            &endless,
            "--restricted",
            "--allow-read",
            "data",
            "--timeout",
            "1",
        ],
        1,
        "timeout",
    );
    assert!(error["error"]["message"].as_str().unwrap().contains("1s"));

    let hungry = restricted_check_spec(
        &s,
        "hungry",
        "SELECT count(*) AS n FROM (SELECT repeat('x', 1000) s, i FROM range(5000000) t(i) ORDER BY s, i DESC)",
    );
    s.error(
        &[
            "check",
            &hungry,
            "--restricted",
            "--allow-read",
            "data",
            "--memory-limit",
            "20MB",
        ],
        1,
        "resource_limit",
    );
}

#[test]
fn check_restricted_flags_refuse_what_they_cannot_honour() {
    let s = Sandbox::new();
    restricted_check_folder(&s);
    let spec = restricted_check_spec(&s, "flags", "SELECT 1 AS n");
    for flags in [
        &["--allow-read", "data"][..],
        &["--timeout", "5"],
        &["--memory-limit", "1GB"],
    ] {
        let mut args = vec!["check", spec.as_str()];
        args.extend_from_slice(flags);
        s.error(&args, 2, "argument");
    }
    for flags in [
        &["--restricted", "--allow-read", "no_such_folder"][..],
        &["--restricted", "--timeout", "0"],
        &["--restricted", "--memory-limit", "lots"],
    ] {
        let mut args = vec!["check", spec.as_str()];
        args.extend_from_slice(flags);
        s.error(&args, 2, "argument");
    }
}

#[test]
fn restricted_stops_at_its_limits_and_says_which() {
    let s = Sandbox::new();
    // Streams, so it holds next to nothing in memory: a join would reach the
    // memory limit first on a fast machine, and say resource_limit instead.
    let endless = "SELECT sum(i) FROM range(100000000000000) t(i)";
    let error = s.error(
        &["query", "--restricted", "--timeout", "1", "--sql", endless],
        1,
        "timeout",
    );
    assert!(error["error"]["message"].as_str().unwrap().contains("1s"));

    let hungry = "SELECT count(*) FROM (SELECT repeat('x', 1000) s, i FROM range(5000000) t(i) \
                  ORDER BY s, i DESC)";
    s.error(
        &[
            "query",
            "--restricted",
            "--memory-limit",
            "20MB",
            "--sql",
            hungry,
        ],
        1,
        "resource_limit",
    );
    let quick = "SELECT 1 AS n";
    let limited = s.success(&[
        "query",
        "--restricted",
        "--timeout",
        "5",
        "--memory-limit",
        "512MB",
        "--sql",
        quick,
    ]);
    assert_eq!(limited["restricted"]["timeout_seconds"], 5);
    assert_eq!(limited["restricted"]["memory_limit"], "512MB");
}

#[test]
fn restricted_flags_refuse_what_they_cannot_honour() {
    let s = Sandbox::new();
    let sql = "SELECT 1";
    // The limits mean nothing without the profile: silently unlimited is the
    // one thing they must not be.
    s.error(&["query", "--allow-read", ".", "--sql", sql], 2, "argument");
    s.error(&["query", "--timeout", "5", "--sql", sql], 2, "argument");
    s.error(
        &["query", "--memory-limit", "1GB", "--sql", sql],
        2,
        "argument",
    );
    s.error(
        &[
            "query",
            "--restricted",
            "--allow-read",
            "no_such_folder",
            "--sql",
            sql,
        ],
        2,
        "argument",
    );
    s.error(
        &["query", "--restricted", "--timeout", "0", "--sql", sql],
        2,
        "argument",
    );
    s.error(
        &["query", "--restricted", "--timeout", "soon", "--sql", sql],
        2,
        "argument",
    );
    s.error(
        &[
            "query",
            "--restricted",
            "--memory-limit",
            "1GB'; COPY",
            "--sql",
            sql,
        ],
        2,
        "argument",
    );
    std::fs::write(s.0.join("app.duckdb"), b"").unwrap();
    s.error(
        &[
            "query",
            "--restricted",
            "--database",
            "app.duckdb",
            "--read-write",
            "--sql",
            sql,
        ],
        2,
        "argument",
    );
    // --allow-read repeats; other flags still may not.
    std::fs::create_dir_all(s.0.join("a")).unwrap();
    std::fs::create_dir_all(s.0.join("b")).unwrap();
    let both = s.success(&[
        "query",
        "--restricted",
        "--allow-read",
        "a",
        "--allow-read",
        "b",
        "--sql",
        sql,
    ]);
    assert_eq!(
        both["restricted"]["allow_read"].as_array().unwrap().len(),
        2
    );
    s.error(
        &["query", "--restricted", "--restricted", "--sql", sql],
        2,
        "argument",
    );
}

#[test]
fn open_tracks_a_request_only_against_a_window_that_is_there() {
    let s = Sandbox::new();
    let help = s.run(&["open", "--help"], None);
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("--if-revision") && help.contains("--wait"));
    // A window started now cannot show the revision the caller read: this
    // fails rather than launching one, with no --no-launch asked for.
    s.error(
        &["open", "--sql", "SELECT 1", "--if-revision", "abc"],
        1,
        "not_running",
    );
    s.error(
        &["open", "--no-launch", "--wait", "--sql", "SELECT 1"],
        1,
        "not_running",
    );
    // They qualify something to open: alone, or on a read, they are mistakes.
    s.error(&["open", "--wait"], 2, "argument");
    s.error(&["open", "--if-revision", "abc"], 2, "argument");
    s.error(&["open", "--state", "--wait"], 2, "argument");
    s.error(&["open", "--state", "--if-revision", "abc"], 2, "argument");
}

#[test]
fn propose_stages_a_change_and_only_the_window_applies_it() {
    let s = Sandbox::new();
    let spec = "query \"q\" {\n  sql = <<SQL\nSELECT 1 AS n\nSQL\n}\n";
    let new = "query \"q\" {\n  sql = <<SQL\nSELECT 2 AS n\nSQL\n}\n";
    std::fs::write(s.0.join("a.dash"), spec).unwrap();
    std::fs::write(s.0.join("new.dash"), new).unwrap();
    std::fs::write(s.0.join("broken.dash"), "query \"q\" {").unwrap();
    let json = |output: Output| -> Value {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };

    let none = json(s.run(&["propose", "a.dash", "--status"], None));
    assert_eq!(none["status"], "none");
    let hash = none["current_hash"].as_str().unwrap().to_string();

    let staged = json(s.run(
        &[
            "propose",
            "a.dash",
            "--from",
            "new.dash",
            "--note",
            "two",
            "--based-on",
            &hash,
        ],
        None,
    ));
    assert_eq!(staged["status"], "pending");
    assert_eq!(staged["stale"], false);
    assert_eq!(staged["base_hash"], hash.as_str());
    assert_eq!(staged["note"], "two");
    assert_eq!(staged["check"]["ok"], true);
    assert_eq!(std::fs::read_to_string(s.0.join("a.dash")).unwrap(), spec);

    // A malformed spec is staged, and the check says what is wrong with it.
    let broken = json(s.run(&["propose", "a.dash", "--from", "broken.dash"], None));
    assert_eq!(broken["status"], "pending");
    assert_eq!(broken["check"]["ok"], false);
    assert!(broken["check"]["failure"].is_string());
    assert_ne!(broken["id"], staged["id"]);
    // From stdin, too.
    let piped = json(s.run(&["propose", "a.dash", "--from", "-"], Some(new.as_bytes())));
    assert_eq!(piped["check"]["ok"], true);
    assert_eq!(std::fs::read_to_string(s.0.join("a.dash")).unwrap(), spec);

    // Someone edits the spec: the proposal reads as stale, and a new one
    // based on the old hash is refused.
    std::fs::write(s.0.join("a.dash"), format!("{spec}\n")).unwrap();
    let status = json(s.run(&["propose", "a.dash", "--status"], None));
    assert_eq!(status["status"], "pending");
    assert_eq!(status["stale"], true);
    assert_ne!(status["current_hash"], hash.as_str());
    let error = s.error(
        &[
            "propose",
            "a.dash",
            "--from",
            "new.dash",
            "--based-on",
            &hash,
        ],
        1,
        "stale",
    );
    assert!(error["error"]["hint"].is_string());

    // Nothing here applies: the flag does not exist.
    s.error(&["propose", "a.dash", "--apply"], 2, "argument");
    // Mistakes.
    s.error(&["propose", "a.dash"], 2, "argument");
    s.error(&["propose", "--status"], 2, "argument");
    s.error(&["propose", "a.dash", "b.dash", "--status"], 2, "argument");
    s.error(
        &["propose", "a.dash", "--status", "--note", "x"],
        2,
        "argument",
    );
    s.error(&["propose", "absent.dash", "--from", "new.dash"], 1, "io");
    s.error(&["propose", "a.dash", "--from", "absent.dash"], 1, "io");
    std::fs::write(s.0.join("same.dash"), format!("{spec}\n")).unwrap();
    s.error(&["propose", "a.dash", "--from", "same.dash"], 2, "argument");
    let help = s.run(&["propose", "--help"], None);
    assert!(String::from_utf8(help.stdout)
        .unwrap()
        .contains("--based-on"));
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

fn project_report(s: &Sandbox, dir: &str) -> Value {
    let output = s.run(&["project", "check", dir], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The shipped example, moved: what the issue means by portable.
#[test]
fn a_project_moves_as_a_folder_and_says_what_a_move_broke() {
    let s = Sandbox::new();
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/portable_project");
    copy_tree(&example, &s.0.join("moved/somewhere/else"));
    let dir = "moved/somewhere/else";

    let report = project_report(&s, dir);
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["portable"], true, "{report}");
    assert_eq!(report["problems"], json!([]));
    assert_eq!(report["dashboards"][0]["status"], "ok");
    assert_eq!(report["dashboards"][0]["checked"], true);
    assert_eq!(report["dashboards"][0]["sources"][0]["status"], "ok");
    assert!(report["duckdb"].is_string());
    assert!(report["limitations"][0]
        .as_str()
        .unwrap()
        .contains("not for holding the same data"));

    // The data did not come along: named, and the way out given.
    let data = s.0.join(dir).join("data/orders.csv");
    let moved = s.0.join(dir).join("data/2026.csv");
    std::fs::rename(&data, &moved).unwrap();
    let report = project_report(&s, dir);
    assert_eq!(report["ok"], false);
    assert_eq!(report["problems"][0]["kind"], "missing_source");
    assert!(report["problems"][0]["hint"]
        .as_str()
        .unwrap()
        .contains("relink --source orders"));
    assert_eq!(report["problems"].as_array().unwrap().len(), 1, "{report}");

    // Relinked: the dashboard says where it is now, relative, and still works.
    let spec = s.0.join(dir).join("reports/orders.dash");
    let before = std::fs::read_to_string(&spec).unwrap();
    let to = format!("{dir}/data/2026.csv");
    let relinked = s.run(
        &["project", "relink", dir, "--source", "orders", "--to", &to],
        None,
    );
    assert!(
        relinked.status.success(),
        "{}",
        String::from_utf8_lossy(&relinked.stderr)
    );
    let after = std::fs::read_to_string(&spec).unwrap();
    assert_eq!(
        after,
        before.replace("../data/orders.csv", "../data/2026.csv")
    );
    assert_eq!(
        std::fs::read_to_string(s.0.join(dir).join("reports/orders.dash.bak")).unwrap(),
        before
    );
    assert_eq!(project_report(&s, dir)["ok"], true);

    // Refused, with nothing changed.
    s.error(
        &[
            "project",
            "relink",
            dir,
            "--source",
            "orders",
            "--to",
            "absent.csv",
        ],
        2,
        "argument",
    );
    s.error(
        &["project", "relink", dir, "--source", "nope", "--to", &to],
        1,
        "not_found",
    );
    s.error(&["project", "relink", dir, "--to", &to], 2, "argument");
    assert_eq!(std::fs::read_to_string(&spec).unwrap(), after);
}

#[test]
fn a_project_check_reports_what_will_not_travel_or_is_not_here() {
    let s = Sandbox::new();
    let spec = |source: &str| {
        format!(
            "source \"d\" {{\n  path = \"{source}\"\n}}\n\nquery \"q\" {{\n  sql = \"SELECT * FROM d\"\n}}\n\n\
             plot \"p\" {{\n  type = \"table\"\n  query = query.q\n}}\n"
        )
    };
    std::fs::create_dir_all(s.0.join("p")).unwrap();
    std::fs::write(s.0.join("p/a.csv"), "n\n1\n").unwrap();
    std::fs::write(s.0.join("outside.csv"), "n\n1\n").unwrap();
    let absolute = s.0.join("outside.csv").to_string_lossy().replace('\\', "/");
    std::fs::write(s.0.join("p/absolute.dash"), spec(&absolute)).unwrap();
    std::fs::write(s.0.join("p/above.dash"), spec("../outside.csv")).unwrap();
    std::fs::write(
        s.0.join("p/remote.dash"),
        "query \"q\" {\n  sql = \"SELECT * FROM 'https://example.com/x.csv'\"\n}\n",
    )
    .unwrap();
    std::fs::write(s.0.join("p/broken.dash"), "query \"q\" {").unwrap();
    std::fs::write(
        s.0.join("p/ducklocal-project.json"),
        json!({
            "version": 1,
            "name": "t",
            "dashboards": [
                "absolute.dash", "above.dash", "remote.dash", "broken.dash",
                "absent.dash", "../escape.dash",
            ],
            "data": [
                {"name": "here", "path": "a.csv"},
                {"name": "gone", "path": "gone.csv", "sample": "a.csv"},
                {"name": "lake", "path": "s3://bucket/key.parquet"},
            ],
            "requires": {"duckdb": "v0.0.1", "extensions": ["no_such_extension", "json"]},
        })
        .to_string(),
    )
    .unwrap();

    let report = project_report(&s, "p");
    assert_eq!(report["ok"], false);
    assert_eq!(report["portable"], false);
    let status = |name: &str| {
        report["dashboards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["path"] == name)
            .unwrap()
            .clone()
    };
    // Reads, but will not travel.
    assert_eq!(status("absolute.dash")["sources"][0]["status"], "absolute");
    assert_eq!(status("above.dash")["sources"][0]["status"], "outside");
    // Left unrun rather than reaching out, which would hang here anyway.
    assert_eq!(status("remote.dash")["status"], "remote");
    assert_eq!(status("remote.dash")["checked"], false);
    assert_eq!(status("broken.dash")["status"], "failed");
    assert_eq!(status("absent.dash")["status"], "missing");
    assert_eq!(status("../escape.dash")["status"], "outside");
    assert_eq!(status("../escape.dash")["checked"], false);

    let kinds: Vec<&str> = report["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["kind"].as_str().unwrap())
        .collect();
    for kind in [
        "not_portable",
        "failed_check",
        "missing_dashboard",
        "missing_data",
        "missing_extension",
    ] {
        assert!(kinds.contains(&kind), "{kind} in {kinds:?}");
    }
    assert!(report["problems"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["hint"].is_string()));
    assert_eq!(report["data"][0]["status"], "ok");
    assert_eq!(report["data"][1]["status"], "missing");
    assert_eq!(report["data"][1]["sample"]["there"], true);
    assert_eq!(report["data"][2]["status"], "remote");
    assert_eq!(report["extensions"][0]["status"], "missing");
    assert_ne!(report["extensions"][1]["status"], "missing");
    let limits = report["limitations"].to_string();
    assert!(limits.contains("v0.0.1"), "{limits}");
    assert!(limits.contains("remote.dash"), "{limits}");
    assert!(limits.contains("lake"), "{limits}");
    // A key in the address would not show, because the address is not repeated.
    assert!(!limits.contains("key.parquet"), "{limits}");

    // A data entry relinks in the manifest, and a dashboard keeps its own path.
    let moved = s.run(
        &[
            "project", "relink", "p", "--data", "gone", "--to", "p/a.csv",
        ],
        None,
    );
    assert!(
        moved.status.success(),
        "{}",
        String::from_utf8_lossy(&moved.stderr)
    );
    assert_eq!(project_report(&s, "p")["data"][1]["path"], "a.csv");
    s.error(
        &[
            "project", "relink", "p", "--data", "nope", "--to", "p/a.csv",
        ],
        1,
        "not_found",
    );
}

#[test]
fn a_project_starts_from_what_is_in_the_folder() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.0.join("p/reports")).unwrap();
    std::fs::create_dir_all(s.0.join("p/.hidden")).unwrap();
    let spec = "query \"q\" {\n  sql = \"SELECT 1 AS n\"\n}\n";
    std::fs::write(s.0.join("p/top.dash"), spec).unwrap();
    std::fs::write(s.0.join("p/reports/sub.dash"), spec).unwrap();
    std::fs::write(s.0.join("p/.hidden/no.dash"), spec).unwrap();

    let made = s.run(&["project", "init", "p", "--name", "mine"], None);
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(s.0.join("p/ducklocal-project.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["name"], "mine");
    assert_eq!(
        manifest["dashboards"],
        json!(["reports/sub.dash", "top.dash"])
    );
    assert!(manifest["requires"]["duckdb"].is_string());
    assert_eq!(project_report(&s, "p")["ok"], true);

    // It will not replace one, and says what to do instead.
    let error = s.error(&["project", "init", "p"], 1, "exists");
    assert!(error["error"]["hint"].is_string());
    // Mistakes.
    s.error(&["project"], 2, "argument");
    s.error(&["project", "dance"], 2, "argument");
    s.error(&["project", "check", "no_such_folder"], 1, "not_found");
    s.error(&["project", "init", "no_such_folder"], 1, "io");
    s.error(&["project", "check", "p", "--to", "x"], 2, "argument");
    std::fs::create_dir_all(s.0.join("bad")).unwrap();
    std::fs::write(s.0.join("bad/ducklocal-project.json"), "{ nope").unwrap();
    s.error(&["project", "check", "bad"], 1, "project");
    let help = s.run(&["project", "--help"], None);
    assert!(String::from_utf8(help.stdout).unwrap().contains("relink"));
    // A lone dashboard needs none of this.
    std::fs::write(s.0.join("lone.dash"), spec).unwrap();
    let lone = s.run(&["check", "lone.dash"], None);
    assert!(lone.status.success());
}

/// A session, end to end through the real commands: what it records, what it
/// keeps out, and what it says once the world has moved on.
#[test]
fn a_session_records_the_lineage_and_says_what_no_longer_holds() {
    let s = Sandbox::new();
    let session = s.0.join("work.session.jsonl");
    let run = |args: &[&str]| -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_ducklocal"))
            .args(args)
            .current_dir(&s.0)
            .env("HOME", s.0.join("home"))
            .env("XDG_CONFIG_HOME", s.0.join("home"))
            .env("XDG_DATA_HOME", s.0.join("home"))
            .env("DUCKLOCAL_SESSION", &session)
            .env("DUCKLOCAL_AGENT", "TestAgent")
            .output()
            .unwrap();
        assert!(
            output.stderr.is_empty() || !output.status.success(),
            "a session must not add noise: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    let json = |output: Output| -> Value {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    std::fs::write(
        s.0.join("orders.csv"),
        "region,amount\nEU,SENSITIVEVALUE\nUS,20\n",
    )
    .unwrap();
    std::fs::write(s.0.join("big.csv"), "n\n1\n2\n3\n").unwrap();

    // Without the variable, nothing is recorded.
    s.success(&["query", "--sql", "SELECT * FROM 'big.csv'"]);
    assert!(!session.exists());

    json(run(&[
        "query",
        "--sql",
        "SELECT region, amount FROM 'orders.csv'",
    ]));
    json(run(&[
        "query",
        "--sql",
        "SELECT n FROM 'big.csv'",
        "--limit",
        "1",
    ]));
    let failed = run(&["query", "--sql", "SELECT * FROM 'no_such_file.csv'"]);
    assert_eq!(failed.status.code(), Some(1));
    let denied = run(&[
        "query",
        "--restricted",
        "--sql",
        "COPY (SELECT 1) TO 'x.csv'",
    ]);
    assert_eq!(denied.status.code(), Some(1));
    json(run(&["query", "--sql", "SELECT 1 AS password"]));

    // No rows and no credentials, anywhere in the file.
    let text = std::fs::read_to_string(&session).unwrap();
    assert!(!text.contains("SENSITIVEVALUE"), "{text}");
    assert!(text.contains("may hold credentials"));
    assert!(!text.contains("AS password"), "{text}");

    let shown = json(run(&["session", "show"]));
    let events = shown["events"].as_array().unwrap();
    assert_eq!(shown["steps"], 5);
    assert_eq!(events[0]["kind"], "query");
    assert_eq!(events[0]["origin"], "TestAgent");
    assert_eq!(events[0]["status"], "ok");
    assert_eq!(events[0]["complete"], true);
    assert_eq!(events[0]["sources"][0]["ref"], "orders.csv");
    assert_eq!(events[0]["doubts"], json!([]));
    assert!(events[0]["engine"]["duckdb"].is_string());
    // Truncated: never shown as complete.
    assert_eq!(events[1]["complete"], false);
    assert_eq!(events[1]["truncated"], true);
    assert!(events[1]["doubts"][0]
        .as_str()
        .unwrap()
        .contains("only 1 of"));
    // Failed, and refused, each said.
    assert_eq!(events[2]["status"], "failed");
    assert_eq!(events[2]["complete"], false);
    assert_eq!(events[3]["error_kind"], "policy_denied");
    assert_eq!(events[3]["restricted"], true);

    // A conclusion rests on steps, and starts out with no doubts about them.
    let noted = json(run(&[
        "session", "note", "--text", "EU leads", "--refs", "1",
    ]));
    assert_eq!(noted["refs"], json!([1]));
    let shown = json(run(&["session", "show"]));
    let conclusion = &shown["conclusions"][0];
    assert_eq!(conclusion["text"], "EU leads");
    assert_eq!(conclusion["latest"], true);
    assert_eq!(conclusion["doubts"], json!([]));

    // The source moves on: the earlier conclusion says so, and a later one is
    // told apart from it.
    std::fs::write(s.0.join("orders.csv"), "region,amount\nEU,1\n").unwrap();
    json(run(&[
        "session",
        "note",
        "--text",
        "On a truncated result",
        "--refs",
        "2",
    ]));
    let shown = json(run(&["session", "show"]));
    let earlier = &shown["conclusions"][0];
    assert_eq!(earlier["latest"], false);
    assert!(earlier["doubts"][0]
        .as_str()
        .unwrap()
        .contains("orders.csv has changed"));
    let later = &shown["conclusions"][1];
    assert_eq!(later["latest"], true);
    assert!(later["doubts"][0].as_str().unwrap().contains("only 1 of"));
    std::fs::remove_file(s.0.join("orders.csv")).unwrap();
    let shown = json(run(&["session", "show"]));
    assert!(shown["conclusions"][0]["doubts"][0]
        .as_str()
        .unwrap()
        .contains("no longer there"));

    // A spec's revisions: checked, changed, and the old text still to hand.
    let spec = "query \"q\" {\n  sql = <<SQL\nSELECT 1 AS n\nSQL\n}\n";
    std::fs::write(s.0.join("a.dash"), spec).unwrap();
    json(run(&["check", "a.dash", "--summary"]));
    let check_seq = json(run(&["session", "show"]))["latest_seq"].clone();
    let seq = check_seq.to_string();
    std::fs::write(s.0.join("a.dash"), format!("{spec}\n")).unwrap();
    let step = json(run(&["session", "get", "--seq", &seq]));
    assert_eq!(step["kind"], "check");
    assert_eq!(step["restricted"], false);
    assert_eq!(step["revision"]["text"], spec);
    assert!(step["doubts"][0]
        .as_str()
        .unwrap()
        .contains("spec has changed"));
    // `show` stays bounded: the text is left to `get`, and --last is honoured.
    let shown = json(run(&["session", "show", "--last", "1"]));
    assert_eq!(shown["events"].as_array().unwrap().len(), 1);
    assert!(shown["omitted_steps"].as_u64().unwrap() > 0);
    assert!(shown["events"][0]["revision"].get("text").is_none());

    // A proposal is a step, and shows how it stands.
    std::fs::write(s.0.join("new.dash"), format!("{spec}\n\n")).unwrap();
    json(run(&["propose", "a.dash", "--from", "new.dash"]));
    let shown = json(run(&["session", "show", "--last", "1"]));
    assert_eq!(shown["events"][0]["kind"], "proposal");
    assert!(shown["events"][0]["doubts"][0]
        .as_str()
        .unwrap()
        .contains("not yet decided"));

    // For reading, with its doubts in it.
    let page = String::from_utf8(run(&["session", "show", "--format", "md"]).stdout).unwrap();
    assert!(page.contains("**Conclusion:** EU leads"), "{page}");
    assert!(page.contains("⚠"), "{page}");

    // Mistakes.
    s.error(&["session"], 2, "argument");
    s.error(&["session", "show"], 2, "argument");
    assert_eq!(
        run(&["session", "note", "--text", "x", "--refs", "999"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        run(&["session", "get", "--seq", "999"]).status.code(),
        Some(1)
    );
    assert_eq!(run(&["session", "dance"]).status.code(), Some(2));

    // Clearing: named outright, and only a session file.
    assert_eq!(run(&["session", "clear"]).status.code(), Some(2));
    std::fs::write(s.0.join("notes.txt"), "mine").unwrap();
    assert_eq!(
        run(&["session", "clear", "notes.txt"]).status.code(),
        Some(1)
    );
    assert_eq!(
        std::fs::read_to_string(s.0.join("notes.txt")).unwrap(),
        "mine"
    );
    assert_eq!(
        json(run(&["session", "clear", "work.session.jsonl"]))["cleared"],
        true
    );
    assert!(!session.exists());
    assert_eq!(
        json(run(&["session", "clear", "work.session.jsonl"]))["cleared"],
        false
    );
}

#[test]
fn a_session_that_cannot_be_written_warns_and_the_work_still_stands() {
    let s = Sandbox::new();
    std::fs::write(s.0.join("notes.txt"), "mine\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ducklocal"))
        .args(["query", "--sql", "SELECT 1 AS n"])
        .current_dir(&s.0)
        .env("HOME", s.0.join("home"))
        .env("XDG_CONFIG_HOME", s.0.join("home"))
        .env("XDG_DATA_HOME", s.0.join("home"))
        .env("DUCKLOCAL_SESSION", "notes.txt")
        .output()
        .unwrap();
    assert!(output.status.success());
    let warning: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(warning["warning"]
        .as_str()
        .unwrap()
        .contains("not a session"));
    assert_eq!(
        std::fs::read_to_string(s.0.join("notes.txt")).unwrap(),
        "mine\n"
    );
}

#[test]
fn schema_stats_profiles_one_relation() {
    let s = Sandbox::new();
    std::fs::write(
        s.0.join("orders.csv"),
        "day,amount\n2026-01-01,10.5\n2026-01-03,20\n",
    )
    .unwrap();
    std::fs::write(s.0.join("other.csv"), "a\n1\n").unwrap();
    let mut stats = s.object(&["schema", "orders.csv", "--stats"]);
    // The same report the deprecated `profile` gives, timing aside.
    let mut profiled = s.profile(&["profile", "orders.csv"]);
    stats.as_object_mut().unwrap().remove("elapsed_ms");
    profiled.as_object_mut().unwrap().remove("elapsed_ms");
    assert_eq!(stats, profiled);
    assert_eq!(stats["row_count"], 2);
    // Two relations need --table to say which.
    let error = s.error(&["schema", ".", "--stats"], 2, "argument");
    let hint = error["error"]["hint"].as_str().unwrap();
    assert!(hint.starts_with("Add --table NAME to pick one: "), "{hint}");
    assert!(
        hint.contains("orders.csv") && hint.contains("other.csv"),
        "{hint}"
    );
    let picked = s.object(&["schema", ".", "--stats", "--table", "orders.csv"]);
    assert_eq!(picked["row_count"], 2);
    s.success(&[
        "query",
        "--database",
        "w.duckdb",
        "--read-write",
        "--sql",
        "CREATE TABLE t AS SELECT 1 AS n",
    ]);
    let table = s.object(&[
        "schema",
        "--database",
        "w.duckdb",
        "--table",
        "t",
        "--stats",
    ]);
    assert_eq!(table["row_count"], 1);
    s.error(
        &["schema", "orders.csv", "--stats", "--full"],
        2,
        "argument",
    );
    // Help lists five commands; the rest stay callable.
    let help = String::from_utf8(s.run(&["--help"], None).stdout).unwrap();
    for gone in ["profile", "lsp", "state"] {
        assert!(!help.contains(&format!("ducklocal {gone}")), "{help}");
    }
}

#[test]
fn schema_stats_names_a_missing_value_marker() {
    let s = Sandbox::new();
    std::fs::write(
        s.0.join("combine.csv"),
        "name,forty,cone,team
A,4.31,NA,NYG
B,NA,6.9,NA
C,4.52,7.1,DAL
",
    )
    .unwrap();
    let stats = s.object(&["schema", "combine.csv", "--stats"]);
    let column = |name: &str| {
        stats["columns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()
            .clone()
    };
    // Numbers and one word: the word is named, with its rows.
    assert_eq!(column("forty")["type"], "VARCHAR");
    assert_eq!(column("forty")["null_marker"], "NA");
    assert_eq!(column("forty")["null_marker_rows"], 1);
    assert_eq!(column("cone")["null_marker"], "NA");
    // Text that happens to hold the word is text.
    assert!(column("team").get("null_marker").is_none());
    assert!(column("name").get("null_marker").is_none());
    let hint = stats["hint"].as_str().unwrap();
    assert!(hint.starts_with("forty, cone hold numbers"), "{hint}");
    assert!(hint.contains("nullstr = 'NA'"), "{hint}");
    // The fix it names works: the columns come back as numbers.
    let read = s.object(&[
        "query",
        "--sql",
        "SELECT typeof(forty) AS t, count(forty) AS n FROM read_csv('combine.csv', nullstr = 'NA') GROUP BY ALL",
    ]);
    assert_eq!(read["rows"][0], json!(["DOUBLE", 2]));
    // A clean file gets no hint.
    std::fs::write(
        s.0.join("clean.csv"),
        "a,b
1,x
2,y
",
    )
    .unwrap();
    assert!(s
        .object(&["schema", "clean.csv", "--stats"])
        .get("hint")
        .is_none());
}

#[test]
fn check_reports_filters_and_runs_queries_with_nothing_picked() {
    let s = Sandbox::new();
    std::fs::write(
        s.0.join("orders.csv"),
        "day,channel,amount\n2026-01-01,web,10\n2026-01-01,shop,20\n2026-01-02,web,5\n",
    )
    .unwrap();
    let spec = r#"
source "orders" { path = "orders.csv" }
query "by_channel" { sql = "SELECT channel, sum(amount) AS amount FROM orders WHERE $day GROUP BY 1" }
query "by_day" { sql = "SELECT day, sum(amount) AS amount FROM orders WHERE $channel AND $day GROUP BY 1" }
query "days" { sql = "SELECT DISTINCT day FROM orders ORDER BY 1" }
plot "channels" { type = "bar" query = query.by_channel x = channel y = amount }
plot "days" { type = "table" query = query.days x = day }
plot "trend" { type = "line" query = query.by_day x = day y = amount }
filter "channel" { plot = plot.channels }
filter "day" {
  plot = plot.days
  column = day
}
"#;
    std::fs::write(s.0.join("board.dash"), spec).unwrap();
    s.success(&[
        "query",
        "--database",
        "w.duckdb",
        "--read-write",
        "--sql",
        "CREATE TABLE x(a INT)",
    ]);
    let out = s.object(&["check", "board.dash", "--database", "w.duckdb"]);
    let filters = out["filters"].as_array().unwrap();
    assert_eq!(filters[0]["name"], "channel");
    assert_eq!(filters[0]["column"], "channel");
    assert_eq!(filters[0]["queries"], json!(["by_day"]));
    assert_eq!(filters[1]["queries"], json!(["by_channel", "by_day"]));

    // A placeholder with no filter, and a filter on a column its plot's
    // query does not return, are both spec mistakes.
    std::fs::write(s.0.join("typo.dash"), spec.replace("AND $day", "AND $dy")).unwrap();
    let error = s.error(&["check", "typo.dash"], 2, "spec");
    assert!(error["error"]["message"].as_str().unwrap().contains("$dy"));
    std::fs::write(
        s.0.join("column.dash"),
        spec.replace("column = day", "column = nope"),
    )
    .unwrap();
    let error = s.error(
        &["check", "column.dash", "--database", "w.duckdb"],
        2,
        "spec",
    );
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("\"nope\""));
}

/// An agent answers review threads by id, and starts them only on a plot the
/// spec has.
#[test]
fn comments_take_a_threads_number_and_refuse_an_unknown_plot() {
    let s = Sandbox::new();
    std::fs::write(
        s.0.join("d.dash"),
        "query \"q\" { sql = \"SELECT 1 AS n\" }\n\
         plot \"p\" {\n  type = \"table\"\n  query = query.q\n}\n",
    )
    .unwrap();

    let error = s.error(
        &["comments", "d.dash", "--add", "pp", "--text", "x"],
        2,
        "argument",
    );
    assert_eq!(error["error"]["message"], "d.dash: no plot named \"pp\"");
    assert_eq!(error["error"]["hint"], "Plots: p");
    assert!(!s.0.join("d.dash.comments.json").exists());

    let added = s.object(&["comments", "d.dash", "--add", "p", "--text", "Sort it"]);
    assert_eq!(added["thread"]["id"], "c1");
    // `1` is c1, as a person reading the threads would say it.
    let replied = s.object(&["comments", "d.dash", "--reply", "1", "--text", "Sorted"]);
    assert_eq!(replied["thread"]["comments"].as_array().unwrap().len(), 2);
    s.object(&["comments", "d.dash", "--resolve", "1"]);
    assert_eq!(s.object(&["comments", "d.dash"])["open"], 0);
    s.object(&["comments", "d.dash", "--reopen", "c1"]);
    assert_eq!(s.object(&["comments", "d.dash"])["open"], 1);
    s.error(&["comments", "d.dash", "--resolve", "9"], 2, "argument");
}
