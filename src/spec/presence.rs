//! An agent's presence on a dashboard, kept quiet: one name in the corner
//! while it works, and its edits marked on the plots until the person has
//! seen them.
//!
//! The window works it out from what the agent does, rather than asking the
//! agent to narrate — agents skip a courtesy no command fails without:
//!
//! - Every command that names a spec says so to the running window, as one
//!   fire-and-forget line (`remote::notify`): `check`, `comments` and
//!   `presence`. The first starts a [`Session`]; each one after keeps it
//!   going; [`QUIET_ENDS`] of silence, or `presence --doing idle`, ends it.
//! - When the spec changes on disk, the window compares the plots before and
//!   after and credits the changed, added and removed ones to the agent
//!   heard from last. Those marks stay until the plot has been in plain view
//!   for [`SEEN_AFTER`]; a removed plot's note, until it is dismissed.
//! - `presence --doing editing --plot NAMES` is the one thing said ahead of
//!   time: where an edit is about to land, plots not yet written included.
//!
//! Reads, replies and checks only keep the session going and fill its log;
//! they put nothing on the plots.

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

/// An agent's work on a dashboard ends this long after its last command.
pub const QUIET_ENDS: Duration = Duration::from_secs(30);
/// How long a finished agent's name stays up, once nothing waits on the
/// person.
pub const DONE_LINGERS: Duration = Duration::from_secs(10);
/// How long a changed plot must be in plain view before it counts as seen.
pub const SEEN_AFTER: Duration = Duration::from_millis(1500);
/// How long after an agent was last heard from an edit on disk is still
/// credited to it: an agent thinks between its commands.
const EDIT_CREDIT: Duration = Duration::from_secs(120);

/// One thing a session did, for the person who opens its log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Did {
    /// Looked at these plots (`presence --doing reading`, or the threads on
    /// them through `comments`).
    Read(Vec<String>),
    /// Ran `check` over the dashboard.
    Checked,
    /// Answered the threads on these plots.
    Replied(Vec<String>),
    /// An edit landed on disk.
    Edited {
        changed: usize,
        added: usize,
        removed: usize,
    },
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// Wall clock, for the log's "14:02".
    pub at: chrono::DateTime<chrono::Local>,
    pub did: Did,
}

/// One agent's run of work on a dashboard: from its first command that names
/// the spec to [`QUIET_ENDS`] after its last, or its `--doing idle`.
#[derive(Clone, Debug)]
pub struct Session {
    pub agent: String,
    last_heard: Instant,
    /// When it finished, by going quiet or by saying so.
    done_at: Option<Instant>,
    /// A `check` is under way: its closing `idle` ends the check, not the
    /// session.
    checking: bool,
    pub log: Vec<Entry>,
}

impl Session {
    fn new(agent: &str, now: Instant) -> Self {
        Self {
            agent: agent.to_string(),
            last_heard: now,
            done_at: None,
            checking: false,
            log: Vec::new(),
        }
    }

    pub fn is_working(&self) -> bool {
        self.done_at.is_none()
    }

    fn heard(&mut self, now: Instant) {
        self.last_heard = now;
        self.done_at = None;
    }

    /// Note what was done; one read after another is one line, not a dozen.
    fn note(&mut self, did: Did) {
        if let (
            Some(Entry {
                did: Did::Read(seen),
                ..
            }),
            Did::Read(more),
        ) = (self.log.last_mut(), &did)
        {
            for plot in more {
                if !seen.contains(plot) {
                    seen.push(plot.clone());
                }
            }
            return;
        }
        self.log.push(Entry {
            at: chrono::Local::now(),
            did,
        });
    }
}

/// What an agent's edit did to a plot, kept until the person has seen it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Changed,
    Added,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeMark {
    pub agent: String,
    pub change: Change,
    /// How long it has been in plain view so far.
    pub seen_for: Duration,
}

/// A plot an agent's edit took out, until the person dismisses the note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    pub agent: String,
    pub plot: String,
    pub title: String,
}

/// Every agent's presence on one dashboard.
#[derive(Debug, Default)]
pub struct Presence {
    sessions: Vec<Session>,
    /// Plots an agent said it is about to edit, by name — including ones
    /// the spec does not have yet — until its edit lands or it finishes.
    editing: HashMap<String, String>,
    changes: HashMap<String, ChangeMark>,
    removed: Vec<Removed>,
}

impl Presence {
    /// An agent's command said what it is doing.
    pub fn apply(&mut self, update: &Update, now: Instant) {
        let session = self.session(&update.agent, now);
        session.heard(now);
        match update.activity {
            Activity::Checking => {
                if update.plots.is_empty() {
                    session.checking = true;
                    session.note(Did::Checked);
                }
            }
            Activity::Reading => {
                if !update.plots.is_empty() {
                    session.note(Did::Read(update.plots.clone()));
                }
            }
            Activity::Replying => session.note(Did::Replied(update.plots.clone())),
            Activity::Editing => {
                for plot in &update.plots {
                    self.editing.insert(plot.clone(), update.agent.clone());
                }
            }
            Activity::Idle => {
                if !update.plots.is_empty() {
                    self.editing.retain(|plot, agent| {
                        *agent != update.agent || !update.plots.contains(plot)
                    });
                } else if session.checking {
                    // A check's own end: the agent is still at work.
                    session.checking = false;
                } else {
                    session.done_at = Some(now);
                    let agent = update.agent.clone();
                    self.editing.retain(|_, who| *who != agent);
                }
            }
        }
    }

    fn session(&mut self, agent: &str, now: Instant) -> &mut Session {
        match self.sessions.iter().position(|s| s.agent == agent) {
            Some(ix) => &mut self.sessions[ix],
            None => {
                self.sessions.push(Session::new(agent, now));
                self.sessions.last_mut().expect("just pushed")
            }
        }
    }

    /// The spec changed on disk: `changed` and `added` are plots the edit
    /// touched, `removed` the ones it took out (name and title). It is
    /// credited to the agent heard from last, if lately — two at work on one
    /// file is rare, and the last to speak is the likeliest writer; with none,
    /// it was the person's own edit and nothing is marked.
    pub fn edited(
        &mut self,
        changed: &[String],
        added: &[String],
        removed: &[(String, String)],
        now: Instant,
    ) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .filter(|s| now.duration_since(s.last_heard) <= EDIT_CREDIT)
            .max_by_key(|s| s.last_heard)
        else {
            return;
        };
        if changed.is_empty() && added.is_empty() && removed.is_empty() {
            return;
        }
        session.heard(now);
        session.note(Did::Edited {
            changed: changed.len(),
            added: added.len(),
            removed: removed.len(),
        });
        let agent = session.agent.clone();
        self.editing.retain(|_, who| *who != agent);
        let mut mark = |plot: &String, change| {
            self.changes.insert(
                plot.clone(),
                ChangeMark {
                    agent: agent.clone(),
                    change,
                    seen_for: Duration::ZERO,
                },
            );
        };
        changed.iter().for_each(|plot| mark(plot, Change::Changed));
        added.iter().for_each(|plot| mark(plot, Change::Added));
        for (plot, title) in removed {
            self.changes.remove(plot);
            self.removed.retain(|r| &r.plot != plot);
            self.removed.push(Removed {
                agent: agent.clone(),
                plot: plot.clone(),
                title: title.clone(),
            });
        }
    }

    /// Time passes: quiet sessions finish, and finished ones go once their
    /// moment is over — unless `waiting` threads still wait on the person,
    /// which keeps the last one up. Whether anything shown changed.
    pub fn tick(&mut self, now: Instant, waiting: usize) -> bool {
        let mut moved = false;
        for session in &mut self.sessions {
            if session.done_at.is_none()
                && !session.checking
                && now.duration_since(session.last_heard) >= QUIET_ENDS
            {
                session.done_at = Some(now);
                moved = true;
            }
        }
        let finished: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| s.done_at.is_some())
            .map(|s| s.agent.clone())
            .collect();
        let before = self.editing.len();
        self.editing.retain(|_, agent| !finished.contains(agent));
        moved |= before != self.editing.len();
        let last = self.sessions.len().saturating_sub(1);
        let before = self.sessions.len();
        let mut ix = 0;
        self.sessions.retain(|s| {
            let keep = match s.done_at {
                None => true,
                Some(_) if waiting > 0 && ix == last => true,
                Some(at) => now.duration_since(at) < DONE_LINGERS,
            };
            ix += 1;
            keep
        });
        moved || before != self.sessions.len()
    }

    /// The changed plots that were in plain view for `elapsed` count it
    /// toward being seen; those seen long enough lose their mark. Whether
    /// any did.
    pub fn viewed(&mut self, plots: &[String], elapsed: Duration) -> bool {
        let before = self.changes.len();
        for plot in plots {
            if let Some(mark) = self.changes.get_mut(plot) {
                mark.seen_for += elapsed;
            }
        }
        self.changes.retain(|_, mark| mark.seen_for < SEEN_AFTER);
        before != self.changes.len()
    }

    /// The person dismissed the note of removed plots.
    pub fn dismiss_removed(&mut self) {
        self.removed.clear();
    }

    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    pub fn is_working(&self) -> bool {
        self.sessions.iter().any(Session::is_working)
    }

    /// The agent about to edit this plot, if one said so.
    pub fn editing(&self, plot: &str) -> Option<&str> {
        self.editing.get(plot).map(String::as_str)
    }

    /// Plots an agent is about to add: named in `--doing editing` but not in
    /// `existing`, with who is adding them, in name order.
    pub fn adding(&self, existing: &[String]) -> Vec<(String, String)> {
        let mut adding: Vec<(String, String)> = self
            .editing
            .iter()
            .filter(|(plot, _)| !existing.contains(plot))
            .map(|(plot, agent)| (plot.clone(), agent.clone()))
            .collect();
        adding.sort();
        adding
    }

    pub fn change(&self, plot: &str) -> Option<&ChangeMark> {
        self.changes.get(plot)
    }

    pub fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }

    pub fn removed(&self) -> &[Removed] {
        &self.removed
    }

    /// Whether there is nothing to show, and so nothing to tick for.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
            && self.editing.is_empty()
            && self.changes.is_empty()
            && self.removed.is_empty()
    }
}

pub const HELP: &str = "\
Usage: ducklocal presence FILE --doing ACTIVITY [--plot NAMES] [--agent NAME]

Tell the running DuckLocal window where you are about to edit a dashboard
the person has open. Your other commands on the spec (check, comments)
already show you at work, and the window marks what your edits change on
its own; `editing` adds where the next edit will land, before it does.

Options:
  --doing ACTIVITY   editing, reading, or idle (you are done)
  --plot NAMES       Plots, as in plot \"NAME\", comma-separated
                     (--plot revenue,orders). With editing, a plot you
                     are about to add may be named before it exists.
  --agent NAME       The name shown; default DUCKLOCAL_AGENT, else the
                     agent the environment names (Claude, Codex, ...)

Output: {delivered}: false when no window is running, which is not an
error. Exit 2 for arguments, or (reading, idle) a plot the spec lacks.
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
    // let the agent think the person can see it. Editing is the exception:
    // a plot about to be added is named before it exists, and the window
    // holds a place for it.
    if !plots.is_empty() && activity != Activity::Editing {
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
        from("Claude", activity, plots)
    }

    fn from(agent: &str, activity: Activity, plots: &[&str]) -> Update {
        Update {
            path: "d.dash".into(),
            agent: agent.into(),
            activity,
            plots: plots.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn names(plots: &[&str]) -> Vec<String> {
        plots.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn a_session_runs_from_the_first_command_to_quiet_or_idle() {
        let now = Instant::now();
        let mut presence = Presence::default();
        presence.apply(&update(Activity::Reading, &["revenue"]), now);
        assert!(presence.is_working());
        // A read puts nothing on the plot.
        assert!(presence.change("revenue").is_none());
        assert!(presence.editing("revenue").is_none());

        // Each command keeps it going; quiet ends it, and it lingers a
        // moment before going.
        presence.apply(
            &update(Activity::Replying, &["revenue"]),
            now + Duration::from_secs(20),
        );
        presence.tick(now + Duration::from_secs(40), 0);
        assert!(presence.is_working());
        presence.tick(now + Duration::from_secs(50), 0);
        assert!(!presence.is_working());
        assert_eq!(presence.sessions().len(), 1);
        presence.tick(now + Duration::from_secs(61), 0);
        assert!(presence.sessions().is_empty());

        // Saying idle ends it at once.
        presence.apply(&update(Activity::Reading, &[]), now);
        presence.apply(&update(Activity::Idle, &[]), now);
        assert!(!presence.is_working());
    }

    #[test]
    fn a_checks_own_end_does_not_end_the_session() {
        let now = Instant::now();
        let mut presence = Presence::default();
        presence.apply(&update(Activity::Checking, &[]), now);
        presence.apply(&update(Activity::Checking, &["revenue"]), now);
        presence.apply(&update(Activity::Idle, &[]), now);
        assert!(presence.is_working(), "the check ended, not the work");
        assert_eq!(presence.sessions()[0].log.len(), 1);
        presence.apply(&update(Activity::Idle, &[]), now);
        assert!(!presence.is_working());
    }

    #[test]
    fn a_finished_agent_stays_up_while_a_thread_waits_on_the_person() {
        let now = Instant::now();
        let mut presence = Presence::default();
        presence.apply(&update(Activity::Replying, &["busiest"]), now);
        presence.apply(&update(Activity::Idle, &[]), now);
        presence.tick(now + Duration::from_secs(60), 1);
        assert_eq!(presence.sessions().len(), 1);
        presence.tick(now + Duration::from_secs(61), 0);
        assert!(presence.sessions().is_empty());
    }

    #[test]
    fn an_edit_marks_what_it_changed_until_seen() {
        let now = Instant::now();
        let mut presence = Presence::default();
        presence.edited(&names(&["revenue"]), &[], &[], now);
        assert!(
            presence.is_empty(),
            "no agent, no credit: the person edited it"
        );

        presence.apply(&update(Activity::Editing, &["revenue", "share"]), now);
        assert_eq!(presence.editing("revenue"), Some("Claude"));
        assert_eq!(
            presence.adding(&names(&["revenue", "orders"])),
            vec![("share".to_string(), "Claude".to_string())]
        );

        presence.edited(
            &names(&["revenue"]),
            &names(&["share"]),
            &[("orders".into(), "Orders per day".into())],
            now + Duration::from_secs(90),
        );
        assert!(presence.editing("revenue").is_none(), "the edit landed");
        assert_eq!(presence.change("revenue").unwrap().change, Change::Changed);
        assert_eq!(presence.change("share").unwrap().change, Change::Added);
        assert_eq!(presence.removed()[0].title, "Orders per day");
        assert!(matches!(
            presence.sessions()[0].log.last().unwrap().did,
            Did::Edited {
                changed: 1,
                added: 1,
                removed: 1
            }
        ));

        // In view for long enough, a mark goes; quiet does not take it.
        presence.tick(now + Duration::from_secs(600), 0);
        assert!(presence.change("revenue").is_some());
        assert!(!presence.viewed(&names(&["revenue"]), Duration::from_millis(1000)));
        assert!(presence.viewed(&names(&["revenue"]), Duration::from_millis(500)));
        assert!(presence.change("revenue").is_none());
        assert!(presence.change("share").is_some());
        presence.dismiss_removed();
        assert!(presence.removed().is_empty());
    }

    #[test]
    fn an_edit_goes_to_the_agent_heard_from_last() {
        let now = Instant::now();
        let mut presence = Presence::default();
        presence.apply(&from("Sonnet", Activity::Reading, &[]), now);
        presence.apply(
            &from("Haiku", Activity::Reading, &[]),
            now + Duration::from_secs(5),
        );
        presence.edited(&names(&["revenue"]), &[], &[], now + Duration::from_secs(6));
        assert_eq!(presence.change("revenue").unwrap().agent, "Haiku");

        // Long quiet, no credit.
        let mut later = Presence::default();
        later.apply(&update(Activity::Reading, &[]), now);
        later.edited(
            &names(&["revenue"]),
            &[],
            &[],
            now + Duration::from_secs(600),
        );
        assert!(later.change("revenue").is_none());
    }

    #[test]
    fn reads_in_a_row_are_one_line_of_the_log() {
        let now = Instant::now();
        let mut presence = Presence::default();
        presence.apply(&update(Activity::Reading, &["a"]), now);
        presence.apply(&update(Activity::Reading, &["b", "a"]), now);
        assert_eq!(
            presence.sessions()[0].log[0].did,
            Did::Read(names(&["a", "b"]))
        );
    }
}
