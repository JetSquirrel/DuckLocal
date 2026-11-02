//! Proposed changes to a dashboard: an agent suggests a whole new spec, and
//! the person looks it over and applies or rejects it in the window.
//!
//! A proposal lives beside the spec, in `NAME.dash.proposal.json`, as its
//! comments do, so an agent needs no window to make one and the window finds
//! it by watching the file. It holds the proposed text and a hash of the spec
//! it was based on, and the spec is not touched until someone applies it.
//!
//! Applying is the window's alone: this module's `apply` has no command. That
//! does not stop an agent with a shell writing the file itself, but a request
//! for review means the person decides. The window runs the checks itself
//! rather than trusting anything the proposal says about them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::cli::{parse_args, Arg, CliError, FlagSpec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Applied,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub status: Status,
    /// The hash of the spec this was based on.
    pub base_hash: String,
    /// The whole new spec.
    pub proposed: String,
    /// Which agent proposed it, as `presence` names it.
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When it was proposed, RFC 3339 in local time.
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    /// Where the spec as it was went, once applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
    /// The person applied this although its checks failed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub applied_despite_failed_checks: bool,
}

/// Where a spec's proposal lives: `a.dash` → `a.dash.proposal.json`.
pub fn path_for(spec: &Path) -> PathBuf {
    with_suffix(spec, ".proposal.json")
}

/// Where applying keeps the spec as it was: `a.dash` → `a.dash.bak`.
pub fn backup_path_for(spec: &Path) -> PathBuf {
    with_suffix(spec, ".bak")
}

fn with_suffix(spec: &Path, suffix: &str) -> PathBuf {
    let mut name = spec.file_name().map(OsString::from).unwrap_or_default();
    name.push(suffix);
    spec.with_file_name(name)
}

pub fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Written whole and renamed into place, so nothing ever reads half a file and
/// a failed write leaves the old one as it was.
pub(crate) fn write_whole(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut name = path.file_name().map(OsString::from).unwrap_or_default();
    name.push(".writing");
    let temporary = path.with_file_name(name);
    std::fs::write(&temporary, bytes)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|e| {
            // Not when it is a folder in the way: that is not ours to remove.
            if temporary.is_file() {
                let _ = std::fs::remove_file(&temporary);
            }
            format!("{}: {e}", path.display())
        })
}

/// The spec's proposal, if it has one. No file is no proposal, not an error;
/// a file that does not read is, and says so rather than hiding one.
pub fn load(spec: &Path) -> Result<Option<Proposal>, String> {
    let path = path_for(spec);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn save(spec: &Path, proposal: &Proposal) -> Result<(), String> {
    let text = serde_json::to_string_pretty(proposal).map_err(|e| e.to_string())?;
    write_whole(&path_for(spec), (text + "\n").as_bytes())
}

impl Proposal {
    /// The spec has moved on since this was proposed.
    pub fn is_stale(&self, spec: &Path) -> bool {
        std::fs::read(spec).map_or(true, |bytes| hash(&bytes) != self.base_hash)
    }

    pub fn is_pending(&self) -> bool {
        self.status == Status::Pending
    }
}

/// Stage `proposed` as the change to `spec`, in place of any earlier proposal.
/// `based_on`, when given, is the hash of the spec the agent worked from.
fn propose(
    spec: &Path,
    proposed: String,
    note: Option<String>,
    based_on: Option<&str>,
) -> Result<Proposal, CliError> {
    let current = std::fs::read(spec).map_err(|e| {
        CliError::failure("io", format!("{}: {e}", spec.display()))
            .with_hint("A proposal changes an existing spec; write a new one directly")
    })?;
    let base_hash = hash(&current);
    if let Some(based_on) = based_on.filter(|based_on| *based_on != base_hash) {
        return Err(CliError::failure(
            "stale",
            format!("The spec changed since {based_on}; it is now {base_hash}"),
        )
        .with_hint("Read the spec again and redo the change on what it says now"));
    }
    if proposed.as_bytes() == current {
        return Err(CliError::argument("The proposal is the same as the spec"));
    }
    let mut id = [0u8; 4];
    getrandom::fill(&mut id)
        .map_err(|e| CliError::failure("internal", format!("no randomness for an id: {e}")))?;
    let proposal = Proposal {
        id: format!(
            "p{}",
            id.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ),
        status: Status::Pending,
        base_hash,
        proposed,
        author: super::presence::agent_name(),
        note,
        at: now(),
        decided_at: None,
        backup: None,
        applied_despite_failed_checks: false,
    };
    save(spec, &proposal).map_err(|e| CliError::failure("io", e))?;
    Ok(proposal)
}

/// What the checks say about a proposal's text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Why the spec fails, when it does: a parse error, a query that does not
    /// run, a column that is not there.
    pub failure: Option<String>,
    /// Queries that read a table no source defines, so went unchecked.
    pub unresolved: Vec<String>,
    /// Queries that ran and returned nothing.
    pub empty: Vec<String>,
    /// Queries the dashboard's row budget cut off.
    pub truncated: Vec<String>,
}

impl Verdict {
    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }

    fn to_json(&self) -> Value {
        json!({
            "ok": self.passed(),
            "failure": self.failure,
            "unresolved": self.unresolved,
            "empty": self.empty,
            "truncated": self.truncated,
        })
    }
}

/// Run `ducklocal check`'s checks on `text` as if it were the spec at `spec`,
/// so its relative paths mean what they will. The SQL has to pass the same
/// read-only gate a dashboard's does before anything runs.
pub fn check(spec: &Path, text: &str) -> Verdict {
    let names = |report: &Value, field: &str| -> Vec<String> {
        let items = report[field].as_array().into_iter().flatten();
        items
            .filter_map(|item| {
                item.get("query")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    };
    match super::check_source(spec, text, None, false, false) {
        Err(error) => Verdict {
            failure: Some(error.message),
            ..Verdict::default()
        },
        Ok(report) => {
            let report: Value = serde_json::from_str(&report).unwrap_or_default();
            let queries = report["queries"].as_array().into_iter().flatten();
            Verdict {
                failure: None,
                unresolved: names(&report, "unresolved"),
                empty: names(&report, "empty"),
                truncated: queries
                    .filter(|query| query["truncated"] == json!(true))
                    .filter_map(|query| query["name"].as_str().map(str::to_string))
                    .collect(),
            }
        }
    }
}

/// Why a proposal was not applied. In each case the spec is as it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyError {
    /// It is no longer the pending proposal the person was looking at.
    Replaced,
    /// The spec changed since it was proposed.
    Stale,
    /// Its checks failed, and nobody said to go ahead regardless.
    ChecksFailed,
    /// A file could not be read or written.
    Io(String),
}

/// Apply proposal `id`: one deliberate change, the spec as it was kept beside
/// it. Refuses whatever the person was not shown.
pub fn apply(
    spec: &Path,
    id: &str,
    verdict: &Verdict,
    despite_failed_checks: bool,
) -> Result<Proposal, ApplyError> {
    let mut proposal = load(spec)
        .map_err(ApplyError::Io)?
        .filter(|proposal| proposal.id == id && proposal.is_pending())
        .ok_or(ApplyError::Replaced)?;
    let current = std::fs::read(spec).map_err(|e| ApplyError::Io(e.to_string()))?;
    if hash(&current) != proposal.base_hash {
        return Err(ApplyError::Stale);
    }
    if !verdict.passed() && !despite_failed_checks {
        return Err(ApplyError::ChecksFailed);
    }
    let backup = backup_path_for(spec);
    write_whole(&backup, &current).map_err(ApplyError::Io)?;
    write_whole(spec, proposal.proposed.as_bytes()).map_err(ApplyError::Io)?;
    proposal.status = Status::Applied;
    proposal.decided_at = Some(now());
    proposal.backup = Some(backup);
    proposal.applied_despite_failed_checks = !verdict.passed();
    save(spec, &proposal).map_err(|e| {
        ApplyError::Io(format!(
            "The change was applied, but its status was not recorded: {e}"
        ))
    })?;
    Ok(proposal)
}

/// Reject proposal `id`. Only the proposal changes; the spec is not opened.
pub fn reject(spec: &Path, id: &str) -> Result<Proposal, ApplyError> {
    let mut proposal = load(spec)
        .map_err(ApplyError::Io)?
        .filter(|proposal| proposal.id == id && proposal.is_pending())
        .ok_or(ApplyError::Replaced)?;
    proposal.status = Status::Rejected;
    proposal.decided_at = Some(now());
    save(spec, &proposal).map_err(ApplyError::Io)?;
    Ok(proposal)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Same,
    Removed,
    Added,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub change: Change,
    pub text: String,
}

/// Past this many cells, the changed middle shows as all removed, then all
/// added: still true, and a person is not reading it line by line anyway.
const MAX_DIFF_CELLS: usize = 4_000_000;

/// `old` to `new`, line by line: a longest common subsequence over what lies
/// between the lines they start and end with in common.
pub fn diff(old: &str, new: &str) -> Vec<DiffLine> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let line = |change, text: &str| DiffLine {
        change,
        text: text.to_string(),
    };
    let shared = |a: &[&str], b: &[&str]| a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let start = shared(&old, &new);
    let (old_rest, new_rest) = (&old[start..], &new[start..]);
    let end = old_rest
        .iter()
        .rev()
        .zip(new_rest.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a, b) = (
        &old_rest[..old_rest.len() - end],
        &new_rest[..new_rest.len() - end],
    );

    let mut out: Vec<DiffLine> = old[..start].iter().map(|l| line(Change::Same, l)).collect();
    if a.len().saturating_mul(b.len()) > MAX_DIFF_CELLS {
        out.extend(a.iter().map(|l| line(Change::Removed, l)));
        out.extend(b.iter().map(|l| line(Change::Added, l)));
    } else {
        // lengths[i][j]: the longest common run of a[i..] and b[j..].
        let width = b.len() + 1;
        let mut lengths = vec![0u32; (a.len() + 1) * width];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                lengths[i * width + j] = if a[i] == b[j] {
                    lengths[(i + 1) * width + j + 1] + 1
                } else {
                    lengths[(i + 1) * width + j].max(lengths[i * width + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                out.push(line(Change::Same, a[i]));
                i += 1;
                j += 1;
            } else if i < a.len()
                && (j == b.len() || lengths[(i + 1) * width + j] >= lengths[i * width + j + 1])
            {
                // At a tie the removal goes first: a replaced line reads old,
                // then new.
                out.push(line(Change::Removed, a[i]));
                i += 1;
            } else {
                out.push(line(Change::Added, b[j]));
                j += 1;
            }
        }
    }
    out.extend(old[old.len() - end..].iter().map(|l| line(Change::Same, l)));
    out
}

pub(crate) const PROPOSE_HELP: &str = "\
Usage: ducklocal propose FILE --from NEW [--note TEXT] [--based-on HASH]
       ducklocal propose FILE --status

Suggest a change to a .dash dashboard for the person to review, without
touching it. NEW is the whole new spec; - reads stdin. The proposal waits beside
the spec in FILE.proposal.json, replacing any earlier one, and the window offers
it for review: the changes, the checks, and Apply or Reject. Only the person
can apply it, in the window.

Options:
  --from NEW         The proposed spec, a UTF-8 file; - reads stdin
  --note TEXT        What the change is for, shown to the person
  --based-on HASH    Refuse (kind stale) unless the spec still has this hash,
                     which --status reports as current_hash
  --status           Report on the current proposal; changes nothing

Output: {id, status, stale, base_hash, current_hash, note, author, at,
decided_at, backup, applied_despite_failed_checks, check}. status is pending,
applied, rejected, or none when there is no proposal. stale means the spec has
changed since, so applying it would be refused: propose again from what it says
now. A proposal is staged even when check.ok is false, and the person sees why;
fix it and propose again if you can. check is only there after proposing.
Exit 2 for arguments, 1 for I/O or a stale --based-on.
";

const SPEC: &[FlagSpec] = &[
    FlagSpec {
        name: "--from",
        takes_value: true,
    },
    FlagSpec {
        name: "--note",
        takes_value: true,
    },
    FlagSpec {
        name: "--based-on",
        takes_value: true,
    },
    FlagSpec {
        name: "--status",
        takes_value: false,
    },
];

fn report(spec: &Path, proposal: Option<&Proposal>) -> Value {
    let current_hash = std::fs::read(spec).ok().map(|bytes| hash(&bytes));
    let Some(proposal) = proposal else {
        return json!({"status": "none", "current_hash": current_hash});
    };
    json!({
        "id": proposal.id,
        "status": proposal.status,
        "stale": proposal.is_pending() && proposal.is_stale(spec),
        "base_hash": proposal.base_hash,
        "current_hash": current_hash,
        "note": proposal.note,
        "author": proposal.author,
        "at": proposal.at,
        "decided_at": proposal.decided_at,
        "backup": proposal.backup,
        "applied_despite_failed_checks": proposal.applied_despite_failed_checks,
    })
}

/// `ducklocal propose`.
pub fn run(args: &[OsString]) -> Result<String, CliError> {
    let mut file = None;
    let mut from: Option<PathBuf> = None;
    let mut note = None;
    let mut based_on = None;
    let mut status = false;
    let text = |value: OsString, what: &str| {
        value
            .into_string()
            .map_err(|_| CliError::argument(format!("{what} must be UTF-8")))
    };
    for arg in parse_args("propose", args, SPEC, &["--note"])? {
        match arg {
            Arg::Flag("--from", Some(value)) => from = Some(value.into()),
            Arg::Flag("--note", Some(value)) => note = Some(text(value, "--note")?),
            Arg::Flag("--based-on", Some(value)) => based_on = Some(text(value, "--based-on")?),
            Arg::Flag("--status", None) => status = true,
            Arg::Positional(value) if file.is_none() => file = Some(PathBuf::from(value)),
            Arg::Positional(_) => {
                return Err(CliError::argument(
                    "Name one spec: a proposal changes one file",
                ))
            }
            _ => {
                return Err(CliError::failure(
                    "internal",
                    "propose accepted a flag it does not handle",
                ))
            }
        }
    }
    let file = file.ok_or_else(|| CliError::argument("Name a .dash file"))?;
    if status {
        if from.is_some() || note.is_some() || based_on.is_some() {
            return Err(CliError::argument(
                "--status reads; it takes no other option",
            ));
        }
        let proposal = load(&file).map_err(|e| CliError::failure("io", e))?;
        return Ok(report(&file, proposal.as_ref()).to_string());
    }
    let from = from.ok_or_else(|| CliError::argument("Give the new spec with --from NEW"))?;
    let proposed = if from.as_os_str() == "-" {
        let mut proposed = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut proposed)
            .map_err(|e| CliError::failure("io", e))?;
        proposed
    } else {
        std::fs::read_to_string(&from)
            .map_err(|e| CliError::failure("io", format!("{}: {e}", from.display())))?
    };
    let proposal = propose(&file, proposed, note, based_on.as_deref())?;
    let mut output = report(&file, Some(&proposal));
    output["check"] = check(&file, &proposal.proposed).to_json();
    Ok(output.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "query \"q\" {\n  sql = <<SQL\nSELECT 1 AS n\nSQL\n}\n";
    const BETTER: &str = "query \"q\" {\n  sql = <<SQL\nSELECT 2 AS n\nSQL\n}\n";

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ducklocal_proposal_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = dir.join("a.dash");
        std::fs::write(&spec, GOOD).unwrap();
        spec
    }

    fn staged(spec: &Path) -> Proposal {
        propose(spec, BETTER.to_string(), Some("two".into()), None).unwrap()
    }

    #[test]
    fn a_proposal_waits_beside_the_spec_and_touches_nothing() {
        let spec = scratch("wait");
        assert_eq!(load(&spec).unwrap(), None);
        let proposal = staged(&spec);
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), GOOD);
        assert_eq!(load(&spec).unwrap().as_ref(), Some(&proposal));
        assert!(proposal.is_pending());
        assert!(!proposal.is_stale(&spec));
        assert_eq!(proposal.note.as_deref(), Some("two"));
    }

    #[test]
    fn a_change_of_nothing_is_no_proposal() {
        let spec = scratch("same");
        let error = propose(&spec, GOOD.to_string(), None, None).unwrap_err();
        assert_eq!(error.kind, "argument");
        assert_eq!(load(&spec).unwrap(), None);
    }

    #[test]
    fn a_proposal_based_on_an_older_spec_is_refused() {
        let spec = scratch("based");
        let error = propose(&spec, BETTER.to_string(), None, Some("old")).unwrap_err();
        assert_eq!(error.kind, "stale");
        assert_eq!(load(&spec).unwrap(), None);
        let current = hash(GOOD.as_bytes());
        propose(&spec, BETTER.to_string(), None, Some(&current)).unwrap();
    }

    #[test]
    fn applying_changes_the_spec_once_and_keeps_what_it_was() {
        let spec = scratch("apply");
        let proposal = staged(&spec);
        let applied = apply(&spec, &proposal.id, &Verdict::default(), false).unwrap();
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), BETTER);
        assert_eq!(applied.status, Status::Applied);
        assert!(applied.decided_at.is_some());
        assert!(!applied.applied_despite_failed_checks);
        let backup = applied.backup.unwrap();
        assert_eq!(std::fs::read_to_string(backup).unwrap(), GOOD);
        // Decided is decided: it does not apply twice.
        assert_eq!(
            apply(&spec, &proposal.id, &Verdict::default(), false),
            Err(ApplyError::Replaced)
        );
    }

    #[test]
    fn rejecting_leaves_the_spec_byte_for_byte() {
        let spec = scratch("reject");
        // Line endings no tool here would write, to catch a rewrite.
        let bytes = b"query \"q\" {\r\n  sql = <<SQL\r\nSELECT 1\r\nSQL\r\n}";
        std::fs::write(&spec, bytes).unwrap();
        let proposal = staged(&spec);
        let rejected = reject(&spec, &proposal.id).unwrap();
        assert_eq!(rejected.status, Status::Rejected);
        assert_eq!(std::fs::read(&spec).unwrap(), bytes);
        assert!(!backup_path_for(&spec).exists());
    }

    #[test]
    fn an_edit_since_the_proposal_stops_it_applying() {
        let spec = scratch("concurrent");
        let proposal = staged(&spec);
        let edited = "query \"q\" {\n  sql = <<SQL\nSELECT 3 AS n\nSQL\n}\n";
        std::fs::write(&spec, edited).unwrap();
        assert!(proposal.is_stale(&spec));
        assert_eq!(
            apply(&spec, &proposal.id, &Verdict::default(), false),
            Err(ApplyError::Stale)
        );
        // Going ahead over failed checks is no way round it either.
        assert_eq!(
            apply(&spec, &proposal.id, &Verdict::default(), true),
            Err(ApplyError::Stale)
        );
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), edited);
        assert!(!backup_path_for(&spec).exists());
    }

    #[test]
    fn a_newer_proposal_replaces_the_one_on_screen() {
        let spec = scratch("replaced");
        let first = staged(&spec);
        let second = propose(&spec, format!("{BETTER}\n"), None, None).unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(
            apply(&spec, &first.id, &Verdict::default(), false),
            Err(ApplyError::Replaced)
        );
        assert_eq!(reject(&spec, &first.id), Err(ApplyError::Replaced));
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), GOOD);
    }

    #[test]
    fn failed_checks_need_saying_so_and_are_recorded() {
        let spec = scratch("failed");
        let proposal = staged(&spec);
        let failing = Verdict {
            failure: Some("no".into()),
            ..Verdict::default()
        };
        assert_eq!(
            apply(&spec, &proposal.id, &failing, false),
            Err(ApplyError::ChecksFailed)
        );
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), GOOD);
        let applied = apply(&spec, &proposal.id, &failing, true).unwrap();
        assert!(applied.applied_despite_failed_checks);
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), BETTER);
    }

    #[test]
    fn a_write_that_fails_leaves_the_spec_and_the_proposal_as_they_were() {
        // A folder where a file has to go fails on every platform, and for
        // whoever runs the tests, as a read-only file would not.
        let spec = scratch("no_backup");
        let proposal = staged(&spec);
        std::fs::create_dir(backup_path_for(&spec)).unwrap();
        let outcome = apply(&spec, &proposal.id, &Verdict::default(), false);
        assert!(matches!(outcome, Err(ApplyError::Io(_))), "{outcome:?}");
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), GOOD);
        assert!(load(&spec).unwrap().unwrap().is_pending());

        let spec = scratch("no_write");
        let proposal = staged(&spec);
        std::fs::create_dir(with_suffix(&spec, ".writing")).unwrap();
        let outcome = apply(&spec, &proposal.id, &Verdict::default(), false);
        assert!(matches!(outcome, Err(ApplyError::Io(_))), "{outcome:?}");
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), GOOD);
        assert!(load(&spec).unwrap().unwrap().is_pending());
    }

    #[test]
    fn a_proposal_file_that_does_not_read_says_so() {
        let spec = scratch("corrupt");
        std::fs::write(path_for(&spec), "{ not json").unwrap();
        assert!(load(&spec).unwrap_err().contains("proposal.json"));
    }

    #[test]
    fn the_checks_see_a_malformed_spec_and_a_sound_one() {
        let spec = scratch("check");
        let sound = "query \"q\" {\n  sql = <<SQL\nSELECT 1 AS n\nSQL\n}\n\
                     plot \"p\" {\n  type = \"bar\"\n  query = query.q\n  x = \"n\"\n  y = \"n\"\n}\n";
        assert_eq!(check(&spec, sound).failure, None);

        let malformed = check(&spec, "query \"q\" {");
        assert!(!malformed.passed());

        let writes = "query \"q\" {\n  sql = <<SQL\nCOPY (SELECT 1) TO 'x.csv'\nSQL\n}\n";
        assert!(!check(&spec, writes).passed());
        assert!(!spec.with_file_name("x.csv").exists());

        let missing = "query \"q\" {\n  sql = <<SQL\nSELECT * FROM absent\nSQL\n}\n";
        assert_eq!(check(&spec, missing).unresolved, vec!["q".to_string()]);

        let nothing = "query \"q\" {\n  sql = <<SQL\nSELECT 1 AS n WHERE false\nSQL\n}\n";
        assert_eq!(check(&spec, nothing).empty, vec!["q".to_string()]);
    }

    fn rendered(lines: &[DiffLine]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                let mark = match line.change {
                    Change::Same => ' ',
                    Change::Removed => '-',
                    Change::Added => '+',
                };
                format!("{mark}{}", line.text)
            })
            .collect()
    }

    #[test]
    fn a_diff_shows_what_changed_in_place() {
        assert_eq!(
            rendered(&diff("a\nb\nc\n", "a\nx\nc\n")),
            vec![" a", "-b", "+x", " c"]
        );
        assert_eq!(rendered(&diff("a\n", "a\nb\n")), vec![" a", "+b"]);
        assert_eq!(rendered(&diff("a\nb\n", "b\n")), vec!["-a", " b"]);
        assert_eq!(rendered(&diff("", "a\n")), vec!["+a"]);
        assert_eq!(rendered(&diff("a\n", "")), vec!["-a"]);
        assert_eq!(rendered(&diff("a\n", "a\n")), vec![" a"]);
        assert_eq!(
            rendered(&diff("a\nb\nc\nd\n", "b\nx\nd\ne\n")),
            vec!["-a", " b", "-c", "+x", " d", "+e"]
        );
    }

    #[test]
    fn a_diff_never_loses_a_line_whichever_way_it_goes() {
        let old: String = (0..40).map(|i| format!("{}\n", i % 7)).collect();
        let new: String = (0..40).map(|i| format!("{}\n", i % 5)).collect();
        let lines = diff(&old, &new);
        let keep = |skip: Change| -> String {
            lines
                .iter()
                .filter(|line| line.change != skip)
                .map(|line| format!("{}\n", line.text))
                .collect()
        };
        assert_eq!(keep(Change::Added), old);
        assert_eq!(keep(Change::Removed), new);
    }
}
