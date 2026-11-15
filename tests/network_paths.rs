//! Every place the app can reach the network, by file, so a new one cannot
//! arrive unnoticed: it fails here until it is added below, which is the
//! prompt to add it to `docs/network-and-privacy.md` too.
//!
//! A search of the source, so it is a tripwire and not a proof: it cannot see
//! inside a dependency. `scripts/check-offline.sh` watches what actually runs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What can open a connection, or asks DuckDB to.
const NETWORK_APIS: &[&str] = &[
    "ureq::",
    "reqwest",
    "hyper::",
    "TcpStream::",
    "TcpListener::",
    "UdpSocket",
    "ToSocketAddrs",
    "open_url(",
    "INSTALL ",
    "UPDATE EXTENSIONS",
];

/// `(file, API)`, each one a row of `docs/network-and-privacy.md`.
const KNOWN: &[(&str, &str)] = &[
    // The update check.
    ("src/ui/update.rs", "ureq::"),
    // Map base tiles.
    ("src/ui/tiles.rs", "ureq::"),
    // S3 browsing, and the httpfs download that enabling S3 starts.
    ("src/s3.rs", "ureq::"),
    ("src/ui/title_bar.rs", "INSTALL "),
    // The Extensions tab.
    ("src/extensions.rs", "INSTALL "),
    ("src/extensions.rs", "UPDATE EXTENSIONS"),
    // The window's listener for `ducklocal open`: loopback only.
    ("src/remote.rs", "TcpListener::"),
    ("src/remote.rs", "TcpStream::"),
    // The download link the status bar shows when an update cannot install.
    ("src/ui/status_bar.rs", "open_url("),
];

/// HTTP and telemetry clients the app must not depend on directly.
const NOT_DEPENDENCIES: &[&str] = &[
    "reqwest",
    "hyper",
    "isahc",
    "curl",
    "attohttpc",
    "sentry",
    "opentelemetry",
    "posthog",
    "segment",
    "amplitude",
    "mixpanel",
    "wry",
    "tungstenite",
];

fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

/// A file's code without its `#[cfg(test)]` tail and comments: what ships.
fn shipped(source: &str) -> String {
    let code = source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(source);
    code.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Files that can name these without using them: test-only modules, and the
/// interface's words.
const NOT_CODE: &[&str] = &["src/perf_probe.rs", "src/duckdb_contract.rs", "src/i18n.rs"];

fn found() -> BTreeSet<(String, &'static str)> {
    let mut files = Vec::new();
    rust_files(&root().join("src"), &mut files);
    let mut found = BTreeSet::new();
    for file in files {
        let name = file
            .strip_prefix(root())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if NOT_CODE.contains(&name.as_str()) {
            continue;
        }
        let source = std::fs::read_to_string(&file)
            .unwrap()
            .replace("\r\n", "\n");
        let code = shipped(&source);
        for api in NETWORK_APIS {
            if code.contains(api) {
                found.insert((name.clone(), *api));
            }
        }
    }
    found
}

#[test]
fn every_network_path_is_a_known_one() {
    let known: BTreeSet<(String, &str)> = KNOWN
        .iter()
        .map(|(file, api)| (file.to_string(), *api))
        .collect();
    let found = found();
    let new: Vec<_> = found.difference(&known).collect();
    assert!(
        new.is_empty(),
        "New network paths: {new:?}. Add each to KNOWN here and to the table in \
         docs/network-and-privacy.md, and keep it off until someone asks for it."
    );
    let gone: Vec<_> = known.difference(&found).collect();
    assert!(
        gone.is_empty(),
        "No longer found: {gone:?}. Take them out of KNOWN, and out of \
         docs/network-and-privacy.md if the path has gone."
    );
}

#[test]
fn the_page_names_what_the_code_reaches() {
    let page = std::fs::read_to_string(root().join("docs/network-and-privacy.md")).unwrap();
    for destination in [
        "dl.ducklocal.app",
        "tile.openstreetmap.org",
        "map_tile_url",
        "extensions.duckdb.org",
        "community-extensions.duckdb.org",
        "httpfs",
        "127.0.0.1",
    ] {
        assert!(
            page.contains(destination),
            "the page does not name {destination}"
        );
    }
    let code = |file: &str| std::fs::read_to_string(root().join(file)).unwrap();
    assert!(code("src/ui/update.rs").contains("dl.ducklocal.app"));
    assert!(code("src/ui/tiles.rs").contains("tile.openstreetmap.org"));
}

#[test]
fn no_telemetry_or_second_http_client_is_a_direct_dependency() {
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).unwrap();
    for line in manifest.lines() {
        let Some(name) = line.split(['=', ' ']).next() else {
            continue;
        };
        assert!(
            !NOT_DEPENDENCIES.contains(&name.trim()),
            "{name} is a direct dependency; if that is right, say what it sends in \
             docs/network-and-privacy.md and take it off this list"
        );
    }
}

#[test]
fn the_check_catches_a_new_path_rather_than_passing_on_nothing() {
    // The search would pass on nothing if it stopped finding anything: at
    // least the paths everyone knows are there must turn up.
    let found = found();
    assert!(found.contains(&("src/ui/tiles.rs".to_string(), "ureq::")));
    assert!(found.contains(&("src/remote.rs".to_string(), "TcpListener::")));
    assert!(
        shipped("let a = ureq::get(x);\n#[cfg(test)]\nmod tests { reqwest }").contains("ureq::")
    );
    assert!(
        !shipped("// ureq::\nlet a = 1;\n#[cfg(test)]\nmod tests { ureq:: }").contains("ureq::")
    );
}
