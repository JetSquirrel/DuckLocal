//! JS-authored analysis apps, as workspace tabs.
//!
//! An app is a directory holding a `main.js` that exports a gpui-shell view.
//! It opens as a tab beside the SQL tabs, closes like one, and comes back on
//! the next launch the way a registered data file does. Its script reaches the
//! database the app has open through the `ducklocal` host module — on a
//! connection of its own, so an app's SQL and the editor's do not wait on each
//! other — at the app's privileges. It is a second face on the data, not a
//! sandbox: SQL from an app can do anything the user could do from the SQL
//! editor.
//!
//! ```text
//! src/analysis/runtime.rs  the script runtime every app shares
//! src/analysis/host.rs     the `ducklocal` module and the SQL→plain-data bridge
//! src/analysis/view.rs     AnalysisHost, the view an app tab renders
//! src/analysis/apps.rs     which apps are open, remembered between launches
//! src/analysis/watch.rs    the host-side reload watcher
//! ```
//!
//! See <https://ducklocal.app/docs/analysis-app> for the script-facing
//! description.

pub mod apps;
pub mod host;
pub mod runtime;
pub mod view;
pub mod watch;
