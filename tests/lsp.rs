//! The language server end to end: real `Content-Length` frames over real
//! stdio, against the real binary. No client library — the protocol half of
//! the tests is thirty lines, and writing the frames by hand keeps the test
//! honest about what a client actually sends.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

/// A running `ducklocal lsp` with its frames parsed on a reader thread, so a
/// stuck server fails the test on a timeout instead of hanging it forever.
struct Lsp {
    child: Child,
    stdin: ChildStdin,
    inbox: Receiver<Value>,
}

impl Lsp {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ducklocal"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the binary spawns");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(message) = read_frame(&mut reader) {
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
        Lsp {
            child,
            stdin,
            inbox: rx,
        }
    }

    fn send(&mut self, message: Value) {
        let body = message.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)
            .and_then(|_| self.stdin.flush())
            .expect("the frame is written");
    }

    fn recv(&self) -> Value {
        self.inbox
            .recv_timeout(Duration::from_secs(30))
            .expect("the server answered within 30s")
    }

    /// Read frames until one matches: responses and notifications interleave
    /// freely once a document is open.
    fn until(&self, matches: impl Fn(&Value) -> bool) -> Value {
        loop {
            let message = self.recv();
            if matches(&message) {
                return message;
            }
        }
    }

    fn response(&self, id: i64) -> Value {
        self.until(|message| message["id"] == id)
    }

    fn notification(&self, method: &str) -> Value {
        self.until(|message| message["method"] == method)
    }

    /// The initialize handshake, done the way every test needs it.
    fn initialize(&mut self) -> Value {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"processId": null, "capabilities": {}},
        }));
        let response = self.response(1);
        self.send(json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
        response
    }

    fn open(&mut self, uri: &str, text: &str) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "dash",
                    "version": 1,
                    "text": text,
                },
            },
        }));
    }

    fn change(&mut self, uri: &str, text: &str) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": text}],
            },
        }));
    }

    fn exit_code(&mut self) -> i32 {
        self.child
            .wait()
            .expect("the server exits")
            .code()
            .expect("the server was not killed by a signal")
    }
}

fn read_frame(reader: &mut BufReader<ChildStdout>) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {}
            Err(_) => return None,
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = Some(value.trim().parse::<usize>().expect("a numeric length"));
        }
    }
    let length = length.expect("a Content-Length header");
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(serde_json::from_slice(&body).expect("a JSON frame"))
}

/// A spec that parses and validates, with a reference to jump through.
const GOOD: &str = "query \"latency\" {
  sql = \"SELECT 1\"
}

plot \"p\" {
  type = \"line\"
  query = query.latency
  x = timestamp
  y = latency
}
";

#[test]
fn initialize_advertises_the_four_features() {
    let mut lsp = Lsp::spawn();
    let response = lsp.initialize();
    let capabilities = &response["result"]["capabilities"];
    assert_eq!(capabilities["textDocumentSync"], 1, "{capabilities}");
    assert!(capabilities["completionProvider"].is_object(), "{capabilities}");
    assert_eq!(capabilities["hoverProvider"], true, "{capabilities}");
    assert_eq!(capabilities["definitionProvider"], true, "{capabilities}");

    // A clean shutdown: exit code 0.
    lsp.send(json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}));
    assert_eq!(lsp.response(2)["result"], Value::Null);
    lsp.send(json!({"jsonrpc": "2.0", "method": "exit"}));
    assert_eq!(lsp.exit_code(), 0);
}

#[test]
fn exit_without_shutdown_is_abnormal() {
    let mut lsp = Lsp::spawn();
    lsp.initialize();
    lsp.send(json!({"jsonrpc": "2.0", "method": "exit"}));
    assert_eq!(lsp.exit_code(), 1);
}

#[test]
fn a_syntax_error_is_one_diagnostic_at_the_token() {
    let mut lsp = Lsp::spawn();
    lsp.initialize();
    let uri = "file:///syntax.dash";
    lsp.open(uri, "plot {\n");
    let params = lsp.notification("textDocument/publishDiagnostics");
    assert_eq!(params["params"]["uri"], uri);
    let diagnostics = &params["params"]["diagnostics"];
    assert_eq!(diagnostics.as_array().unwrap().len(), 1, "{diagnostics}");
    // `{` of `plot {` — byte span (5,6), so columns 5-6 of line 0.
    assert_eq!(
        diagnostics[0]["range"],
        json!({"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 6}}),
        "{diagnostics}"
    );
    assert_eq!(diagnostics[0]["severity"], 1, "errors, not warnings");
    assert!(!diagnostics[0]["message"].as_str().unwrap().is_empty());

    lsp.send(json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}));
    lsp.response(2);
    lsp.send(json!({"jsonrpc": "2.0", "method": "exit"}));
    assert_eq!(lsp.exit_code(), 0);
}

#[test]
fn a_semantic_error_points_at_the_kind_keyword() {
    let mut lsp = Lsp::spawn();
    lsp.initialize();
    let uri = "file:///semantic.dash";
    lsp.open(uri, "wat \"h\" {}\n");
    let params = lsp.notification("textDocument/publishDiagnostics");
    let diagnostics = &params["params"]["diagnostics"];
    assert_eq!(diagnostics.as_array().unwrap().len(), 1, "{diagnostics}");
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .contains("Unknown block"),
        "{diagnostics}"
    );
    assert_eq!(
        diagnostics[0]["range"],
        json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}}),
        "{diagnostics}"
    );

    lsp.send(json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}));
    lsp.response(2);
    lsp.send(json!({"jsonrpc": "2.0", "method": "exit"}));
    assert_eq!(lsp.exit_code(), 0);
}

#[test]
fn a_full_document_change_republishes_diagnostics() {
    let mut lsp = Lsp::spawn();
    lsp.initialize();
    let uri = "file:///changing.dash";
    lsp.open(uri, "plot {\n");
    let params = lsp.notification("textDocument/publishDiagnostics");
    assert_eq!(params["params"]["diagnostics"].as_array().unwrap().len(), 1);

    // Full sync: the change replaces the whole document, and a clean file
    // publishes an empty list rather than going silent.
    lsp.change(uri, GOOD);
    let params = lsp.notification("textDocument/publishDiagnostics");
    assert_eq!(
        params["params"]["diagnostics"],
        json!([]),
        "{params}"
    );

    lsp.send(json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}));
    lsp.response(2);
    lsp.send(json!({"jsonrpc": "2.0", "method": "exit"}));
    assert_eq!(lsp.exit_code(), 0);
}

#[test]
fn definition_jumps_a_reference_to_its_query_block() {
    let mut lsp = Lsp::spawn();
    lsp.initialize();
    let uri = "file:///definition.dash";
    lsp.open(uri, GOOD);
    lsp.notification("textDocument/publishDiagnostics");

    // Line 6 is `  query = query.latency`; the `latency` segment starts at
    // column 16. Asking from inside it resolves to the query block's quoted
    // name on line 0: `"latency"` spans columns 6-15.
    lsp.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/definition",
        "params": {
            "textDocument": {"uri": uri},
            "position": {"line": 6, "character": 18},
        },
    }));
    let response = lsp.response(2);
    assert_eq!(
        response["result"],
        json!({
            "uri": uri,
            "range": {
                "start": {"line": 0, "character": 6},
                "end": {"line": 0, "character": 15},
            },
        }),
        "{response}"
    );

    lsp.send(json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown"}));
    lsp.response(3);
    lsp.send(json!({"jsonrpc": "2.0", "method": "exit"}));
    assert_eq!(lsp.exit_code(), 0);
}

#[test]
fn completion_inside_a_plot_offers_its_attributes() {
    let mut lsp = Lsp::spawn();
    lsp.initialize();
    let uri = "file:///completion.dash";
    lsp.open(uri, "plot \"p\" {\n  \n}\n");
    lsp.notification("textDocument/publishDiagnostics");

    // On the empty line inside the plot block: every plot attribute, each
    // with a text edit replacing the (empty) prefix at the cursor.
    lsp.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/completion",
        "params": {
            "textDocument": {"uri": uri},
            "position": {"line": 1, "character": 2},
        },
    }));
    let response = lsp.response(2);
    let items = response["result"].as_array().expect("an array of items");
    let labels: Vec<&str> = items
        .iter()
        .map(|item| item["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        ["type", "query", "x", "y", "series", "title"],
        "{response}"
    );
    assert_eq!(items[0]["kind"], 10, "property: {response}");
    assert_eq!(
        items[0]["textEdit"],
        json!({
            "range": {
                "start": {"line": 1, "character": 2},
                "end": {"line": 1, "character": 2},
            },
            "newText": "type = ",
        }),
        "{response}"
    );

    lsp.send(json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown"}));
    lsp.response(3);
    lsp.send(json!({"jsonrpc": "2.0", "method": "exit"}));
    assert_eq!(lsp.exit_code(), 0);
}

#[test]
fn hover_documents_an_attribute() {
    let mut lsp = Lsp::spawn();
    lsp.initialize();
    let uri = "file:///hover.dash";
    lsp.open(uri, GOOD);
    lsp.notification("textDocument/publishDiagnostics");

    // On the `type` attribute name of the plot block, line 5.
    lsp.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/hover",
        "params": {
            "textDocument": {"uri": uri},
            "position": {"line": 5, "character": 3},
        },
    }));
    let response = lsp.response(2);
    let value = response["result"]["contents"]["value"].as_str().unwrap();
    assert!(value.contains("plot's kind"), "{response}");

    // On the `latency` segment of the reference: the query and its SQL.
    lsp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "textDocument/hover",
        "params": {
            "textDocument": {"uri": uri},
            "position": {"line": 6, "character": 18},
        },
    }));
    let response = lsp.response(3);
    let value = response["result"]["contents"]["value"].as_str().unwrap();
    assert!(value.contains("query \"latency\""), "{response}");
    assert!(value.contains("SELECT 1"), "{response}");

    lsp.send(json!({"jsonrpc": "2.0", "id": 4, "method": "shutdown"}));
    lsp.response(4);
    lsp.send(json!({"jsonrpc": "2.0", "method": "exit"}));
    assert_eq!(lsp.exit_code(), 0);
}
