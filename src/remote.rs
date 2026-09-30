//! `ducklocal open`: hand the window that is already running something to
//! show — SQL in a new query tab, a `.dash` spec, an app folder, data files.
//!
//! The window listens on a loopback port and writes the port, with a random
//! token, to an endpoint file in the app data directory that only this user
//! can read. The command reads that file, connects, and sends one JSON line;
//! the window queues the request for the root view to open (app.rs), the way
//! Finder's opens are queued. Loopback TCP rather than a Unix socket so the
//! three platforms share one path; the token is what keeps another user on
//! the machine, or a web page aiming requests at localhost, out.
//!
//! What opens runs in the window's session — its connection, its attached
//! data, its history — which is the point: an agent's work lands where a
//! person can see it and carry on.
//!
//! `ducklocal open --state` is the other direction on the same channel: what the
//! window shows now — its tabs, each query's SQL and result, each
//! dashboard's picks — so an agent can pick up where the person left off.
//! The listener thread cannot read the UI, so it parks the question with a
//! channel to answer on, and the window's poll (app.rs) answers it from the
//! UI thread.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cli::{parse_args, Arg, CliError, FlagSpec};

pub(crate) const OPEN_HELP: &str = "\
Usage: ducklocal open [OPTIONS] [PATH ...]

Show something in the DuckLocal window that is already running, starting one
if none is: SQL in a new query tab, and PATHs opened the way a drop on the
window opens them — a .dash spec or an app folder as a tab, data files,
folders and patterns attached.

Options:
  --sql SQL          SQL for a new query tab
  --sql-file FILE    UTF-8 SQL file; - reads stdin
  --title TITLE      The new query tab's title
  --run              Run the SQL once the tab is open and PATHs are attached
  --no-launch        Fail with kind not_running instead of starting a window
  --state            Open nothing: print what the window shows (see below)

Output: {delivered, launched}. The window opens the request in its own
session — its connection, attached data and history, not this command's —
and reports its own errors there. Exit 2 for arguments, 1 when no window can
be reached.

With --state, nothing opens and no window starts: the output is what the
running window shows — {database, attached, active_tab, tabs}: each query
tab's SQL and result (columns, row count, the first 20 rows, or the error),
each dashboard's plots and the values its filters have picked, each app's
folder. Exit 1 with kind not_running when no window is up, timeout when it
does not answer within 5s. It takes no other option or PATH.
";

const OPEN_SPEC: &[FlagSpec] = &[
    FlagSpec {
        name: "--sql",
        takes_value: true,
    },
    FlagSpec {
        name: "--sql-file",
        takes_value: true,
    },
    FlagSpec {
        name: "--title",
        takes_value: true,
    },
    FlagSpec {
        name: "--run",
        takes_value: false,
    },
    FlagSpec {
        name: "--no-launch",
        takes_value: false,
    },
    FlagSpec {
        name: "--state",
        takes_value: false,
    },
];

/// The endpoint file, next to the history store.
const ENDPOINT_FILE: &str = "gui-endpoint.json";

/// The first rows a query tab's state carries: enough to see what the person
/// is looking at, not a copy of the result.
pub(crate) const STATE_PREVIEW_ROWS: usize = 20;

/// How long the listener waits for the window to answer a state request.
const STATE_TIMEOUT: Duration = Duration::from_secs(5);

/// Largest request line the window reads: SQL files are small, and a line
/// with no end should not grow without bound.
const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;

/// How long a started window gets to begin listening.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a running window can be reached.
#[derive(Debug, Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
    pid: u32,
}

/// What the window is asked to open. Paths are absolute: the window's
/// working directory is not the command's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Request {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub sql: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub run: bool,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    token: String,
    /// Something to open; absent when the envelope asks for the state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request: Option<Request>,
    /// Ask what the window shows instead of opening anything.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    state: bool,
}

/// What an accepted envelope asks for.
enum Incoming {
    Open(Request),
    State,
}

/// Requests the listener accepted, for the root view to drain.
static PENDING: Mutex<Vec<Request>> = Mutex::new(Vec::new());

/// Take every request accepted since the last call.
pub(crate) fn take_pending() -> Vec<Request> {
    std::mem::take(&mut *PENDING.lock().unwrap_or_else(PoisonError::into_inner))
}

/// State requests waiting for the window, each with where to send it.
static STATE_WAITERS: Mutex<Vec<mpsc::Sender<Value>>> = Mutex::new(Vec::new());

/// Whether a `ducklocal open --state` is waiting for the window to answer.
pub(crate) fn state_wanted() -> bool {
    !STATE_WAITERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_empty()
}

/// Answer every waiting state request with `state`.
pub(crate) fn answer_state(state: Value) {
    let waiters =
        std::mem::take(&mut *STATE_WAITERS.lock().unwrap_or_else(PoisonError::into_inner));
    for waiter in waiters {
        // A caller that gave up has dropped its receiver; nothing to do.
        let _ = waiter.send(state.clone());
    }
}

/// Park a state request for the window's poll and wait for its answer.
fn ask_window_for_state() -> Result<Value, String> {
    let (sender, receiver) = mpsc::channel();
    STATE_WAITERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(sender);
    receiver
        .recv_timeout(STATE_TIMEOUT)
        .map_err(|_| "the window did not answer within 5s".to_string())
}

fn data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "DuckLocal").map(|dirs| dirs.data_dir().to_path_buf())
}

/// Start listening for `ducklocal open`, on a thread of its own. A window
/// that cannot listen still works; the command then says none is running.
pub(crate) fn serve() {
    let Some(dir) = data_dir() else {
        tracing::warn!("no app data directory; `ducklocal open` cannot reach this window");
        return;
    };
    match listen(&dir) {
        Ok(listener) => {
            std::thread::Builder::new()
                .name("ducklocal-open".into())
                .spawn(move || {
                    accept_loop(
                        listener,
                        |request| {
                            PENDING
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push(request);
                        },
                        ask_window_for_state,
                    )
                })
                .ok();
        }
        Err(error) => tracing::warn!("`ducklocal open` listener not started: {error:#}"),
    }
}

/// Bind a loopback port and publish it, with a fresh token, in `dir`.
fn listen(dir: &Path) -> anyhow::Result<(TcpListener, String)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness for a token: {e}"))?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let endpoint = Endpoint {
        port: listener.local_addr()?.port(),
        token: token.clone(),
        pid: std::process::id(),
    };
    std::fs::create_dir_all(dir)?;
    // Written whole and renamed into place, so a command never reads half an
    // endpoint; created owner-only, so the token is this user's alone.
    let temporary = dir.join(format!("{ENDPOINT_FILE}.{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&temporary)?;
    file.write_all(serde_json::to_string(&endpoint)?.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, dir.join(ENDPOINT_FILE))?;
    Ok((listener, token))
}

fn accept_loop(
    (listener, token): (TcpListener, String),
    accept: impl Fn(Request),
    state: impl Fn() -> Result<Value, String>,
) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // One connection at a time is plenty for a command a person or an
        // agent runs; the timeouts keep a stalled one from holding the rest.
        let reply = match receive(&stream, &token) {
            Ok(Incoming::Open(request)) => {
                accept(request);
                json!({"ok": true})
            }
            Ok(Incoming::State) => match state() {
                Ok(state) => json!({"ok": true, "state": state}),
                Err(message) => json!({"error": message, "kind": "timeout"}),
            },
            Err(message) => json!({"error": message}),
        };
        let mut stream = stream;
        let _ = writeln!(stream, "{reply}").and_then(|_| stream.flush());
    }
}

fn receive(stream: &TcpStream, token: &str) -> Result<Incoming, String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    BufReader::new(stream.take(MAX_REQUEST_BYTES))
        .read_line(&mut line)
        .map_err(|e| format!("reading the request: {e}"))?;
    let envelope: Envelope =
        serde_json::from_str(&line).map_err(|e| format!("malformed request: {e}"))?;
    if envelope.token != token {
        return Err("wrong token".into());
    }
    match (envelope.state, envelope.request) {
        (true, None) => Ok(Incoming::State),
        (false, Some(request)) => Ok(Incoming::Open(request)),
        _ => Err("malformed request: ask for the state or send something to open".into()),
    }
}

/// Why a request did not reach a window.
#[derive(Debug)]
enum Unreachable {
    /// No endpoint, or nothing listening at it: no window is running.
    NotRunning,
    /// Something answered and refused, or spoke another protocol.
    Refused(String),
    /// The window took the request but did not answer in time.
    Timeout(String),
}

/// Send `request` to the window whose endpoint is in `dir`.
fn deliver(dir: &Path, request: &Request) -> Result<(), Unreachable> {
    exchange(dir, Some(request.clone()), false).map(|_| ())
}

/// Ask the window whose endpoint is in `dir` what it shows.
fn fetch_state(dir: &Path) -> Result<Value, Unreachable> {
    let mut reply = exchange(dir, None, true)?;
    reply
        .get_mut("state")
        .map(Value::take)
        .ok_or_else(|| Unreachable::Refused("the window answered without its state".into()))
}

/// One envelope out, one reply line back.
fn exchange(dir: &Path, request: Option<Request>, state: bool) -> Result<Value, Unreachable> {
    let text =
        std::fs::read_to_string(dir.join(ENDPOINT_FILE)).map_err(|_| Unreachable::NotRunning)?;
    let endpoint: Endpoint = serde_json::from_str(&text).map_err(|_| Unreachable::NotRunning)?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    // A window that quit leaves its endpoint behind; a refused connection is
    // how that reads, and it means the same as no file at all.
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .map_err(|_| Unreachable::NotRunning)?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let envelope = Envelope {
        token: endpoint.token,
        request,
        state,
    };
    let line = serde_json::to_string(&envelope).map_err(|e| Unreachable::Refused(e.to_string()))?;
    writeln!(stream, "{line}")
        .and_then(|_| stream.flush())
        .map_err(|e| Unreachable::Refused(e.to_string()))?;
    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|e| Unreachable::Refused(e.to_string()))?;
    let reply: serde_json::Value = serde_json::from_str(&reply).map_err(|_| {
        Unreachable::Refused("the port answered with something other than DuckLocal".into())
    })?;
    match reply.get("error").and_then(|e| e.as_str()) {
        Some(message) if reply.get("kind") == Some(&json!("timeout")) => {
            Err(Unreachable::Timeout(message.to_string()))
        }
        Some(message) => Err(Unreachable::Refused(message.to_string())),
        None if reply.get("ok") == Some(&json!(true)) => Ok(reply),
        None => Err(Unreachable::Refused("unexpected reply".into())),
    }
}

/// `ducklocal open --state`: what the running window shows. Never starts
/// one — a fresh window has nothing to report.
fn read_state(dir: &Path) -> Result<String, CliError> {
    match fetch_state(dir) {
        Ok(state) => Ok(state.to_string()),
        Err(Unreachable::NotRunning) => Err(CliError::failure(
            "not_running",
            "No DuckLocal window is running",
        )
        .with_hint("Start one with `ducklocal open PATH` or `ducklocal PATH`")),
        Err(Unreachable::Timeout(message)) => Err(CliError::failure("timeout", message)),
        Err(Unreachable::Refused(message)) => Err(CliError::failure("refused", message)),
    }
}

/// `ducklocal open`.
pub(crate) fn open(args: &[std::ffi::OsString]) -> Result<String, CliError> {
    let mut request = Request {
        paths: Vec::new(),
        sql: None,
        title: None,
        run: false,
    };
    let mut sql_file: Option<PathBuf> = None;
    let mut launch = true;
    let mut state = false;
    let cwd = std::env::current_dir().map_err(|e| CliError::failure("io", e))?;
    for arg in parse_args("open", args, OPEN_SPEC, &["--sql", "--title"])? {
        match arg {
            Arg::Flag("--sql", Some(value)) => {
                request.sql = Some(utf8(value, "SQL")?);
            }
            Arg::Flag("--sql-file", Some(value)) => sql_file = Some(value.into()),
            Arg::Flag("--title", Some(value)) => {
                request.title = Some(utf8(value, "--title")?);
            }
            Arg::Flag("--run", None) => request.run = true,
            Arg::Flag("--no-launch", None) => launch = false,
            Arg::Flag("--state", None) => state = true,
            Arg::Positional(value) => {
                // Joined rather than canonicalized: a pattern or a path that
                // does not exist is the window's to report, and a pattern
                // has no canonical form.
                let path = cwd.join(PathBuf::from(value));
                request.paths.push(path.to_string_lossy().into_owned());
            }
            _ => {
                return Err(CliError::failure(
                    "internal",
                    "open accepted a flag it does not handle",
                ))
            }
        }
    }
    if state {
        if args.len() > 1 {
            return Err(CliError::argument(
                "--state reads the window and opens nothing; it takes no other option or PATH",
            ));
        }
        let dir = data_dir().ok_or_else(|| {
            CliError::failure("not_running", "Cannot resolve the app data directory")
        })?;
        return read_state(&dir);
    }
    if request.sql.is_some() && sql_file.is_some() {
        return Err(CliError::argument("Use --sql or --sql-file, not both"));
    }
    if let Some(path) = sql_file {
        let sql = if path.as_os_str() == "-" {
            let mut sql = String::new();
            std::io::stdin()
                .read_to_string(&mut sql)
                .map_err(|e| CliError::failure("io", e))?;
            sql
        } else {
            std::fs::read_to_string(&path).map_err(|e| CliError::failure("io", e))?
        };
        request.sql = Some(sql);
    }
    if request
        .sql
        .as_deref()
        .is_some_and(|sql| sql.trim().is_empty())
    {
        return Err(CliError::argument("The SQL is empty"));
    }
    if request.sql.is_none() && (request.title.is_some() || request.run) {
        return Err(CliError::argument(
            "--title and --run need --sql or --sql-file",
        ));
    }
    if request.sql.is_none() && request.paths.is_empty() {
        return Err(CliError::argument(
            "Nothing to open: give --sql, --sql-file or a PATH",
        ));
    }

    let dir = data_dir()
        .ok_or_else(|| CliError::failure("not_running", "Cannot resolve the app data directory"))?;
    let launched = match deliver(&dir, &request) {
        Ok(()) => false,
        Err(Unreachable::Refused(message) | Unreachable::Timeout(message)) => {
            return Err(CliError::failure("refused", message));
        }
        Err(Unreachable::NotRunning) if !launch => {
            return Err(CliError::failure(
                "not_running",
                "No DuckLocal window is running; omit --no-launch to start one",
            ));
        }
        Err(Unreachable::NotRunning) => {
            start_window()?;
            wait_and_deliver(&dir, &request)?;
            true
        }
    };
    Ok(json!({"delivered": true, "launched": launched}).to_string())
}

fn utf8(value: std::ffi::OsString, what: &str) -> Result<String, CliError> {
    value
        .into_string()
        .map_err(|_| CliError::argument(format!("{what} must be UTF-8")))
}

/// Start the GUI detached from this command, so it outlives the terminal.
/// Inside a macOS bundle, through LaunchServices, which gives it the Dock
/// icon and menu bar a double-click would.
fn start_window() -> Result<(), CliError> {
    let exe = std::env::current_exe().map_err(|e| CliError::failure("launch", e))?;
    let mut command = match bundle_of(&exe) {
        Some(bundle) => {
            let mut command = std::process::Command::new("open");
            command.arg("-a").arg(bundle);
            command
        }
        None => std::process::Command::new(&exe),
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
        .spawn()
        .map_err(|e| CliError::failure("launch", format!("Could not start DuckLocal: {e}")))?;
    Ok(())
}

/// The `.app` an executable sits in, when it is the bundle's own binary.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}

fn wait_and_deliver(dir: &Path, request: &Request) -> Result<(), CliError> {
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    loop {
        match deliver(dir, request) {
            Ok(()) => return Ok(()),
            Err(Unreachable::Refused(message) | Unreachable::Timeout(message)) => {
                return Err(CliError::failure("refused", message))
            }
            Err(Unreachable::NotRunning) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(Unreachable::NotRunning) => {
                return Err(CliError::failure(
                    "not_running",
                    "Started DuckLocal, but its window did not begin listening within 30s",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/remote-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn request() -> Request {
        Request {
            paths: vec!["/tmp/a.csv".into()],
            sql: Some("SELECT 1".into()),
            title: Some("Agent".into()),
            run: true,
        }
    }

    #[test]
    fn a_request_reaches_the_listening_window() {
        let dir = scratch("roundtrip");
        let listening = listen(&dir).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            accept_loop(listening, move |r| tx.send(r).unwrap(), || Ok(json!({"tabs": []})))
        });
        deliver(&dir, &request()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), request());
    }

    #[test]
    fn a_wrong_token_is_refused_and_nothing_is_queued() {
        let dir = scratch("token");
        let listening = listen(&dir).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            accept_loop(listening, move |r| tx.send(r).unwrap(), || Ok(json!({"tabs": []})))
        });
        let path = dir.join(ENDPOINT_FILE);
        let mut endpoint: Endpoint =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        endpoint.token = "0".repeat(64);
        std::fs::write(&path, serde_json::to_string(&endpoint).unwrap()).unwrap();
        assert!(matches!(
            deliver(&dir, &request()),
            Err(Unreachable::Refused(message)) if message == "wrong token"
        ));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_state_request_is_answered_and_opens_nothing() {
        let dir = scratch("state");
        let listening = listen(&dir).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            accept_loop(
                listening,
                move |r| tx.send(r).unwrap(),
                || Ok(json!({"tabs": [{"kind": "query"}]})),
            )
        });
        let state = fetch_state(&dir).unwrap();
        assert_eq!(state["tabs"][0]["kind"], "query");
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_window_that_does_not_answer_times_out() {
        let dir = scratch("state-timeout");
        let listening = listen(&dir).unwrap();
        std::thread::spawn(move || {
            accept_loop(listening, |_| {}, || Err("the window did not answer within 5s".into()))
        });
        assert!(matches!(fetch_state(&dir), Err(Unreachable::Timeout(_))));
    }

    #[test]
    fn waiting_state_requests_are_all_answered() {
        let (first, first_rx) = mpsc::channel();
        let (second, second_rx) = mpsc::channel();
        STATE_WAITERS.lock().unwrap().extend([first, second]);
        assert!(state_wanted());
        answer_state(json!({"n": 1}));
        assert!(!state_wanted());
        assert_eq!(first_rx.recv().unwrap()["n"], 1);
        assert_eq!(second_rx.recv().unwrap()["n"], 1);
    }

    #[test]
    fn a_missing_or_stale_endpoint_means_no_window() {
        let dir = scratch("stale");
        assert!(matches!(
            deliver(&dir, &request()),
            Err(Unreachable::NotRunning)
        ));
        // A window that quit: its port no longer listens.
        let (listener, _) = listen(&dir).unwrap();
        drop(listener);
        assert!(matches!(
            deliver(&dir, &request()),
            Err(Unreachable::NotRunning)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn the_endpoint_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let _listening = listen(&dir).unwrap();
        let mode = std::fs::metadata(dir.join(ENDPOINT_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn only_a_bundles_own_binary_launches_through_the_bundle() {
        let inside = Path::new("/Applications/DuckLocal.app/Contents/MacOS/ducklocal");
        let loose = Path::new("/Users/me/ducklocal/target/debug/ducklocal");
        assert_eq!(bundle_of(loose), None);
        if cfg!(target_os = "macos") {
            assert_eq!(
                bundle_of(inside),
                Some(PathBuf::from("/Applications/DuckLocal.app"))
            );
        } else {
            assert_eq!(bundle_of(inside), None);
        }
    }
}
