//! What the column overview has already profiled, so that looking at a
//! column or a relation again costs nothing — and what makes it stale.
//!
//! **Scope.** One cache for the process, in memory, keyed by the relation's
//! SQL text exactly as the overview reads it. Profiles are never written to
//! disk and die with the app. At most [`RELATIONS`] relations are kept; the
//! one used longest ago goes first.
//!
//! **Validity.** Every entry carries the [`Stamp`] it was computed under, and
//! a lookup under a different stamp drops the entry rather than serve it. A
//! stamp is:
//!
//! - the **data generation**, which [`data_changed`] moves on whenever
//!   DuckLocal runs a statement that may write, attaches or detaches
//!   something, or switches databases — conservatively: a `SET` moves it too;
//! - the **files** the relation reads: every local path a string literal in
//!   its SQL names (a glob counts as the files it matches), and those named
//!   by the views it mentions, each with its size and modification time — so
//!   a CSV rewritten by another program is noticed.
//!
//! What no stamp sees — a remote object replaced, a database file written by
//! another process while attached read-only — is why the overview also says
//! when a profile came from the cache and offers to refresh it.
//!
//! The profiling options (bins, common values) are compile-time constants,
//! so they need no place in the key.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

use duckdb::Connection;

use crate::overview::{ColumnOverview, Shell};

/// Relations whose profiles are kept at once.
const RELATIONS: usize = 16;
/// Files a stamp looks at, at most. A glob over more is stamped by its
/// first this many, in sorted order, and their count.
const FILES: usize = 1024;

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Something DuckLocal ran may have changed data: every profile computed
/// before now is stale.
pub fn data_changed() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// What a profile's validity depends on. Two stamps that differ mean the
/// data may have changed between them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    generation: u64,
    /// `(path, size and modification time)`; `None` for a path that could not
    /// be read, so a file appearing is a change too.
    files: Vec<(PathBuf, Option<(u64, SystemTime)>)>,
    /// How many files the relation's globs matched, all told.
    matched: usize,
}

/// The stamp `relation` has now. Reads the catalog for the views it names,
/// and the file system; nothing reads the data.
pub fn stamp_of(conn: &Connection, relation: &str) -> Stamp {
    // Read first: a change that lands while the files are looked at then
    // leaves a stamp that is already stale, never one that looks fresh.
    let generation = GENERATION.load(Ordering::SeqCst);
    let mut literals = literals_of(relation);
    for definition in view_definitions(conn, relation) {
        literals.extend(literals_of(&definition));
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut matched = 0;
    for literal in literals {
        let expanded = crate::db::expand_tilde(&literal);
        // Only what reads as a data file: a `'*'` in a query is no glob.
        if crate::db::data_file_kind(&expanded).is_none() {
            continue;
        }
        if crate::sources::is_pattern(&expanded) {
            let files = crate::sources::resolve(std::slice::from_ref(&expanded)).files;
            matched += files.len();
            paths.extend(files.into_iter().map(PathBuf::from));
        } else {
            paths.push(PathBuf::from(expanded));
        }
    }
    paths.sort();
    paths.dedup();
    paths.truncate(FILES);
    let files = paths
        .into_iter()
        .map(|path| {
            let seen = std::fs::metadata(&path)
                .ok()
                .and_then(|m| Some((m.len(), m.modified().ok()?)));
            (path, seen)
        })
        .collect();
    Stamp {
        generation,
        files,
        matched,
    }
}

/// The text of every string literal and quoted identifier in `sql`: where a
/// file path can stand in a `FROM`. Over-collecting is harmless — a literal
/// that names no data file is not stamped.
fn literals_of(sql: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                let mut text = String::new();
                while let Some(next) = chars.next() {
                    if next == c {
                        // A doubled quote is the quote itself.
                        if chars.peek() == Some(&c) {
                            chars.next();
                            text.push(c);
                            continue;
                        }
                        break;
                    }
                    text.push(next);
                }
                found.push(text);
            }
            // A comment can hold an apostrophe that opens nothing.
            '-' if chars.peek() == Some(&'-') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    found
}

/// The SQL of the user's views whose names `relation` mentions: a file the
/// sidebar opened is a view over its reader, and the overview of
/// `SELECT * FROM sales` reads `sales.csv` through it.
fn view_definitions(conn: &Connection, relation: &str) -> Vec<String> {
    let lower = relation.to_lowercase();
    let Ok(mut stmt) = conn.prepare("SELECT view_name, sql FROM duckdb_views() WHERE NOT internal")
    else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    }) else {
        return Vec::new();
    };
    rows.filter_map(|row| row.ok())
        .filter(|(name, _)| lower.contains(&name.to_lowercase()))
        .filter_map(|(_, sql)| sql)
        .collect()
}

/// What the cache holds for a relation.
#[derive(Clone)]
pub struct Cached {
    pub shell: Arc<Shell>,
    pub columns: HashMap<String, Arc<ColumnOverview>>,
    /// When the first of these was computed.
    pub since: SystemTime,
}

struct Entry {
    stamp: Stamp,
    cached: Cached,
    used: u64,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<String, Entry>,
    clock: u64,
}

impl Cache {
    fn lookup(&mut self, relation: &str, stamp: &Stamp) -> Option<Cached> {
        self.clock += 1;
        let clock = self.clock;
        match self.entries.get_mut(relation) {
            Some(entry) if entry.stamp == *stamp => {
                entry.used = clock;
                Some(entry.cached.clone())
            }
            Some(_) => {
                self.entries.remove(relation);
                None
            }
            None => None,
        }
    }

    fn store_shell(&mut self, relation: &str, stamp: &Stamp, shell: Arc<Shell>) {
        self.clock += 1;
        let used = self.clock;
        self.entries.insert(
            relation.to_string(),
            Entry {
                stamp: stamp.clone(),
                cached: Cached {
                    shell,
                    columns: HashMap::new(),
                    since: SystemTime::now(),
                },
                used,
            },
        );
        while self.entries.len() > RELATIONS {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    fn store_columns(&mut self, relation: &str, stamp: &Stamp, columns: &[Arc<ColumnOverview>]) {
        let Some(entry) = self.entries.get_mut(relation) else {
            return;
        };
        if entry.stamp != *stamp {
            return;
        }
        for column in columns {
            entry
                .cached
                .columns
                .insert(column.name.clone(), column.clone());
        }
    }
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);

fn cache() -> std::sync::MutexGuard<'static, Cache> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// What is cached for `relation` under `stamp`. An entry computed under any
/// other stamp is dropped, not returned.
pub fn lookup(relation: &str, stamp: &Stamp) -> Option<Cached> {
    cache().lookup(relation, stamp)
}

/// Keep `shell` for `relation`, replacing whatever was kept for it.
pub fn store_shell(relation: &str, stamp: &Stamp, shell: Arc<Shell>) {
    cache().store_shell(relation, stamp, shell);
}

/// Keep `columns` for `relation`, if what is kept for it was computed under
/// `stamp` — profiles from before a change never join those from after it.
pub fn store_columns(relation: &str, stamp: &Stamp, columns: &[Arc<ColumnOverview>]) {
    cache().store_columns(relation, stamp, columns);
}

/// Forget `relation`, so the next look profiles it afresh.
pub fn forget(relation: &str) {
    cache().entries.remove(relation);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overview::{Distribution, Shape};

    fn column(name: &str) -> Arc<ColumnOverview> {
        Arc::new(ColumnOverview {
            name: name.into(),
            data_type: "INTEGER".into(),
            shape: Shape::Numeric,
            null_fraction: 0.,
            approx_unique: 1,
            min: None,
            max: None,
            median: None,
            distribution: Distribution::None,
        })
    }

    fn shell() -> Arc<Shell> {
        Arc::new(Shell {
            columns: vec![("a".into(), "INTEGER".into())],
            row_count: 1,
        })
    }

    #[test]
    fn literals_are_read_with_their_escapes_and_comments_are_skipped() {
        assert_eq!(
            literals_of("SELECT 'it''s', \"a\"\"b\" -- don't\nFROM read_csv('/tmp/x.csv')"),
            ["it's", "a\"b", "/tmp/x.csv"]
        );
    }

    #[test]
    fn a_stamp_moves_with_the_generation_and_with_its_files() {
        let conn = Connection::open_in_memory().unwrap();
        let dir = std::env::temp_dir().join(format!("ducklocal-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("data.csv");
        std::fs::write(&csv, "n\n1\n").unwrap();
        let path = csv.to_string_lossy().replace('\\', "/");
        conn.execute_batch(&format!(
            "CREATE VIEW data AS SELECT * FROM read_csv('{path}')"
        ))
        .unwrap();

        // The generation is process-wide and tests running alongside move
        // it, so the files are compared on their own, and the generation
        // only for having moved forward.
        let direct = format!("(SELECT * FROM '{path}')");
        let first = stamp_of(&conn, &direct);
        assert_eq!(first.files.len(), 1);
        assert_eq!(stamp_of(&conn, &direct).files, first.files);
        // Through the view the sidebar makes, the same file.
        let through = stamp_of(&conn, "(SELECT * FROM data)");
        assert_eq!(through.files, first.files);

        std::fs::write(&csv, "n\n1\n2\n").unwrap();
        assert_ne!(stamp_of(&conn, &direct).files, first.files);

        let before = stamp_of(&conn, "t");
        data_changed();
        assert!(stamp_of(&conn, "t").generation > before.generation);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn stamp(generation: u64) -> Stamp {
        Stamp {
            generation,
            files: Vec::new(),
            matched: 0,
        }
    }

    #[test]
    fn an_entry_serves_only_its_own_stamp() {
        let mut cache = Cache::default();
        let relation = "(SELECT 1)";
        let (now, later) = (stamp(1), stamp(2));
        cache.store_shell(relation, &now, shell());
        cache.store_columns(relation, &now, &[column("a")]);
        let cached = cache.lookup(relation, &now).expect("cached");
        assert!(cached.columns.contains_key("a"));

        // Columns profiled under another stamp do not join it…
        cache.store_columns(relation, &later, &[column("b")]);
        assert!(!cache
            .lookup(relation, &now)
            .unwrap()
            .columns
            .contains_key("b"));
        // …and a lookup under another stamp drops it.
        assert!(cache.lookup(relation, &later).is_none());
        assert!(cache.lookup(relation, &now).is_none());
    }

    #[test]
    fn the_least_recently_used_relation_goes_first() {
        let mut cache = Cache::default();
        let now = stamp(7);
        let name = |i: usize| format!("(SELECT {i})");
        for i in 0..RELATIONS {
            cache.store_shell(&name(i), &now, shell());
        }
        // Touch the first, so the second is now the oldest.
        assert!(cache.lookup(&name(0), &now).is_some());
        cache.store_shell(&name(RELATIONS), &now, shell());
        assert_eq!(cache.entries.len(), RELATIONS);
        assert!(cache.lookup(&name(0), &now).is_some());
        assert!(cache.lookup(&name(1), &now).is_none());
    }
}
