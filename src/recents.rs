//! Recently opened documents: `.dash` dashboards.
//!
//! The sidebar lists these so a document can be reopened the way a file is —
//! click, and the tab is back. The list is not the session's open tabs (those
//! restore separately, and only while open): a document stays here after its
//! tab closes, until newer documents push it out.

use std::path::Path;

use serde::{Deserialize, Serialize};

const SETTING: &str = "recent_documents";

/// How many documents the list holds. The sidebar shows every entry, so this
/// is a display budget as much as a storage one.
const MAX: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecentKind {
    /// A `.dash` file, opened as a dashboard tab.
    Dashboard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentDocument {
    pub path: String,
    pub kind: RecentKind,
    pub title: String,
}

/// Whether two paths name the same file. Compared as the file system
/// resolves them, not as text: on Windows `E:\Proj.dash` and
/// `E:\proj.dash` are one file, and so are a relative and an absolute
/// spelling anywhere. A path that does not resolve (the file is gone) falls
/// back to its text.
pub fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Move `document` to the front, dropping any older entry for the same file.
fn upsert(documents: &mut Vec<RecentDocument>, document: RecentDocument) {
    let path = Path::new(&document.path);
    documents.retain(|d| !same_file(Path::new(&d.path), path));
    documents.insert(0, document);
    documents.truncate(MAX);
}

/// Record an opening. The title is remembered with the path so the sidebar
/// can name an entry without re-reading the document.
pub fn add(path: &Path, kind: RecentKind, title: &str) {
    let mut documents = list();
    upsert(
        &mut documents,
        RecentDocument {
            path: path.to_string_lossy().into_owned(),
            kind,
            title: title.to_string(),
        },
    );
    let json = serde_json::to_string(&documents).unwrap_or_else(|_| "[]".to_string());
    if let Err(error) = crate::history::set_setting(SETTING, &json) {
        tracing::warn!("Could not remember the recent document: {error}");
    }
}

/// The list, most recent first. A value this build cannot read is an empty
/// list rather than a sidebar that fails: recents are a convenience.
pub fn list() -> Vec<RecentDocument> {
    let documents = match crate::history::get_setting(SETTING) {
        Ok(Some(json)) => parse(&json),
        _ => Vec::new(),
    };
    // A list written before entries were compared as files may name one
    // file twice; the more recent spelling stays.
    let mut kept: Vec<RecentDocument> = Vec::with_capacity(documents.len());
    for document in documents {
        let path = Path::new(&document.path);
        if !kept.iter().any(|k| same_file(Path::new(&k.path), path)) {
            kept.push(document);
        }
    }
    kept
}

/// Read the stored list one entry at a time, so an entry of a kind this build
/// no longer opens (the old `app` documents) is dropped rather than costing
/// the whole list.
fn parse(json: &str) -> Vec<RecentDocument> {
    serde_json::from_str::<Vec<serde_json::Value>>(json)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| serde_json::from_value(entry).ok())
        .collect()
}

/// Drop one path — the document is gone, so offering it would be a broken
/// promise.
pub fn remove(path: &str) {
    let mut documents = list();
    documents.retain(|d| d.path != path);
    let json = serde_json::to_string(&documents).unwrap_or_else(|_| "[]".to_string());
    if let Err(error) = crate::history::set_setting(SETTING, &json) {
        tracing::warn!("Could not drop the recent document: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(path: &str) -> RecentDocument {
        RecentDocument {
            path: path.to_string(),
            kind: RecentKind::Dashboard,
            title: path.to_string(),
        }
    }

    #[test]
    fn reopening_a_document_moves_it_to_the_front_without_duplicating_it() {
        let mut documents = vec![document("/a"), document("/b")];
        upsert(&mut documents, document("/b"));
        let paths: Vec<&str> = documents.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["/b", "/a"]);
    }

    #[test]
    fn two_spellings_of_one_file_are_one_entry() {
        let dir = std::env::temp_dir().join(format!("ducklocal_recents_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let file = dir.join("d.dash");
        std::fs::write(&file, "").unwrap();
        // The same file through a `..` detour.
        let detour = dir.join("sub").join("..").join("d.dash");
        let mut documents = vec![document(&file.to_string_lossy())];
        upsert(&mut documents, document(&detour.to_string_lossy()));
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].path, detour.to_string_lossy());
        assert!(!same_file(&file, &dir.join("other.dash")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_list_holds_at_most_max_entries() {
        let mut documents = Vec::new();
        for index in 0..MAX + 5 {
            upsert(&mut documents, document(&format!("/{index}")));
        }
        assert_eq!(documents.len(), MAX);
        assert_eq!(documents[0].path, format!("/{}", MAX + 4));
        assert!(documents.iter().all(|d| d.path != "/0"));
    }

    #[test]
    fn an_entry_of_a_retired_kind_is_dropped_alone() {
        let json = r#"[
            {"path": "/apps/sales", "kind": "app", "title": "sales"},
            {"path": "/d/usage.dash", "kind": "dashboard", "title": "usage"}
        ]"#;
        let paths: Vec<String> = parse(json).into_iter().map(|d| d.path).collect();
        assert_eq!(paths, vec!["/d/usage.dash"]);
        assert!(parse("not json").is_empty());
    }
}
