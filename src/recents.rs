//! Recently opened documents: analysis apps and `.dash` dashboards.
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
    /// A folder with a main.js, opened as an app tab.
    App,
    /// A `.dash` file, opened as a dashboard tab.
    Dashboard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentDocument {
    pub path: String,
    pub kind: RecentKind,
    pub title: String,
}

/// Move `document` to the front, dropping any older entry for the same path.
fn upsert(documents: &mut Vec<RecentDocument>, document: RecentDocument) {
    documents.retain(|d| d.path != document.path);
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
    match crate::history::get_setting(SETTING) {
        Ok(Some(json)) => serde_json::from_str(&json).unwrap_or_default(),
        _ => Vec::new(),
    }
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
            kind: RecentKind::App,
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
    fn the_list_holds_at_most_max_entries() {
        let mut documents = Vec::new();
        for index in 0..MAX + 5 {
            upsert(&mut documents, document(&format!("/{index}")));
        }
        assert_eq!(documents.len(), MAX);
        assert_eq!(documents[0].path, format!("/{}", MAX + 4));
        assert!(documents.iter().all(|d| d.path != "/0"));
    }
}
