//! The contract between the window and an agent at the command line: what
//! `ducklocal open --state` publishes, and what became of each `open`.
//!
//! The state is versioned (`schema_version`, documented in
//! `docs/agent-state-v1.schema.json`): adding a field keeps the version,
//! renaming or removing one, or changing what one means, bumps it. Its
//! `revision` is a hash of what the window shows, so it needs no bookkeeping
//! at the many places a tab can change, and an agent can say which view of the
//! window its action was based on.
//!
//! Delivering a request says only that the window has it. What happened next
//! lands in the action log, under the id the command sent, and the state
//! carries the log: an agent asks the way it asks everything else.

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The version of the published state.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// The longest one preview cell runs: a JSON document or a page of text in a
/// column must not come along whole.
pub(crate) const PREVIEW_CELL_CHARS: usize = 1_000;
/// Roughly how much preview one result carries, however wide its rows.
pub(crate) const PREVIEW_BYTES: usize = 64 * 1024;
/// How many actions the log keeps: an agent reads its own soon after sending.
const MAX_ACTIONS: usize = 32;

/// What became of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActionStatus {
    /// Refused: the window changed since the revision it was based on.
    /// Nothing opened.
    Stale,
    /// Taken, and waiting for what it attaches or for a run already going.
    Queued,
    /// Its SQL is running.
    Running,
    /// Opened, with nothing asked to run.
    Opened,
    /// Its SQL ran to a result.
    Succeeded,
    /// Its attach or its SQL failed; `error` says why.
    Failed,
}

/// One request's fate, as the state reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Action {
    pub request_id: String,
    pub status: ActionStatus,
    /// The query tab the request opened.
    pub tab_id: Option<u64>,
    /// The tab's `result.status` once its SQL has run.
    pub result_status: Option<String>,
    pub error: Option<String>,
    /// For a stale request, the revision it was based on and the current one.
    pub based_on_revision: Option<String>,
    pub current_revision: Option<String>,
}

impl Action {
    pub(crate) fn new(request_id: String, status: ActionStatus) -> Self {
        Self {
            request_id,
            status,
            tab_id: None,
            result_status: None,
            error: None,
            based_on_revision: None,
            current_revision: None,
        }
    }
}

/// The window's recent actions, oldest first.
#[derive(Debug, Default)]
pub(crate) struct ActionLog(Vec<Action>);

impl ActionLog {
    /// Add an action, making room by dropping the oldest finished one: one
    /// still in flight is what someone is waiting on.
    pub(crate) fn record(&mut self, action: Action) {
        if self.0.len() >= MAX_ACTIONS {
            let finished = self
                .0
                .iter()
                .position(|a| !matches!(a.status, ActionStatus::Queued | ActionStatus::Running));
            self.0.remove(finished.unwrap_or(0));
        }
        self.0.push(action);
    }

    pub(crate) fn update(&mut self, request_id: &str, change: impl FnOnce(&mut Action)) {
        if let Some(action) = self.0.iter_mut().find(|a| a.request_id == request_id) {
            change(action);
        }
    }

    fn entries(&self) -> &[Action] {
        &self.0
    }
}

/// The token for what the window shows: equal for equal content, whichever
/// tabs changed and however.
pub(crate) fn revision(body: &Value) -> String {
    let digest = Sha256::digest(body.to_string().as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// The published state: what the window shows, with its version, its revision,
/// the bounds on its previews and the action log. Actions stay out of the
/// revision, so reading how a request went does not make the next one stale.
pub(crate) fn publish(mut body: Value, actions: &ActionLog) -> Value {
    let revision = revision(&body);
    body["schema_version"] = json!(SCHEMA_VERSION);
    body["revision"] = json!(revision);
    body["limits"] = json!({
        "preview_rows": crate::remote::STATE_PREVIEW_ROWS,
        "preview_cell_chars": PREVIEW_CELL_CHARS,
        "preview_bytes": PREVIEW_BYTES,
    });
    body["actions"] = json!(actions.entries());
    body
}

/// A result's first rows as the state carries them, and whether anything of
/// those rows was left out: a long cell is cut at `PREVIEW_CELL_CHARS`, and
/// rows stop once `PREVIEW_BYTES` is reached.
pub(crate) fn bounded_preview(rows: &[Vec<String>]) -> (Vec<Vec<String>>, bool) {
    let mut preview = Vec::new();
    let mut cut = false;
    let mut bytes = 0;
    for row in rows.iter().take(crate::remote::STATE_PREVIEW_ROWS) {
        let mut kept = Vec::with_capacity(row.len());
        for cell in row {
            match cell.char_indices().nth(PREVIEW_CELL_CHARS) {
                Some((end, _)) => {
                    kept.push(format!("{}…", &cell[..end]));
                    cut = true;
                }
                None => kept.push(cell.clone()),
            }
        }
        bytes += kept.iter().map(String::len).sum::<usize>();
        if bytes > PREVIEW_BYTES {
            return (preview, true);
        }
        preview.push(kept);
    }
    (preview, cut)
}

/// Checks a state against the schema file, for tests here and for the one
/// that reads a real window's.
#[cfg(test)]
pub(crate) mod contract {
    use serde_json::Value;

    /// `value` against the subset of JSON Schema the published file uses:
    /// `type`, `enum`, `required`, `properties`, `items`.
    fn violations(schema: &Value, value: &Value, at: &str, found: &mut Vec<String>) {
        if let Some(types) = schema.get("type") {
            let named: Vec<&str> = match types {
                Value::Array(types) => types.iter().filter_map(Value::as_str).collect(),
                other => other.as_str().into_iter().collect(),
            };
            let fits = named.iter().any(|name| match *name {
                "object" => value.is_object(),
                "array" => value.is_array(),
                "string" => value.is_string(),
                "integer" => value.is_i64() || value.is_u64(),
                "boolean" => value.is_boolean(),
                "null" => value.is_null(),
                other => panic!("the check does not know type {other}"),
            });
            if !fits {
                found.push(format!("{at}: {value} is not {named:?}"));
            }
        }
        if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
            if !allowed.contains(value) {
                found.push(format!("{at}: {value} is not one of {allowed:?}"));
            }
        }
        for name in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if value.get(name).is_none() {
                found.push(format!("{at}: missing {name}"));
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (name, property) in properties {
                if let Some(child) = value.get(name) {
                    violations(property, child, &format!("{at}.{name}"), found);
                }
            }
        }
        if let (Some(items), Some(values)) = (schema.get("items"), value.as_array()) {
            for (ix, child) in values.iter().enumerate() {
                violations(items, child, &format!("{at}[{ix}]"), found);
            }
        }
    }

    pub(crate) fn schema() -> Value {
        serde_json::from_str(include_str!("../docs/agent-state-v1.schema.json")).unwrap()
    }

    /// Where `state` departs from the published schema; empty when it fits.
    pub(crate) fn schema_violations(state: &Value) -> Vec<String> {
        let mut found = Vec::new();
        violations(&schema(), state, "state", &mut found);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::contract::{schema, schema_violations};
    use super::*;

    fn body() -> Value {
        json!({
            "database": ":memory:",
            "attached": [],
            "active_tab": 0,
            "tabs": [{
                "id": 1,
                "title": "Query 1",
                "active": true,
                "kind": "query",
                "sql": "SELECT 1",
                "result": {"status": "empty", "view": "table"},
            }],
        })
    }

    #[test]
    fn the_published_schema_is_this_version_and_fits_the_state() {
        assert_eq!(
            schema()["properties"]["schema_version"]["enum"],
            json!([SCHEMA_VERSION])
        );
        let mut log = ActionLog::default();
        log.record(Action::new("abc".into(), ActionStatus::Queued));
        let state = publish(body(), &log);
        assert_eq!(schema_violations(&state), Vec::<String>::new());
    }

    #[test]
    fn the_check_catches_a_field_the_contract_promises() {
        let mut state = publish(body(), &ActionLog::default());
        state["tabs"][0].as_object_mut().unwrap().remove("id");
        state["revision"] = json!(7);
        let found = schema_violations(&state);
        assert_eq!(found.len(), 2, "{found:?}");
    }

    #[test]
    fn a_revision_follows_the_content_and_ignores_the_log() {
        let quiet = publish(body(), &ActionLog::default());
        let mut log = ActionLog::default();
        log.record(Action::new("abc".into(), ActionStatus::Succeeded));
        let same = publish(body(), &log);
        assert_eq!(quiet["revision"], same["revision"]);
        assert_eq!(quiet["revision"].as_str().unwrap().len(), 16);

        let mut edited = body();
        edited["tabs"][0]["sql"] = json!("SELECT 2");
        assert_ne!(quiet["revision"], publish(edited, &log)["revision"]);
    }

    #[test]
    fn a_preview_stops_at_its_rows_its_cells_and_its_bytes() {
        let rows: Vec<Vec<String>> = (0..100).map(|i| vec![i.to_string()]).collect();
        let (preview, cut) = bounded_preview(&rows);
        assert_eq!(preview.len(), crate::remote::STATE_PREVIEW_ROWS);
        // Rows past the preview are what `row_count` and `truncated` say.
        assert!(!cut);

        let (preview, cut) = bounded_preview(&[vec!["é".repeat(PREVIEW_CELL_CHARS + 5)]]);
        assert!(cut);
        assert_eq!(preview[0][0].chars().count(), PREVIEW_CELL_CHARS + 1);
        assert!(preview[0][0].ends_with('…'));

        let wide = vec!["x".repeat(PREVIEW_CELL_CHARS); 40];
        let (preview, cut) = bounded_preview(&vec![wide; 20]);
        assert!(cut);
        assert_eq!(preview.len(), 1);
        let bytes: usize = preview.iter().flatten().map(String::len).sum();
        assert!(bytes <= PREVIEW_BYTES);
    }

    #[test]
    fn the_log_keeps_what_is_in_flight_when_it_fills() {
        let mut log = ActionLog::default();
        log.record(Action::new("waiting".into(), ActionStatus::Running));
        for i in 0..MAX_ACTIONS {
            log.record(Action::new(format!("r{i}"), ActionStatus::Opened));
        }
        assert_eq!(log.entries().len(), MAX_ACTIONS);
        assert_eq!(log.entries()[0].request_id, "waiting");
        assert_eq!(log.entries()[1].request_id, "r1");

        log.update("waiting", |action| action.status = ActionStatus::Failed);
        assert_eq!(log.entries()[0].status, ActionStatus::Failed);
    }
}
