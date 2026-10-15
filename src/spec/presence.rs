//! An agent's presence on a dashboard: which plots it is reading, checking
//! or changing, shown live on the open tab the way a collaborator's cursor
//! shows in a shared document.
//!
//! The agent works through the CLI and edits the spec as a file, so the
//! window learns what it is doing two ways:
//!
//! - The commands an agent runs on a spec say so as they run: `ducklocal
//!   check` marks each plot while its query runs, `ducklocal comments` marks
//!   the plots whose threads it reads or answers, and `ducklocal presence`
//!   says outright that it is reading or about to edit some plots — the one
//!   thing the window cannot see, since the edit itself is the agent's own
//!   file write. Each is one fire-and-forget line to the running window
//!   (`remote::notify`); with no window up, nothing waits and nothing fails.
//! - When the spec changes on disk while an agent has been present, the
//!   window compares the plots before and after, and marks the ones the edit
//!   touched as just changed by it.
//!
//! Every mark expires on its own, so an agent that stops mid-way never leaves
//! a plot marked for good.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::cli::{parse_args, Arg, CliError, FlagSpec};

/// What an agent is doing to a plot, or to the whole dashboard when no plot
/// is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Activity {
    Reading,
    Editing,
    Checking,
    Replying,
    /// Done: its marks go.
    Idle,
}

impl Activity {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "reading" => Some(Self::Reading),
            "editing" => Some(Self::Editing),
            "idle" | "done" => Some(Self::Idle),
            _ => None,
        }
    }
}

/// One line from an agent's command to the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    /// The spec, absolute: the window's working directory is not the
    /// command's.
    pub path: String,
    /// Who is at work, as the window labels it.
    pub agent: String,
    pub activity: Activity,
    /// The plots it is about; none means the dashboard as a whole.
    #[serde(default)]
    pub plots: Vec<String>,
}

/// The name the window shows for whoever runs this command: `DUCKLOCAL_AGENT`
/// when set, else the agent the environment gives away, else "Agent".
pub fn agent_name() -> String {
    if let Some(name) = std::env::var("DUCKLOCAL_AGENT")
        .ok()
        .filter(|name| !name.trim().is_empty())
    {
        return name.trim().to_string();
    }
    let set = |name: &str| std::env::var_os(name).is_some();
    if set("CLAUDECODE") || set("CLAUDE_CODE_ENTRYPOINT") {
        "Claude".into()
    } else if set("CODEX_SANDBOX") || set("CODEX_HOME") {
        "Codex".into()
    } else if set("CURSOR_AGENT") || set("CURSOR_TRACE_ID") {
        "Cursor".into()
    } else if set("GEMINI_CLI") {
        "Gemini".into()
    } else {
        "Agent".into()
    }
}

/// Tell the running window, if there is one, what this command is doing to
/// `spec`. Never waits on a window that is not there, and never fails the
/// command: presence is a courtesy to the person watching.
pub fn announce(spec: &std::path::Path, activity: Activity, plots: Vec<String>) {
    let path = std::env::current_dir()
        .map(|cwd| cwd.join(spec))
        .unwrap_or_else(|_| spec.to_path_buf());
    crate::remote::notify(&Update {
        path: path.to_string_lossy().into_owned(),
        agent: agent_name(),
        activity,
        plots,
    });
}

/// Marks the whole dashboard as being checked while it lives, and takes the
/// mark off when dropped — whichever way the check ends.
pub struct Checking<'a> {
    spec: &'a std::path::Path,
}

impl<'a> Checking<'a> {
    pub fn start(spec: &'a std::path::Path) -> Self {
        announce(spec, Activity::Checking, Vec::new());
        Self { spec }
    }
}

impl Drop for Checking<'_> {
    fn drop(&mut self) {
        announce(self.spec, Activity::Idle, Vec::new());
    }
}

/// How long a mark stays without being said again.
fn lasts(activity: Activity) -> Duration {
    match activity {
        // Long enough to see; a plot read is over in a moment.
        Activity::Reading | Activity::Replying => Duration::from_secs(6),
        // Until the edit lands on disk, which takes the mark off sooner.
        Activity::Editing => Duration::from_secs(45),
        Activity::Checking => Duration::from_millis(1800),
        Activity::Idle => Duration::ZERO,
    }
}

/// What a plot shows, or the dashboard as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    Reading,
    Editing,
    Checking,
    Replying,
    /// The agent's edit just changed it.
    Edited,
    /// The agent's check of the whole dashboard just finished.
    Checked,
}

impl From<Activity> for Shown {
    fn from(activity: Activity) -> Self {
        match activity {
            Activity::Reading => Shown::Reading,
            Activity::Editing => Shown::Editing,
            Activity::Checking => Shown::Checking,
            Activity::Replying => Shown::Replying,
            Activity::Idle => Shown::Checked,
        }
    }
}

impl Shown {
    /// The i18n key of the tag's text, `{}` the agent's name.
    pub fn label_key(self) -> &'static str {
        match self {
            Shown::Reading => "presence.reading",
            Shown::Editing => "presence.editing",
            Shown::Checking => "presence.checking",
            Shown::Replying => "presence.replying",
            Shown::Edited => "presence.edited",
            Shown::Checked => "presence.checked",
        }
    }

    /// Whether the mark shows work in progress, which pulses, rather than
    /// work just done, which fades.
    pub fn is_ongoing(self) -> bool {
        !matches!(self, Shown::Edited | Shown::Checked)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub agent: String,
    pub shown: Shown,
    pub until: Instant,
}

/// Every agent mark on one dashboard.
#[derive(Debug, Default)]
pub struct Marks {
    plots: HashMap<String, Mark>,
    whole: Option<Mark>,
    /// The agent heard from last, and when: an edit landing soon after is
    /// taken as its.
    last_agent: Option<(String, Instant)>,
}

/// How long after an agent was last heard from an edit on disk is still
/// credited to it.
const EDIT_CREDIT: Duration = Duration::from_secs(120);
/// How long a just-changed plot stays marked.
pub const EDITED_LASTS: Duration = Duration::from_secs(5);
/// How long "checked" stays on the dashboard after a check ends.
const CHECKED_LASTS: Duration = Duration::from_millis(2500);

impl Marks {
    pub fn apply(&mut self, update: &Update, now: Instant) {
        self.last_agent = Some((update.agent.clone(), now));
        let agent = update.agent.as_str();
        if update.activity == Activity::Idle {
            if update.plots.is_empty() {
                self.plots
                    .retain(|_, mark| mark.agent != agent || !mark.shown.is_ongoing());
                // A check that ends leaves "checked" up a moment, so even a
                // check over in milliseconds is seen to have happened.
                self.whole = match self.whole.take() {
                    Some(mark) if mark.agent == agent && mark.shown == Shown::Checking => {
                        Some(Mark {
                            agent: mark.agent,
                            shown: Shown::Checked,
                            until: now + CHECKED_LASTS,
                        })
                    }
                    Some(mark) if mark.agent == agent => None,
                    other => other,
                };
            } else {
                for plot in &update.plots {
                    if self.plots.get(plot).is_some_and(|mark| mark.agent == agent) {
                        self.plots.remove(plot);
                    }
                }
            }
            return;
        }
        let mark = Mark {
            agent: update.agent.clone(),
            shown: update.activity.into(),
            until: now + lasts(update.activity),
        };
        if update.plots.is_empty() {
            self.whole = Some(Mark {
                // The dashboard-wide check runs as long as the command does:
                // its end takes it off.
                until: now + Duration::from_secs(60),
                ..mark
            });
        } else {
            for plot in &update.plots {
                self.plots.insert(plot.clone(), mark.clone());
            }
        }
    }

    /// The spec changed on disk and `changed` are the plots it changed. With
    /// an agent heard from lately, they are marked as its edit, and its
    /// "editing" marks come off: the edit has landed.
    pub fn edited(&mut self, changed: &[String], now: Instant) {
        let Some((agent, at)) = self.last_agent.clone() else {
            return;
        };
        if now.duration_since(at) > EDIT_CREDIT {
            return;
        }
        self.plots
            .retain(|_, mark| !(mark.agent == agent && mark.shown == Shown::Editing));
        for plot in changed {
            self.plots.insert(
                plot.clone(),
                Mark {
                    agent: agent.clone(),
                    shown: Shown::Edited,
                    until: now + EDITED_LASTS,
                },
            );
        }
    }

    /// Drop what has expired; whether anything went.
    pub fn prune(&mut self, now: Instant) -> bool {
        let before = self.plots.len() + usize::from(self.whole.is_some());
        self.plots.retain(|_, mark| mark.until > now);
        if self.whole.as_ref().is_some_and(|mark| mark.until <= now) {
            self.whole = None;
        }
        before != self.plots.len() + usize::from(self.whole.is_some())
    }

    pub fn is_empty(&self) -> bool {
        self.plots.is_empty() && self.whole.is_none()
    }

    pub fn on_plot(&self, plot: &str) -> Option<&Mark> {
        self.plots.get(plot)
    }

    pub fn on_dashboard(&self) -> Option<&Mark> {
        self.whole.as_ref()
    }
}

pub const HELP: &str = "\
Usage: ducklocal presence FILE --doing ACTIVITY [--plot NAMES] [--agent NAME]

Show the person watching a dashboard in the running DuckLocal window what
you are doing to it: the plots you are reading, or about to edit. The
window marks them live, as a shared document marks a collaborator's
cursor. Say `editing` before you change a plot's block and the window
marks it until your edit lands, then shows what the edit changed.

Options:
  --doing ACTIVITY   reading, editing, or idle (clears your marks)
  --plot NAMES       Plots, as in plot \"NAME\", comma-separated
                     (--plot revenue,orders). Without it, the dashboard
                     as a whole.
  --agent NAME       The name shown; default DUCKLOCAL_AGENT, else the
                     agent the environment names (Claude, Codex, ...)

`ducklocal check` and `ducklocal comments` mark the plots they work on
without being asked. Output: {delivered}: false when no window is running,
which is not an error. Exit 2 for arguments, or a plot the spec lacks.
";

const SPEC: &[FlagSpec] = &[
    FlagSpec {
        name: "--doing",
        takes_value: true,
    },
    FlagSpec {
        name: "--plot",
        takes_value: true,
    },
    FlagSpec {
        name: "--agent",
        takes_value: true,
    },
];

/// `ducklocal presence FILE --doing ACTIVITY [--plot NAME ...]`.
pub fn run(args: &[OsString]) -> Result<String, CliError> {
    let mut file: Option<PathBuf> = None;
    let mut activity = None;
    let mut plots = Vec::new();
    let mut agent = None;
    for arg in parse_args("presence", args, SPEC, &["--agent"])? {
        let text = |value: Option<OsString>| value.map(|v| v.to_string_lossy().into_owned());
        match arg {
            Arg::Flag("--doing", value) => {
                let value = text(value).unwrap_or_default();
                activity = Some(Activity::parse(&value).ok_or_else(|| {
                    CliError::argument(format!("--doing {value:?}: say reading, editing or idle"))
                })?);
            }
            // One flag, a comma-separated list: the argument walker takes
            // each option once.
            Arg::Flag("--plot", value) => plots.extend(
                text(value)
                    .unwrap_or_default()
                    .split(',')
                    .map(str::trim)
                    .filter(|plot| !plot.is_empty())
                    .map(String::from),
            ),
            Arg::Flag("--agent", value) => agent = text(value),
            Arg::Positional(value) => {
                if file.is_some() {
                    return Err(CliError::argument("Name one .dash file"));
                }
                file = Some(PathBuf::from(value));
            }
            Arg::Flag(..) => unreachable!("presence declares every flag it matches"),
        }
    }
    let file = file.ok_or_else(|| CliError::argument("Name the .dash file you are working on"))?;
    let activity =
        activity.ok_or_else(|| CliError::argument("Missing --doing: reading, editing or idle"))?;
    let source = std::fs::read_to_string(&file).map_err(|e| CliError::failure("io", e))?;
    // A plot the spec does not have would mark nothing; say so rather than
    // let the agent think the person can see it.
    if !plots.is_empty() {
        let known = super::comments::plot_names(&source);
        if let Some(missing) = plots.iter().find(|plot| !known.contains(plot)) {
            return Err(CliError::argument(format!(
                "{}: no plot named {missing:?}",
                file.display()
            ))
            .with_hint(format!("Plots: {}", known.join(", "))));
        }
    }
    let path = std::env::current_dir()
        .map(|cwd| cwd.join(&file))
        .unwrap_or(file);
    let delivered = crate::remote::notify(&Update {
        path: path.to_string_lossy().into_owned(),
        agent: agent.unwrap_or_else(agent_name),
        activity,
        plots,
    });
    Ok(json!({ "delivered": delivered }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(activity: Activity, plots: &[&str]) -> Update {
        Update {
            path: "d.dash".into(),
            agent: "Claude".into(),
            activity,
            plots: plots.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn a_mark_lasts_until_it_expires_or_is_cleared() {
        let now = Instant::now();
        let mut marks = Marks::default();
        marks.apply(&update(Activity::Reading, &["revenue"]), now);
        assert_eq!(marks.on_plot("revenue").unwrap().shown, Shown::Reading);
        assert!(!marks.prune(now + Duration::from_secs(1)));
        assert!(marks.prune(now + Duration::from_secs(7)));
        assert!(marks.is_empty());

        marks.apply(&update(Activity::Editing, &["revenue", "orders"]), now);
        marks.apply(&update(Activity::Idle, &["orders"]), now);
        assert!(marks.on_plot("orders").is_none());
        assert_eq!(marks.on_plot("revenue").unwrap().shown, Shown::Editing);
    }

    #[test]
    fn a_check_that_ends_leaves_checked_up_a_moment() {
        let now = Instant::now();
        let mut marks = Marks::default();
        marks.apply(&update(Activity::Checking, &[]), now);
        marks.apply(&update(Activity::Checking, &["revenue"]), now);
        marks.apply(&update(Activity::Idle, &[]), now);
        assert_eq!(marks.on_dashboard().unwrap().shown, Shown::Checked);
        assert!(marks.on_plot("revenue").is_none());
        marks.prune(now + Duration::from_secs(3));
        assert!(marks.is_empty());
    }

    #[test]
    fn an_edit_is_credited_only_to_an_agent_heard_from_lately() {
        let now = Instant::now();
        let mut marks = Marks::default();
        marks.edited(&["revenue".into()], now);
        assert!(
            marks.is_empty(),
            "no agent, no credit: the person edited it"
        );

        marks.apply(&update(Activity::Editing, &["revenue"]), now);
        marks.edited(&["orders".into()], now + Duration::from_secs(3));
        assert!(marks.on_plot("revenue").is_none(), "the edit landed");
        assert_eq!(marks.on_plot("orders").unwrap().shown, Shown::Edited);

        let mut later = Marks::default();
        later.apply(&update(Activity::Reading, &["revenue"]), now);
        later.edited(&["revenue".into()], now + Duration::from_secs(600));
        assert_ne!(
            later.on_plot("revenue").map(|m| m.shown),
            Some(Shown::Edited)
        );
    }
}
