//! What DuckLocal relies on DuckDB to do, checked against the bundled one.
//!
//! Most of DuckLocal's DuckDB use is ordinary SQL that any release runs. A
//! few places lean on something narrower — a message's wording, a catalog
//! function's columns, how the C API counts statements, which statements
//! `json_serialize_sql` will serialize, how a value comes back through Arrow —
//! and those are the places a new DuckDB release changes without a compile
//! error. Each test here pins one such assumption and names the code that
//! makes it, so when the `duckdb` dependency moves (2.0 above all) a failure
//! says which assumption broke and where to look, rather than a dashboard
//! check or an open-file message quietly going wrong.
//!
//! Some of what is pinned is behaviour DuckLocal would rather not have (a
//! nanosecond timestamp shown to the microsecond, say). It is pinned as it
//! is, marked as such: a change to it is a change to look at either way.
//!
//! `files_written_by_v1_5_5_still_open` reads databases this release wrote,
//! kept gzipped under `tests/fixtures/duckdb-v1.5.5/`. Today that is
//! trivially true; its point is the day the dependency is newer.

use std::path::{Path, PathBuf};

use duckdb::Connection;

use crate::query::{run_of, QueryOutcome};

fn mem() -> Connection {
    Connection::open_in_memory().unwrap()
}

/// The message DuckDB fails `sql` with.
fn error_of(conn: &Connection, sql: &str) -> String {
    match conn.prepare(sql) {
        Ok(mut statement) => statement
            .query([])
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| panic!("{sql:?} did not fail")),
        Err(e) => e.to_string(),
    }
}

/// The columns `function` returns, by name.
fn columns_of(conn: &Connection, function: &str) -> Vec<String> {
    let mut statement = conn
        .prepare(&format!("DESCRIBE SELECT * FROM {function}"))
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The catalog functions' columns the sidebar, the schema command, the
/// extension list, `open_file` and alias picking select by name
/// (`schema.rs`, `db.rs`, `extensions.rs`).
#[test]
fn catalog_functions_return_the_columns_read() {
    let conn = mem();
    let expected: &[(&str, &[&str])] = &[
        (
            "duckdb_databases()",
            &[
                "database_name",
                "database_oid",
                "path",
                "type",
                "readonly",
                "internal",
                "tags",
            ],
        ),
        (
            "duckdb_tables()",
            &[
                "database_name",
                "schema_name",
                "table_name",
                "estimated_size",
                "comment",
            ],
        ),
        (
            "duckdb_views()",
            &[
                "database_name",
                "schema_name",
                "view_name",
                "internal",
                "comment",
            ],
        ),
        (
            "duckdb_columns()",
            &[
                "database_name",
                "schema_name",
                "table_name",
                "column_name",
                "data_type",
            ],
        ),
        (
            "duckdb_extensions()",
            &[
                "extension_name",
                "loaded",
                "installed",
                "install_mode",
                "extension_version",
                "installed_from",
                "description",
            ],
        ),
    ];
    for (function, wanted) in expected {
        let have = columns_of(&conn, function);
        for column in *wanted {
            assert!(
                have.iter().any(|c| c == column),
                "{function} has no {column}: {have:?}"
            );
        }
    }
    // `db.rs` reads a file database's storage version from its tags.
    let dir = scratch("tags");
    let path = dir.join("tagged.duckdb");
    let conn = Connection::open(&path).unwrap();
    let storage: Option<String> = conn
        .query_row(
            "SELECT tags['storage_version'] FROM duckdb_databases() \
             WHERE database_name = current_database()",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        storage.as_deref().is_some_and(|v| v.starts_with('v')),
        "{storage:?}"
    );
}

/// `spec::names_a_missing_table` lets `ducklocal check` without a database
/// list a query on an unknown table as unresolved instead of failing it. It
/// reads the message's words.
#[test]
fn a_missing_table_is_told_from_other_failures() {
    let conn = mem();
    conn.execute_batch("CREATE TABLE t(a INTEGER)").unwrap();
    for sql in [
        "SELECT * FROM nope",
        "SELECT * FROM main.nope",
        "SELECT * FROM nodb.nope",
    ] {
        let message = error_of(&conn, sql);
        assert!(
            crate::spec::names_a_missing_table(&message),
            "{sql}: {message}"
        );
    }
    for sql in [
        "SELECT nope FROM t",
        "SELECT 1 +",
        "SELECT CAST('x' AS INTEGER)",
    ] {
        let message = error_of(&conn, sql);
        assert!(
            !crate::spec::names_a_missing_table(&message),
            "{sql}: {message}"
        );
    }
}

/// `cli::validate_sql` is the gate that keeps `ducklocal query` and every
/// dashboard query to one statement, through the C API's
/// `duckdb_extract_statements`: a `COPY …; SELECT …` must never get past it.
#[test]
fn the_c_api_counts_statements() {
    let one = [
        "SELECT 1",
        "SELECT 1;",
        "SELECT 1;;",
        "SELECT ';' AS s",
        "SELECT 1 -- ; a comment\n",
        "SELECT 1 /* ; */",
    ];
    for sql in one {
        assert!(crate::cli::validate_sql(sql).is_ok(), "{sql:?}");
    }
    for (sql, count) in [
        ("SELECT 1; SELECT 2", 2),
        ("COPY (SELECT 1) TO 'x.csv'; SELECT 2", 2),
        ("", 0),
        ("   ", 0),
        ("-- only a comment", 0),
    ] {
        let message = crate::cli::validate_sql(sql)
            .unwrap_err()
            .message()
            .to_string();
        assert_eq!(
            message,
            format!("Expected exactly one SQL statement; found {count}"),
            "{sql:?}"
        );
    }
    let message = crate::cli::validate_sql("SELEC 1")
        .unwrap_err()
        .message()
        .to_string();
    assert!(message.starts_with("Parser Error"), "{message}");
}

/// `spec::validate_query_sql` takes "only reads" from `json_serialize_sql`,
/// which serializes SELECT-shaped statements and refuses the rest; `PIVOT`
/// is its one false refusal, let through by name. Were a release to
/// serialize `PIVOT`, the carve-out could go; were it to serialize a write,
/// the gate would open.
#[test]
fn json_serialize_sql_refuses_all_but_reads() {
    let conn = mem();
    let refused = |sql: &str| -> bool {
        conn.query_row(
            "SELECT coalesce(json_serialize_sql(?1::VARCHAR)::JSON->>'error' = 'true', true)",
            [sql],
            |row| row.get(0),
        )
        .unwrap()
    };
    for sql in [
        "SELECT 1",
        "FROM t",
        "WITH s AS (SELECT 1) SELECT * FROM s",
        "VALUES (1)",
        "TABLE t",
        "SHOW TABLES",
        "DESCRIBE t",
        "SUMMARIZE t",
    ] {
        assert!(!refused(sql), "{sql} is a read");
    }
    for sql in [
        "INSERT INTO t VALUES (1)",
        "COPY t TO 'x.csv'",
        "ATTACH 'x.duckdb'",
        "SET threads = 1",
        "CALL pragma_version()",
        "CREATE TABLE u(a INTEGER)",
        // Not a write; refused all the same, hence the carve-out.
        "PIVOT t ON a USING sum(b)",
    ] {
        assert!(refused(sql), "{sql} is refused");
    }
    // End to end, the gate keeps its promise either way.
    assert!(crate::spec::validate_query_sql("PIVOT t ON a IN ('x', 'y') USING sum(b)").is_ok());
    assert!(crate::spec::validate_query_sql("UNPIVOT t ON a, b INTO NAME k VALUE v").is_ok());
    assert!(crate::spec::validate_query_sql("FROM t PIVOT (sum(b) FOR a IN ('x'))").is_ok());
    assert!(crate::spec::validate_query_sql("COPY t TO 'x.csv'").is_err());
    assert!(crate::spec::validate_query_sql("SELECT 1; DROP TABLE t").is_err());
}

/// A PIVOT without an `IN` list is two statements to the C API — it must
/// first find the values to pivot on — so `cli::validate_sql` refuses it,
/// nested or not, as "found 2", before the carve-out above is reached. Only
/// an `IN` list, UNPIVOT or the standard `PIVOT (… FOR … IN …)` gets
/// through. A known gap, pinned as it is: the docs promise PIVOT.
#[test]
fn a_pivot_without_values_counts_as_two_statements() {
    for sql in [
        "PIVOT t ON a USING sum(b)",
        "SELECT * FROM (PIVOT t ON a USING sum(b))",
        "WITH p AS (PIVOT t ON a USING sum(b)) SELECT * FROM p",
    ] {
        let message = crate::cli::validate_sql(sql)
            .unwrap_err()
            .message()
            .to_string();
        assert_eq!(
            message, "Expected exactly one SQL statement; found 2",
            "{sql}"
        );
    }
}

/// How each type comes back through Arrow (`query.rs`'s
/// `value_to_string`), beyond the everyday types `borrowed_and_owned_cell_
/// formatting_agree` covers: the wide integers, the other timestamp units,
/// the time zone types, and the types Arrow has no plain form for.
#[test]
fn every_type_renders_as_pinned() {
    let conn = mem();
    let sql = "SELECT
        170141183460469231731687303715884105727::HUGEINT AS hugeint,
        340282366920938463463374607431768211455::UHUGEINT AS uhugeint,
        123456789012345678901234567890.1234567::DECIMAL(38, 7) AS dec38,
        TIMESTAMP_MS '2024-10-04 07:08:09.123' AS ts_ms,
        TIMESTAMP_S '2024-10-04 07:08:09' AS ts_s,
        TIMESTAMP_NS '2024-10-04 07:08:09.123456789' AS ts_ns,
        TIMESTAMPTZ '2024-10-04 07:08:09+02' AS tstz,
        TIMETZ '07:08:09+02' AS timetz,
        '4ac7a9e9-607c-4c8a-84f3-843f0191e3fd'::UUID AS uuid,
        'b'::ENUM('a', 'b') AS enum,
        union_value(n := 3)::UNION(n INTEGER, s VARCHAR) AS onion,
        [1, 2, 3]::INTEGER[3] AS arr,
        '{\"a\": 1}'::JSON AS json,
        INTERVAL 3 DAY AS days,
        INTERVAL '1 day 2 hours' AS day_hours,
        INTERVAL 0 SECOND AS nothing,
        '0101'::BIT AS bit,
        12345678901234567890123456789012345678901234567890::BIGNUM AS bignum";
    let QueryOutcome::Rows(result) = run_of(&conn, sql).unwrap() else {
        panic!("expected rows");
    };
    let rendered: Vec<(&str, &str)> = result
        .columns
        .iter()
        .zip(&result.rows[0])
        .map(|(c, v)| (c.name.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        rendered,
        [
            ("hugeint", "170141183460469231731687303715884105727"),
            ("uhugeint", "340282366920938463463374607431768211455"),
            ("dec38", "123456789012345678901234567890.1234567"),
            ("ts_ms", "2024-10-04 07:08:09.123"),
            ("ts_s", "2024-10-04 07:08:09"),
            // Lossy today: the nanoseconds are cut to microseconds.
            ("ts_ns", "2024-10-04 07:08:09.123456"),
            // Lossy today: the instant in UTC, with no zone said.
            ("tstz", "2024-10-04 05:08:09"),
            // Lossy today: the offset is dropped.
            ("timetz", "07:08:09"),
            ("uuid", "4ac7a9e9-607c-4c8a-84f3-843f0191e3fd"),
            ("enum", "b"),
            ("onion", "3"),
            ("arr", "[1, 2, 3]"),
            ("json", "{\"a\": 1}"),
            ("days", "3 days"),
            // Unreadable today: the time part as raw nanoseconds, "1 days".
            ("day_hours", "1 days 7200000000000 ns"),
            ("nothing", "0"),
            // Unreadable today: Arrow hands both over as bytes.
            ("bit", "<blob 2 bytes>"),
            ("bignum", "<blob 24 bytes>"),
        ]
    );
    // Arrow's names for them, which the result header shows.
    let types: Vec<&str> = result
        .columns
        .iter()
        .map(|c| c.duck_type.as_str())
        .collect();
    assert_eq!(
        types[0], "Decimal128(38, 0)",
        "HUGEINT arrives as a decimal"
    );
    assert_eq!(types[6], "Timestamp(Microsecond, Some(\"UTC\"))");
}

/// The bundled build loads `json` and `parquet` (the crate features) and
/// has no ICU: a TIMESTAMPTZ does not cast to DATE, which is why
/// `examples/usage_dashboard` casts through VARCHAR. A build with ICU would
/// make that detour unnecessary, and this fail.
#[test]
fn the_build_has_json_and_parquet_and_no_icu() {
    let conn = mem();
    let loaded: Vec<String> = conn
        .prepare("SELECT extension_name FROM duckdb_extensions() WHERE loaded")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for name in ["json", "parquet"] {
        assert!(
            loaded.iter().any(|l| l == name),
            "{name} not loaded: {loaded:?}"
        );
    }
    let message = error_of(
        &conn,
        "SELECT CAST(TIMESTAMPTZ '2024-10-04 07:08:09+02' AS DATE)",
    );
    assert!(message.contains("Unimplemented type for cast"), "{message}");
}

/// `storage::explain_open_failure` explains a refusal only when DuckDB
/// blamed the file's version. The version messages themselves cannot be
/// produced here — the header is checksummed, so a patched version number
/// reads as corruption — and are pinned as quoted in `storage.rs`; what can
/// be checked is that corruption is not mistaken for a version.
#[test]
fn a_corrupt_header_is_not_called_a_version() {
    let dir = scratch("corrupt");
    let path = dir.join("patched.duckdb");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t AS SELECT 1 AS n; CHECKPOINT;")
            .unwrap();
    }
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[12..20].copy_from_slice(&9_999u64.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();
    let message = Connection::open(&path).err().unwrap().to_string();
    assert!(message.contains("Corrupt database file"), "{message}");
    assert!(
        !crate::storage::refused_for_its_version(&message),
        "{message}"
    );
}

/// Databases written by DuckDB v1.5.5 — a user's data and DuckLocal's own
/// history store — open, read back, and take writes. Each is unpacked to a
/// scratch copy first, so nothing here can rewrite the committed fixture.
#[test]
fn files_written_by_v1_5_5_still_open() {
    let dir = scratch("fixtures");
    let copy = |name: &str| {
        let to = dir.join(name);
        let gz = std::fs::File::open(fixtures().join(format!("{name}.gz"))).unwrap();
        let mut out = std::fs::File::create(&to).unwrap();
        std::io::copy(&mut flate2::read::GzDecoder::new(gz), &mut out).unwrap();
        to
    };

    let conn = Connection::open(copy("data.duckdb")).unwrap();
    let QueryOutcome::Rows(result) =
        run_of(&conn, "SELECT * FROM sales.orders ORDER BY id").unwrap()
    else {
        panic!("expected rows");
    };
    assert_eq!(result.rows, fixture_rows());
    let view: i64 = conn
        .query_row("SELECT total FROM sales.order_total", [], |r| r.get(0))
        .unwrap();
    assert_eq!(view, 3);
    let comment: String = conn
        .query_row(
            "SELECT comment FROM duckdb_tables() WHERE table_name = 'orders'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(comment, "written by DuckDB v1.5.5");
    conn.execute_batch("INSERT INTO sales.orders (id) VALUES (4)")
        .unwrap();
    drop(conn);

    let conn = Connection::open(copy("history.duckdb")).unwrap();
    // Already at the current schema version: preparing it is a no-op.
    crate::history::prepare_schema(&conn).unwrap();
    let recent = crate::history::recent_of(&conn, 10).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].sql, "SELECT 42");
    assert_eq!(recent[0].row_count, Some(1));
    let files = crate::history::attached_files_of(&conn).unwrap();
    assert_eq!(files[0].view_name, "orders");
    assert_eq!(
        crate::history::get_setting_of(&conn, "language")
            .unwrap()
            .as_deref(),
        Some("zh")
    );
}

/// `sales.orders` as `run_of` renders it: every row of the fixture.
fn fixture_rows() -> Vec<Vec<String>> {
    [
        [
            "1",
            "9007199254740993",
            "12.500",
            "web",
            "true",
            "2026-09-01",
            "2026-09-01 08:30:00",
            "2026-09-01 06:30:00",
            "1 days 7200000000000 ns",
            "4ac7a9e9-607c-4c8a-84f3-843f0191e3fd",
            "[1, 2]",
            "{x: 1, y: a}",
            "{k: 1}",
            "b",
            "<blob 3 bytes>",
        ],
        [
            "2",
            "-1",
            "0.000",
            "柜面",
            "false",
            "1970-01-01",
            "1970-01-01 00:00:00",
            "1970-01-01 00:00:00",
            "0",
            "00000000-0000-0000-0000-000000000000",
            "[]",
            "{x: 0, y: }",
            "{}",
            "a",
            "<blob 0 bytes>",
        ],
        [
            "3", "NULL", "NULL", "NULL", "NULL", "NULL", "NULL", "NULL", "NULL", "NULL", "NULL",
            "NULL", "NULL", "NULL", "NULL",
        ],
    ]
    .iter()
    .map(|row| row.iter().map(|c| c.to_string()).collect())
    .collect()
}

/// Writes the fixtures `files_written_by_v1_5_5_still_open` reads. Run only
/// on the release they are named for:
/// `cargo test duckdb_contract::write_fixtures -- --ignored`.
#[test]
#[ignore]
fn write_fixtures() {
    assert_eq!(
        crate::storage::library_version(),
        "v1.5.5",
        "the fixtures are v1.5.5's"
    );
    let dir = scratch("write-fixtures");

    let conn = Connection::open(dir.join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE SCHEMA sales;
         CREATE TYPE sales.mood AS ENUM ('a', 'b');
         CREATE TABLE sales.orders (
             id INTEGER, big BIGINT, amount DECIMAL(18, 3), channel VARCHAR,
             paid BOOLEAN, day DATE, placed TIMESTAMP, placed_tz TIMESTAMPTZ,
             took INTERVAL, ref UUID, tags INTEGER[], detail STRUCT(x INTEGER, y VARCHAR),
             attrs MAP(VARCHAR, INTEGER), mood sales.mood, raw BLOB);
         COMMENT ON TABLE sales.orders IS 'written by DuckDB v1.5.5';
         INSERT INTO sales.orders VALUES
             (1, 9007199254740993, 12.5, 'web', true, DATE '2026-09-01',
              TIMESTAMP '2026-09-01 08:30:00', TIMESTAMPTZ '2026-09-01 08:30:00+02',
              INTERVAL '1 day 2 hours', '4ac7a9e9-607c-4c8a-84f3-843f0191e3fd',
              [1, 2], {'x': 1, 'y': 'a'}, MAP {'k': 1}, 'b', 'abc'::BLOB),
             (2, -1, 0, '柜面', false, DATE '1970-01-01', TIMESTAMP '1970-01-01',
              TIMESTAMPTZ '1970-01-01 00:00:00+00', INTERVAL 0 SECOND,
              '00000000-0000-0000-0000-000000000000', [], {'x': 0, 'y': ''}, MAP {},
              'a', ''::BLOB),
             (3, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
              NULL, NULL);
         CREATE VIEW sales.order_total AS SELECT count(*) AS total FROM sales.orders;
         CHECKPOINT;",
    )
    .unwrap();
    drop(conn);

    let conn = Connection::open(dir.join("history.duckdb")).unwrap();
    crate::history::prepare_schema(&conn).unwrap();
    crate::history::record_to(
        &conn,
        &crate::history::HistoryEntry {
            id: 0,
            sql: "SELECT 42".into(),
            started_at: "2026-10-11 12:00:00".into(),
            duration_ms: 3,
            row_count: Some(1),
            ok: true,
            error: None,
        },
    )
    .unwrap();
    crate::history::register_attached_file_to(&conn, "/data/orders.csv", "orders", "csv").unwrap();
    crate::history::set_setting_of(&conn, "language", "zh").unwrap();
    conn.execute_batch("CHECKPOINT").unwrap();
    drop(conn);

    std::fs::create_dir_all(fixtures()).unwrap();
    for name in ["data.duckdb", "history.duckdb"] {
        let mut file = std::fs::File::open(dir.join(name)).unwrap();
        let out = std::fs::File::create(fixtures().join(format!("{name}.gz"))).unwrap();
        let mut gz = flate2::write::GzEncoder::new(out, flate2::Compression::best());
        std::io::copy(&mut file, &mut gz).unwrap();
        gz.finish().unwrap();
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/duckdb-v1.5.5")
}

/// A fresh directory of its own per test, under the system's temp folder.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("ducklocal-duckdb-contract")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
