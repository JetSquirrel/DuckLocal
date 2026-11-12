//! `examples/cloudtrail_investigation`, walked the way its README tells an agent
//! to, on a copy so nothing lands in the repository. The person's part in the
//! window is not here: applying a proposal has no command, by design.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

struct Copy {
    root: PathBuf,
    folder: PathBuf,
}

impl Copy {
    fn new(name: &str) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/cli-tests")
            .join(format!("cloudtrail-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let folder = root.join("investigation");
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/cloudtrail_investigation"),
            &folder,
        );
        std::fs::create_dir_all(root.join("home")).unwrap();
        Self { root, folder }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ducklocal"))
            .args(args)
            .current_dir(&self.folder)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home"))
            .env("XDG_DATA_HOME", self.root.join("home"))
            .env("DUCKLOCAL_SESSION", "investigation.session.jsonl")
            .env("DUCKLOCAL_AGENT", "ExampleAgent")
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// A step of the investigation, under the limits the README asks for.
    fn step(&self, file: &str) -> Value {
        self.json(&[
            "query",
            "--restricted",
            "--allow-read",
            "data",
            "--sql-file",
            &format!("sql/{file}"),
        ])
    }
}

impl Drop for Copy {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A row by the value of its first columns.
fn row<'a>(result: &'a Value, leading: &[&str]) -> &'a Vec<Value> {
    result["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row.as_array().unwrap())
        .find(|row| {
            leading
                .iter()
                .zip(row.iter())
                .all(|(want, cell)| cell.as_str() == Some(want))
        })
        .unwrap_or_else(|| panic!("no row {leading:?} in {result}"))
}

#[test]
fn the_investigation_finds_what_the_readme_says() {
    let copy = Copy::new("finding");

    // The log's own shape: one object holding every event.
    let schema = copy.json(&["schema", "data/cloudtrail.json.gz", "--stats"]);
    assert_eq!(schema["row_count"], 1);
    assert_eq!(schema["columns"][0]["name"], "Records");
    // One row, one column: the schema says how to get at the 795 events.
    let hint = schema["hint"].as_str().unwrap();
    assert!(
        hint.contains("SELECT unnest(\"Records\", max_depth := 2) FROM"),
        "{hint}"
    );
    let flat = hint.split("with: ").nth(1).unwrap();
    // In a copy of its own, so the session below holds only the investigation.
    let aside = Copy::new("hint");
    let events = aside.json(&["query", "--sql", flat, "--limit", "1"]);
    assert_eq!(events["total_rows"], 795);

    let who = copy.step("01_who.sql");
    assert_eq!(who["row_count"], 5);
    let by_address = row(&who, &["ci-deploy", "203.0.113.77"]);
    assert_eq!(by_address[2], 33, "events");
    assert_eq!(by_address[3], 28, "refused");
    assert_eq!(row(&who, &["ci-deploy", "198.51.100.20"])[3], 0);
    let new_user = row(&who, &["svc-backup", "203.0.113.78"]);
    assert_eq!(new_user[2], 243);
    assert_eq!(new_user[3], 1);
    let total: i64 = who["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[2].as_i64().unwrap())
        .sum();
    assert_eq!(total, 795);

    let hours = copy.step("02_outside_hours.sql");
    assert_eq!(row(&hours, &["198.51.100.20"])[2], 0);
    assert_eq!(row(&hours, &["203.0.113.77"])[2], 33);

    let changes = copy.step("03_changes.sql");
    let names: Vec<&str> = changes["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[3].as_str().unwrap())
        .collect();
    assert_eq!(names, ["CreateUser", "AttachUserPolicy", "CreateAccessKey"]);
    assert_eq!(
        changes["rows"][1][5],
        "arn:aws:iam::aws:policy/AdministratorAccess"
    );

    let after = copy.step("04_new_user.sql");
    assert_eq!(row(&after, &["GetObject", "customer-exports"])[3], 240);
    assert_eq!(row(&after, &["StopLogging"])[2], "AccessDenied");

    // The whole timeline does not fit, and the output says so rather than
    // passing the first rows off as all of them.
    let timeline = copy.json(&[
        "query",
        "--restricted",
        "--allow-read",
        "data",
        "--limit",
        "5",
        "--sql-file",
        "sql/05_timeline.sql",
    ]);
    assert_eq!(timeline["row_count"], 5);
    assert_eq!(timeline["truncated"], true);
    assert_eq!(timeline["total_rows"], 276);

    // The session keeps that doubt for whoever picks the work up.
    let session = copy.json(&["session", "show"]);
    let events = session["events"].as_array().unwrap();
    // Each step keeps its SQL and knows what it read: a column named for a key
    // would cost both, since the session errs towards leaving text out.
    assert_eq!(events[0]["sources"][0]["ref"], "data/cloudtrail.json.gz");
    assert!(events[0]["sql"].as_str().unwrap().contains("user_name"));
    assert_eq!(events[0]["doubts"], json!([]));
    let last = events.last().unwrap();
    assert_eq!(last["complete"], false);
    assert!(last["doubts"][0].as_str().unwrap().contains("only 5 of"));
    let text = std::fs::read_to_string(copy.folder.join("investigation.session.jsonl")).unwrap();
    // A value only the rows hold: the session keeps the SQL, never the results.
    assert!(!text.contains("GetCallerIdentity"), "{text}");
}

#[test]
fn a_proposal_leaves_the_report_until_the_person_decides() {
    let copy = Copy::new("proposal");
    let report = "reports/investigation.dash";
    let before = std::fs::read(copy.folder.join(report)).unwrap();

    let summary = copy.json(&["check", report, "--summary"]);
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["plots"], 5);
    assert_eq!(summary["empty"], json!([]));
    assert_eq!(summary["unresolved"], json!([]));
    assert_eq!(summary["truncated"], json!([]));

    // A plot on the first version, then the question asked about it.
    let thread = copy.json(&[
        "comments",
        report,
        "--add",
        "changes",
        "--text",
        "Did the new user do anything after this?",
    ]);
    assert_eq!(thread["thread"]["plot_query"], "changes");
    assert_eq!(copy.json(&["comments", report])["open"], 1);

    let proposal = copy.json(&[
        "propose",
        report,
        "--from",
        "proposals/answer-new-user.dash",
        "--note",
        "Adds what svc-backup did",
    ]);
    assert_eq!(proposal["status"], "pending");
    assert_eq!(proposal["check"]["ok"], true);
    assert_eq!(proposal["check"]["empty"], json!([]));
    assert_eq!(std::fs::read(copy.folder.join(report)).unwrap(), before);

    // Only the window applies it.
    assert_eq!(
        copy.json(&["propose", report, "--status"])["status"],
        "pending"
    );
    let summary = copy.json(&["check", "proposals/answer-new-user.dash", "--summary"]);
    assert_eq!(summary["plots"], 7);

    let reply = copy.json(&[
        "comments",
        report,
        "--reply",
        "c1",
        "--text",
        "Yes: 240 objects from customer-exports. See the proposal.",
        "--resolve",
    ]);
    assert_eq!(reply["thread"]["resolved"], true);
    assert_eq!(copy.json(&["comments", report])["open"], 0);
}

#[test]
fn the_project_moves_and_says_what_a_move_broke() {
    let copy = Copy::new("project");
    let check = |folder: &Path| -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_ducklocal"))
            .args(["project", "check"])
            .current_dir(folder)
            .env("HOME", copy.root.join("home"))
            .env("XDG_CONFIG_HOME", copy.root.join("home"))
            .env("XDG_DATA_HOME", copy.root.join("home"))
            .output()
            .unwrap();
        assert!(output.status.success());
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let report = check(&copy.folder);
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["portable"], true, "{report}");
    assert_eq!(report["dashboards"].as_array().unwrap().len(), 1);
    assert_eq!(report["dashboards"][0]["checked"], true);

    let moved = copy.root.join("somewhere/else");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&copy.folder, &moved).unwrap();
    assert_eq!(check(&moved)["ok"], true);

    std::fs::remove_file(moved.join("data/cloudtrail.json.gz")).unwrap();
    let broken = check(&moved);
    assert_eq!(broken["ok"], false);
    assert_eq!(broken["dashboards"][0]["sources"][0]["status"], "missing");
    assert!(!broken["problems"].as_array().unwrap().is_empty());
}
