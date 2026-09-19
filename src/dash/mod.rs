//! `ducklocal dash export --html`: a panel's data as one standalone file.
//!
//! An analysis panel is a script that draws itself with native components, and
//! none of that travels: the description the runtime builds says *that* a chart
//! is there, not what its bars are. What does travel is what the panel asked
//! the database, so the export runs the panel once and writes down its
//! statements and their results — a file you can send to someone with no
//! DuckLocal, no database, and no JavaScript.
//!
//! ```text
//! src/dash/capture.rs  what the panel asked, recorded at the host module
//! src/dash/run.rs      the panel, run once in a hidden window
//! src/dash/report.rs   the captures, as HTML (pure, and tested as such)
//! ```

pub mod capture;
pub mod report;
pub mod run;

use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::json;

use crate::cli::{parse_args, Arg, CliError, FlagSpec, HELP};

struct Request {
    /// The path as given; resolved to a panel folder before anything runs.
    panel: PathBuf,
    out: PathBuf,
    database: Option<PathBuf>,
    read_write: bool,
    force: bool,
}

/// `ducklocal dash export --html [--out FILE] [--force] [--database PATH]
/// [--read-write] PANEL`.
///
/// Returns only for `--help` and for a mistake in the arguments: the export
/// itself finishes inside the application loop, where it writes the file,
/// prints its one JSON object, and exits with the status it earned.
pub fn dispatch(args: &[OsString]) -> Result<String, CliError> {
    let first = args.first().and_then(|arg| arg.to_str());
    match first {
        Some("--help") if args.len() == 1 => return Ok(HELP.to_string()),
        Some("export") => {}
        // An option where the subcommand goes is a command line to correct,
        // not a subcommand that does not exist.
        Some(other) if other.starts_with("--") => {
            return Err(CliError::argument(
                "Unknown or extra arguments; use ducklocal --help",
            ))
        }
        Some(other) => {
            return Err(CliError::argument(format!(
                "Unknown dash subcommand: {other}; use ducklocal --help"
            )))
        }
        None => {
            return Err(CliError::argument(
                "Name a subcommand: ducklocal dash export --html PANEL",
            ))
        }
    }
    if args.len() == 2 && args[1] == "--help" {
        return Ok(HELP.to_string());
    }
    export(&args[1..])?;
    Err(CliError::failure(
        "internal",
        "the export returned instead of exiting from the application loop",
    ))
}

fn export(args: &[OsString]) -> Result<(), CliError> {
    let request = parse(args)?;
    let directory = gpui_shell::resolve_app_root(&request.panel, crate::analysis::host::ENTRY)
        .map_err(|error| CliError::argument(error.to_string()))?;
    if request.out.exists() && !request.force {
        return Err(CliError::argument(format!(
            "{} already exists; name another --out or pass --force",
            request.out.display()
        )));
    }
    // The same connection rules as `query`: an existing file read-only unless
    // asked otherwise, no extension installation, no falling back to memory.
    let connection = crate::cli::open(request.database.clone(), request.read_write)?;
    let database = match &request.database {
        Some(path) => path.display().to_string(),
        None => "in-memory".to_string(),
    };

    let panel = directory.clone();
    run::capture(
        run::Job {
            panel: directory,
            connection,
        },
        move |outcome| finish(&request, &panel, &database, outcome),
    )
}

const SPEC: &[FlagSpec] = &[
    FlagSpec {
        name: "--html",
        takes_value: false,
    },
    FlagSpec {
        name: "--force",
        takes_value: false,
    },
    FlagSpec {
        name: "--read-write",
        takes_value: false,
    },
    FlagSpec {
        name: "--out",
        takes_value: true,
    },
    FlagSpec {
        name: "--database",
        takes_value: true,
    },
];

fn parse(args: &[OsString]) -> Result<Request, CliError> {
    let mut html = false;
    let mut out = None;
    let mut database = None;
    let mut read_write = false;
    let mut force = false;
    let mut panel = None;
    for arg in parse_args("dash", args, SPEC, &[])? {
        match arg {
            Arg::Flag("--html", None) => html = true,
            Arg::Flag("--force", None) => force = true,
            Arg::Flag("--read-write", None) => read_write = true,
            Arg::Flag("--out", Some(value)) => out = Some(PathBuf::from(value)),
            Arg::Flag("--database", Some(value)) => {
                database = Some(crate::cli::database_path(&value)?);
            }
            Arg::Positional(value) => {
                if panel.is_some() {
                    return Err(CliError::argument(
                        "Name one panel: the export writes one file for one panel",
                    ));
                }
                panel = Some(PathBuf::from(value));
            }
            _ => {
                return Err(CliError::failure(
                    "internal",
                    "the argument walker produced a flag dash does not declare",
                ));
            }
        }
    }
    if !html {
        return Err(CliError::argument(
            "dash export needs --html; it is the only format there is",
        ));
    }
    let panel = panel.ok_or_else(|| {
        CliError::argument("Name a panel folder (containing main.js), or its main.js")
    })?;
    if read_write && database.is_none() {
        return Err(CliError::argument("--read-write requires --database"));
    }
    // Named after the panel so that the common case needs no `--out`, and
    // beside the working directory rather than inside the panel folder: the
    // folder is the panel, and a report in it is one more thing to commit.
    let out = out.unwrap_or_else(|| {
        let name = panel
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "panel".to_string());
        let name = name.strip_suffix(".js").unwrap_or(&name).to_string();
        PathBuf::from(format!("{name}.html"))
    });
    Ok(Request {
        panel,
        out,
        database,
        read_write,
        force,
    })
}

/// Write the report, say what happened, and end the process.
///
/// The exit happens here rather than in a caller because everything that knows
/// the answer runs inside the application loop, which does not unwind.
fn finish(request: &Request, panel: &std::path::Path, database: &str, outcome: run::Outcome) -> ! {
    let (captures, panel_error, reported, elapsed_ms) = match outcome {
        run::Outcome::Captured {
            captures,
            panel_error,
            reported,
            elapsed_ms,
        } => (captures, panel_error, reported, elapsed_ms),
        run::Outcome::Failed(message) => {
            fail(&CliError::failure("panel", message));
        }
    };
    let queries = captures
        .iter()
        .filter(|capture| capture.sql().is_some())
        .count();
    let rows: usize = captures
        .iter()
        .filter_map(|capture| capture.result())
        .filter_map(|result| result.as_ref().ok())
        .map(|result| result.row_count)
        .sum();
    let exported_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let html = report::build(&report::Report {
        panel,
        entry: crate::analysis::host::ENTRY,
        database,
        exported_at: &exported_at,
        version: env!("CARGO_PKG_VERSION"),
        captures: &captures,
        panel_error: panel_error.as_deref(),
        reported: &reported,
    });
    if let Err(error) = std::fs::write(&request.out, html) {
        fail(&CliError::failure("io", error));
    }
    let written = std::fs::canonicalize(&request.out).unwrap_or_else(|_| request.out.clone());
    let report = json!({
        "html": written.display().to_string(),
        "panel": panel.display().to_string(),
        "queries": queries,
        "rows": rows,
        "panel_errors": reported.len(),
        "captured_ms": elapsed_ms as u64,
    });
    let mut stdout = std::io::stdout();
    use std::io::Write as _;
    let _ = writeln!(stdout, "{report}");
    let _ = stdout.flush();

    // A panel that threw still produced the only evidence of what it managed;
    // the file is written, and the exit status says the panel did not finish.
    if let Some(error) = panel_error {
        eprintln!(
            "{}",
            json!({"error": {
                "kind": "panel",
                "message": format!("{error} (the report was written to {})", written.display()),
            }})
        );
        std::process::exit(1);
    }
    std::process::exit(0);
}

fn fail(error: &CliError) -> ! {
    eprintln!(
        "{}",
        json!({"error": {"kind": error.kind(), "message": error.message()}})
    );
    std::process::exit(error.code())
}
