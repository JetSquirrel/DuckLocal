//! The script runtime the panels run on.
//!
//! One per process, created with the first panel and owned by the workspace —
//! the view that owns the panel tabs — so it is dropped with the window, while
//! the application is still alive.
//!
//! **That ownership and that timing are the point, not an implementation
//! detail.** A `ShellRuntime` must not be dropped after the application is
//! gone: releasing
//! a mounted application's QuickJS handles reaches back into state the
//! application owns, and a runtime kept in a thread local is dropped at thread
//! exit, after the application has been destroyed. The process aborts with
//! `cannot access a Thread Local Storage value during or after destruction`.
//! The shell's own host keeps its runtime in the same kind of place this does:
//! host state that dies with the window rather than with the thread.

use std::rc::Rc;

use gpui_kit::{App, Global};
use gpui_shell::ShellRuntime;

use crate::analysis::host;

/// Whether the shell's application-wide globals have been installed.
///
/// They are installed once, not once per window: installing them resets what
/// the base theme layer holds, and by the time a panel opens the main window
/// is already drawn in this application's theme.
struct ShellInstalled;

impl Global for ShellInstalled {}

/// Creates a script runtime with the `ducklocal` module exported.
///
/// The module is exported before any application is loaded, because an import
/// is resolved while a script's module graph is linked: a module registered
/// afterwards is not in that graph.
pub fn create(cx: &mut App) -> anyhow::Result<Rc<ShellRuntime>> {
    if !cx.has_global::<ShellInstalled>() {
        // The component catalog is what makes the toolkit's components
        // available to a script, and its initializer is what installs the
        // globals those components need at run time.
        let components = gpui_component_shell::components()
            .map_err(|error| anyhow::anyhow!("gpui-component-shell: {error}"))?;
        gpui_component_shell::init(cx);
        gpui_shell::init_with_components(cx, &components);
        cx.set_global(ShellInstalled);
    }

    gpui_shell::export_module(host::module()).map_err(|error| anyhow::anyhow!("{error}"))?;

    let components = gpui_component_shell::components()
        .map_err(|error| anyhow::anyhow!("gpui-component-shell: {error}"))?;
    ShellRuntime::new_with_components(cx, components)
        .map_err(|error| anyhow::anyhow!("Failed to create the script runtime: {error}"))
}
