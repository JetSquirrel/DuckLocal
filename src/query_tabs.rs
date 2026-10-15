//! Which query tabs are open, and the SQL in each, remembered between
//! launches.
//!
//! A query tab holds SQL that may never have run, so the history does not
//! have it. The workspace writes the open set to one `settings` value a moment
//! after it changes (an edit, a tab opened, closed, renamed or moved), and the
//! next launch puts the tabs back in place of the welcome tab. Their results
//! are not kept: a result can be hundreds of megabytes, and rerunning is one
//! keystroke.

use serde::{Deserialize, Serialize};

/// The `settings` key the list lives under.
pub const SETTING: &str = "query_tabs";

/// One remembered query tab.
///
/// The field names are the stored JSON's, so they must not change: a stored
/// value this build cannot read comes back as no tabs at all.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryTabState {
    pub title: String,
    pub sql: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryTabs {
    pub tabs: Vec<QueryTabState>,
    /// Which of `tabs` was in front, when a query tab was. `None` when a
    /// dashboard was, which then comes back in front as it always has.
    pub active: Option<usize>,
}

pub fn to_json(tabs: &QueryTabs) -> String {
    serde_json::to_string(tabs).unwrap_or_else(|_| "{}".to_string())
}

/// Read back a remembered set. A value this build cannot read is no tabs
/// rather than a launch that fails.
pub fn from_json(json: &str) -> QueryTabs {
    let mut tabs: QueryTabs = serde_json::from_str(json).unwrap_or_default();
    tabs.active = tabs.active.filter(|&ix| ix < tabs.tabs.len());
    tabs
}

/// Write the open query tabs, replacing what was there.
pub fn remember(tabs: &QueryTabs) {
    if let Err(error) = crate::history::set_setting(SETTING, &to_json(tabs)) {
        tracing::warn!("Could not remember the open query tabs: {error}");
    }
}

/// The query tabs to reopen at launch.
pub fn restore() -> QueryTabs {
    match crate::history::get_setting(SETTING) {
        Ok(Some(json)) => from_json(&json),
        Ok(None) => QueryTabs::default(),
        Err(error) => {
            tracing::warn!("Could not read the open query tabs: {error}");
            QueryTabs::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remembered_set_reads_back_and_drops_an_active_tab_past_the_end() {
        let tabs = QueryTabs {
            tabs: vec![
                QueryTabState {
                    title: "Revenue".into(),
                    sql: "SELECT 1".into(),
                },
                QueryTabState {
                    title: "Query 2".into(),
                    sql: String::new(),
                },
            ],
            active: Some(1),
        };
        assert_eq!(from_json(&to_json(&tabs)), tabs);

        let past_the_end = to_json(&QueryTabs {
            active: Some(5),
            ..tabs.clone()
        });
        assert_eq!(from_json(&past_the_end).active, None);
        assert_eq!(from_json("not json"), QueryTabs::default());
    }
}
