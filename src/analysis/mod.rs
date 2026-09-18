//! JS-authored analysis panels, as workspace tabs.
//!
//! A panel is a directory holding a `main.js` that exports a gpui-shell view.
//! It opens as a tab beside the SQL tabs, closes like one, and comes back on
//! the next launch the way a registered data file does. Its script reaches this
//! app's own DuckDB connection through the `ducklocal` host module — the same
//! connection the main window queries, at the app's privileges. It is a second
//! face on the data, not a sandbox: SQL from a panel can do anything the user
//! could do from the SQL editor.
//!
//! ```text
//! src/analysis/runtime.rs  the script runtime every panel shares
//! src/analysis/host.rs     the `ducklocal` module and the SQL→plain-data bridge
//! src/analysis/view.rs     AnalysisHost, the view a panel tab renders
//! src/analysis/panels.rs   which panels are open, remembered between launches
//! src/analysis/watch.rs    the host-side reload watcher
//! ```
//!
//! See `docs/analysis-panel.md` for the script-facing description.

pub mod host;
pub mod panels;
pub mod runtime;
pub mod view;
pub mod watch;
