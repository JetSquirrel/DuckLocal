//! Performance probe harness (test-only).
//!
//! Reports wall time, allocation count, and allocated bytes for the paths that
//! scale with result-set size or catalog size. Allocation counts come from a
//! counting global allocator installed only in the test binary.
//!
//! Run one probe at a time so the numbers are not interleaved:
//!
//! ```text
//! cargo test --release perf_a -- --nocapture --test-threads=1
//! ```
//!
//! Probes: A row materialization, B result filter, C catalog load,
//! D chart data, E completion candidates.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Time a closure and report allocations made inside it.
fn probe<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let a0 = ALLOCS.load(Ordering::Relaxed);
    let b0 = BYTES.load(Ordering::Relaxed);
    let t = Instant::now();
    let out = f();
    let dt = t.elapsed();
    let allocs = ALLOCS.load(Ordering::Relaxed) - a0;
    let bytes = BYTES.load(Ordering::Relaxed) - b0;
    println!(
        "{name:<44} {:>9.2} ms  {:>12} allocs  {:>10.2} MB",
        dt.as_secs_f64() * 1000.0,
        allocs,
        bytes as f64 / 1e6
    );
    out
}

const ROWS: usize = 100_000;

fn wide_table(conn: &duckdb::Connection) {
    conn.execute_batch(&format!(
        "CREATE TABLE wide AS
         SELECT i AS id,
                'label_' || (i % 997) AS name,
                i * 1.5 AS amount,
                (i % 7 = 0) AS flag,
                DATE '2020-01-01' + INTERVAL (i % 900) DAY AS d,
                'city_' || (i % 31) AS city,
                i % 1000 AS bucket,
                'note text for row ' || i AS note
         FROM range({ROWS}) t(i)"
    ))
    .unwrap();
}

#[test]
fn perf_a_row_materialization() {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    probe("A0 create 100k x 8 table", || wide_table(&conn));
    let outcome = probe("A1 run_of: materialize 100k x 8", || {
        crate::query::run_of(&conn, "SELECT * FROM wide").unwrap()
    });
    let crate::query::QueryOutcome::Rows(result) = outcome else {
        panic!()
    };
    println!("   -> {} rows x {} cols", result.rows.len(), result.columns.len());

    // Baselines, kept for reference: the row-set copies that the delegate and
    // the chart panel used to make. Both are now `Rc` handles, so nothing in
    // the app pays these any more.
    probe("A2 baseline: rows.clone() (was per result)", || result.rows.clone());
    probe("A3 baseline: QueryResult.clone() (was per frame)", || result.clone());
}

#[test]
fn perf_b_filter() {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    wide_table(&conn);
    let crate::query::QueryOutcome::Rows(result) =
        crate::query::run_of(&conn, "SELECT * FROM wide").unwrap()
    else {
        panic!()
    };
    let hits = probe("B1 filter_row_indices('city_7')", || {
        crate::ui::results::filter_row_indices_probe(&result.rows, "city_7")
    });
    println!("   -> {:?} matches", hits.map(|h| h.len()));
}

#[test]
fn perf_c_catalog() {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    probe("C0 create 300 tables x 8 cols", || {
        for i in 0..300 {
            conn.execute_batch(&format!(
                "CREATE TABLE t{i}(a INT, b VARCHAR, c DOUBLE, d BOOLEAN, e DATE, f VARCHAR, g INT, h VARCHAR)"
            ))
            .unwrap();
        }
    });
    let catalog = probe("C1 load_catalog_of", || {
        crate::schema::load_catalog_of(&conn).unwrap()
    });
    let tables: usize = catalog.iter().map(|d| d.tables.len()).sum();
    let cols: usize = catalog
        .iter()
        .flat_map(|d| d.tables.iter())
        .map(|t| t.columns.len())
        .sum();
    println!("   -> {tables} tables, {cols} columns");
    probe("C2 catalog.clone() (rebuild_tree)", || catalog.clone());
}

#[test]
fn perf_d_chart_build() {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    wide_table(&conn);
    let crate::query::QueryOutcome::Rows(result) =
        crate::query::run_of(&conn, "SELECT d, amount, bucket FROM wide").unwrap()
    else {
        panic!()
    };
    let data = probe("D1 ChartData::prepare (once per result)", || {
        crate::ui::chart::ChartData::prepare(&result)
    });
    // Repeated to show what each frame in the chart tab actually costs now.
    for _ in 0..3 {
        probe("D2 per-frame handoff to the chart", || {
            data.clone_rows_for_probe()
        });
    }
}

#[test]
fn perf_e_completion_candidates() {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    for i in 0..300 {
        conn.execute_batch(&format!(
            "CREATE TABLE t{i}(a INT, b VARCHAR, c DOUBLE, d BOOLEAN, e DATE, f VARCHAR, g INT, h VARCHAR)"
        ))
        .unwrap();
    }
    let catalog = crate::schema::load_catalog_of(&conn).unwrap();
    let n = probe("E1 completion candidates per keystroke", || {
        crate::ui::completion::collect_candidates(&catalog, "t1").len()
    });
    println!("   -> {n} items offered");
}

/// Reproduces the reported hang: a wide PIVOT of node metrics, as used for
/// "one line per node" charting.
#[test]
fn perf_f_wide_pivot() {
    for (nodes, minutes) in [(20usize, 20_160usize), (100, 20_160), (500, 20_160)] {
        println!("\n--- {nodes} nodes x {minutes} timestamps ---");
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE node_metrics AS
             SELECT TIMESTAMP '2026-08-19 04:00:00' + INTERVAL (m) MINUTE AS timestamp,
                    'node-' || n AS node,
                    CAST(30 + (m % 50) + n AS VARCHAR) AS cpu_usage
             FROM range({minutes}) t(m), range({nodes}) u(n)"
        ))
        .unwrap();

        let sql = "PIVOT (
                       SELECT timestamp AS ts, node, TRY_CAST(cpu_usage AS DOUBLE) AS value
                       FROM node_metrics
                   ) ON node USING avg(value) GROUP BY ts ORDER BY ts";

        let outcome = probe("F1 run_of (materialize pivot)", || {
            crate::query::run_of(&conn, sql).unwrap()
        });
        let crate::query::QueryOutcome::Rows(result) = outcome else {
            panic!()
        };
        println!(
            "   -> {} rows x {} cols = {} cells",
            result.rows.len(),
            result.columns.len(),
            result.rows.len() * result.columns.len()
        );

        let data = probe("F2 ChartData::prepare", || {
            crate::ui::chart::ChartData::prepare(&result)
        });
        println!(
            "   -> {} series x {} points = {} plotted values",
            data.series_count_for_probe(),
            data.clone_rows_for_probe().len(),
            data.series_count_for_probe() * data.clone_rows_for_probe().len()
        );
    }
}
