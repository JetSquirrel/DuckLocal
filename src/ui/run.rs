//! One Run, shared by everything that has an editor of SQL to execute: a
//! query tab and each cell of a notebook.
//!
//! The work off the UI thread is the same wherever the SQL came from — run
//! it as a script, record it in the history, keep a typed `LOAD`, reload the
//! catalog when the SQL may have changed it — and so is what comes back to
//! the window: the history, the catalog, the search path and the status
//! bar's numbers. Only where the result is shown differs, and that stays
//! with the caller.

use gpui_kit::component::input::{
    RangeDecoration, RangeDecorationCollection, RangeDecorationStyle, TextDecoration,
    TextDecorationCollection,
};
use gpui_kit::component::ActiveTheme;
use gpui_kit::*;

use crate::history::HistoryEntry;
use crate::query::QueryOutcome;
use crate::schema::DatabaseInfo;
use crate::script::{Piece, PieceKind, ScriptOutcome, Step};
use crate::state::{AppState, QueryStats};

/// What running a script produced, ready to hand back to the window.
pub(crate) struct Executed {
    pub script: ScriptOutcome,
    history: Vec<HistoryEntry>,
    /// The reloaded catalog, when the SQL may have changed it.
    catalog: Option<Vec<DatabaseInfo>>,
}

/// Run `sql` on the shared connection, blocking: call it inside
/// `smol::unblock`.
pub(crate) fn execute(sql: &str) -> Executed {
    execute_searching(None, sql)
}

/// `execute`, with relative file paths also looked up in `search_path`: a
/// notebook's folder, so `FROM 'orders.csv'` finds the file beside it
/// whatever folder the app was started in.
pub(crate) fn execute_searching(search_path: Option<&str>, sql: &str) -> Executed {
    // Decided before the SQL is handed to the history entry.
    let reload_catalog = crate::query::may_change_catalog(sql);
    let script = crate::script::run_searching(search_path, sql);
    // A `LOAD` typed here is kept for the next connection, as the sidebar's
    // Load button is.
    if crate::extensions::may_load(sql) {
        let _ = crate::db::with_connection(crate::extensions::remember_loaded_of);
    }
    // A script is one history entry: its total time, the row count of the
    // result it shows, and its first error.
    let (duration_ms, row_count, ok, error) = match &script {
        Ok(script) => {
            let mut duration_ms = 0i64;
            let mut error = None;
            for step in &script.steps {
                match &step.outcome {
                    Some(Ok(outcome)) => duration_ms += outcome_elapsed(outcome) as i64,
                    Some(Err(e)) => error = Some(e.to_string()),
                    None => {}
                }
            }
            let row_count = script
                .primary()
                .and_then(|ix| script.steps[ix].outcome.as_ref())
                .and_then(|outcome| outcome.as_ref().ok())
                .map(outcome_count);
            (duration_ms, row_count, error.is_none(), error)
        }
        Err(e) => (0, None, false, Some(e.to_string())),
    };
    crate::history::record(&HistoryEntry {
        id: 0,
        sql: sql.to_string(),
        started_at: crate::history::now_timestamp(),
        duration_ms,
        row_count,
        ok,
        error,
    })
    .ok();
    let history = crate::history::recent(200).unwrap_or_default();
    // A plain read leaves the sidebar valid, so skip the reload instead of
    // paying for it on the way back from every query.
    let catalog = reload_catalog.then(|| crate::schema::load_catalog().unwrap_or_default());
    let script = script.unwrap_or_else(|e| ScriptOutcome {
        // The connection itself was unavailable: one failure for the whole
        // script, as a single statement's would be.
        steps: Vec::from([Step {
            piece: Piece {
                kind: PieceKind::Sql,
                range: 0..sql.len(),
            },
            outcome: Some(Err(e)),
        }]),
        search_path: None,
    });
    Executed {
        script,
        history,
        catalog,
    }
}

impl Executed {
    /// The step whose outcome the results panel opens on, with that outcome.
    pub fn primary(&self) -> Option<(usize, &anyhow::Result<QueryOutcome>)> {
        let ix = self.script.primary()?;
        Some((ix, self.script.steps[ix].outcome.as_ref()?))
    }

    /// The status bar's numbers for the primary result; `None` for a failure.
    pub fn stats(&self) -> Option<QueryStats> {
        match self.primary() {
            Some((_, Ok(QueryOutcome::Rows(result)))) => Some(QueryStats {
                elapsed_ms: result.elapsed_ms,
                rows: result.row_count(),
                cols: result.columns.len(),
            }),
            Some((_, Ok(QueryOutcome::Affected { elapsed_ms, count }))) => Some(QueryStats {
                elapsed_ms: *elapsed_ms,
                rows: *count as usize,
                cols: 0,
            }),
            Some((_, Err(_))) | None => None,
        }
    }

    /// Hand the window what every run changes, then the steps for the caller
    /// to show.
    pub fn publish(self, state: &Entity<AppState>, sql: &str, cx: &mut App) -> Vec<Step> {
        let stats = self.stats();
        let Executed {
            script,
            history,
            catalog,
        } = self;
        let search_path = script.search_path;
        state.update(cx, |s, cx| {
            s.set_history(history, cx);
            if let Some(catalog) = catalog {
                s.set_catalog(catalog, cx);
            }
            if search_path.is_some() {
                s.set_search_path(search_path, cx);
            }
            if let Some(stats) = stats {
                s.set_last_query(stats, sql.to_string(), cx);
            }
        });
        script.steps
    }
}

/// Underline the span a failed run's error named, if it named one: a wavy
/// underline on the token and a quiet fill behind it, both in the theme's
/// danger color. The collections follow edits and sit outside undo history,
/// so nothing here is replayed by ⌘Z.
/// `offset` is where `sql` starts in the editor: past zero for a selection.
/// `piece` is the byte range of the statement that failed: DuckDB's position
/// is relative to that statement, not to the whole script.
pub(crate) fn mark_sql_error(
    sql: &str,
    offset: usize,
    squiggles: &TextDecorationCollection,
    fill: &RangeDecorationCollection,
    piece: std::ops::Range<usize>,
    error: &anyhow::Error,
    cx: &mut App,
) {
    let Some(statement) = sql.get(piece.clone()) else {
        return;
    };
    let Some(range) = crate::query::error_byte_range(statement, &error.to_string()) else {
        return;
    };
    let start = offset + piece.start;
    let range = start + range.start..start + range.end;
    let danger = cx.theme().danger;
    squiggles.set(
        vec![TextDecoration::new(
            range.clone(),
            HighlightStyle {
                underline: Some(UnderlineStyle {
                    color: Some(danger),
                    thickness: px(1.),
                    wavy: true,
                }),
                ..Default::default()
            },
        )],
        cx,
    );
    fill.set(
        vec![RangeDecoration::new(range)
            .with_style(RangeDecorationStyle::Fill)
            .with_color(danger.opacity(0.1))],
        cx,
    );
}

pub(crate) fn outcome_elapsed(outcome: &QueryOutcome) -> u128 {
    match outcome {
        QueryOutcome::Rows(result) => result.elapsed_ms,
        QueryOutcome::Affected { elapsed_ms, .. } => *elapsed_ms,
    }
}

pub(crate) fn outcome_count(outcome: &QueryOutcome) -> i64 {
    match outcome {
        QueryOutcome::Rows(result) => result.row_count() as i64,
        QueryOutcome::Affected { count, .. } => *count as i64,
    }
}

/// The editor inserts `"\n" + indent` before emitting `PressEnter`; the
/// cursor then sits right after the insertion. Delete exactly that
/// insertion — walk back over the indent, then require a newline, and do
/// nothing if the text before the cursor does not match that shape.
/// With multiple cursors only the active one is repaired.
///
/// ⌘↵ reaches an editor as `secondary-enter`, which the Input key context
/// binds to "insert newline", so a run from the keyboard has a newline to
/// take back first.
pub(crate) fn remove_enter_newline(
    editor: &Entity<gpui_kit::component::input::EditorState>,
    window: &mut Window,
    cx: &mut App,
) {
    let range = editor.update(cx, |editor, _| {
        let cursor = editor.cursor();
        let text = editor.text();
        let mut start = cursor;
        let mut chars = text.chars_at(cursor);
        let mut prev = chars.prev();
        // The inserted indent is plain spaces/tabs (ASCII), so byte
        // arithmetic on `start` stays on char boundaries.
        while matches!(prev, Some(' ' | '\t')) {
            start -= 1;
            prev = chars.prev();
        }
        if prev != Some('\n') {
            return None;
        }
        Some(start - 1..cursor)
    });
    if let Some(range) = range {
        editor.update(cx, |editor, cx| {
            editor.set_selected_range(range, cx);
            editor.replace("", window, cx);
        });
    }
}
