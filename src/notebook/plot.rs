//! A notebook's plot cell, read with the `.dash` format's own rules.
//!
//! A plot cell holds the attributes of a `.dash` plot block, one a line —
//! `type = "bar"`, `x = "channel"`, `y = "revenue"`, and the rest the format
//! knows — plus `data`, which names the SQL cell it draws. Rather than a
//! second reader with its own idea of what a plot is, the cell is wrapped
//! into a one-plot spec and handed to `spec::model`, so a notebook plot and
//! a dashboard plot accept the same types and attributes and fail with the
//! same words.

use crate::spec::model::{self, Plot};

/// What the wrapper puts in front of the cell's lines: a query block for the
/// plot to name, standing for the data cell, and the plot block's opening.
/// The query's SQL is never run — the cell's own result is what is drawn.
const QUERY: &str = "query \"data\" {\n  sql = \"SELECT 1\"\n}\n";

/// A plot cell, read.
#[derive(Debug, Clone)]
pub(crate) struct PlotCell {
    /// The SQL cell `data` names; `None` is the nearest one above.
    pub data: Option<String>,
    /// The plot, or what is wrong with the cell, one message a line, each
    /// with the cell's own line number.
    pub plot: Result<Plot, Vec<String>>,
}

/// Read a plot cell's text. `key` makes the plot's name — which the chart
/// keys its hover state on — unique among the window's plots.
pub(crate) fn read(source: &str, key: u64) -> PlotCell {
    let mut data = None;
    let mut kept = Vec::new();
    let mut text_panel = false;
    for line in source.lines() {
        match line.split_once('=') {
            Some((name, value)) if name.trim() == "data" => {
                // `data = sales`, `data = "sales"`, or as a `.dash` query
                // reference is written, `data = query.sales`.
                let value = value.trim().trim_matches('"');
                let value = value.strip_prefix("query.").unwrap_or(value);
                data = Some(value.to_string());
                // Kept as a blank so line numbers still match the cell's.
                kept.push("");
            }
            Some((name, value)) if name.trim() == "type" => {
                text_panel = value.trim().trim_matches('"') == "text";
                kept.push(line);
            }
            _ => kept.push(line),
        }
    }
    if let Some(name) = &data {
        if !super::is_cell_name(name) {
            return PlotCell {
                plot: Err(vec![crate::i18n::trf("notebook.plot.bad_data", &[name])]),
                data,
            };
        }
    }
    // A text panel draws no query; every other type draws the data cell.
    let query = if text_panel {
        ""
    } else {
        "  query = query.data\n"
    };
    let head = format!("{QUERY}plot \"cell_{key}\" {{\n{query}");
    let offset = head.lines().count();
    let wrapped = format!("{head}{}\n}}\n", kept.join("\n"));
    let at = |line: usize, message: &str| {
        // A line inside the cell is numbered as the cell numbers it; one in
        // the wrapper (a missing `}`, say) is said without a number.
        match line
            .checked_sub(offset)
            .filter(|line| *line >= 1 && *line <= kept.len())
        {
            Some(line) => crate::i18n::trf("notebook.plot.at_line", &[&line.to_string(), message]),
            None => message.to_string(),
        }
    };
    let file = match model::parse(&wrapped) {
        Ok(file) => file,
        Err(error) => {
            return PlotCell {
                data,
                plot: Err(vec![at(error.line, &error.message)]),
            }
        }
    };
    let (mut spec, diagnostics) = model::validate_partial(&file);
    let plot = if !diagnostics.is_empty() {
        Err(diagnostics
            .iter()
            .map(|diagnostic| at(diagnostic.line, &diagnostic.message))
            .collect())
    } else if spec.plots.len() == 1 {
        let plot = spec.plots.remove(0);
        if plot.kind == "table" {
            Err(vec![crate::i18n::tr("notebook.plot.table").to_string()])
        } else {
            Ok(plot)
        }
    } else {
        Err(vec![crate::i18n::tr("notebook.plot.empty").to_string()])
    };
    PlotCell { data, plot }
}

/// A new plot cell's text, for a result with `columns`: its first column
/// across and its first number up, as a bar — something to see at once and
/// edit from. `None` without a result to look at.
pub(crate) fn starter(columns: &[(String, bool)]) -> String {
    let x = columns.first().map(|(name, _)| name.as_str());
    let y = columns
        .iter()
        .skip(1)
        .find(|(_, numeric)| *numeric)
        .map(|(name, _)| name.as_str());
    match (x, y) {
        (Some(x), Some(y)) => format!("type = \"bar\"\nx = \"{x}\"\ny = \"{y}\""),
        (Some(x), None) => format!("type = \"bar\"\nx = \"{x}\""),
        _ => "type = \"bar\"\nx = \"\"\ny = \"\"".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plot_cell_reads_as_a_dash_plot_with_its_data() {
        let cell = read(
            "type = \"line\"\ndata = sales\nx = \"day\"\ny = \"revenue\"",
            7,
        );
        assert_eq!(cell.data.as_deref(), Some("sales"));
        let plot = cell.plot.unwrap();
        assert_eq!(plot.kind, "line");
        assert_eq!(plot.x, "day");
        assert_eq!(plot.y.as_deref(), Some("revenue"));
        assert_eq!(plot.name, "cell_7");
    }

    #[test]
    fn data_may_be_quoted_or_a_query_reference_or_left_out() {
        assert_eq!(read("data = \"s\"", 1).data.as_deref(), Some("s"));
        assert_eq!(read("data = query.s", 1).data.as_deref(), Some("s"));
        assert_eq!(read("type = \"bar\"\nx = a\ny = b", 1).data, None);
        assert!(read("data = \"a b\"", 1).plot.is_err());
    }

    #[test]
    fn a_mistake_is_said_as_dash_says_it_at_the_cells_own_line() {
        let errors = read("type = \"bar\"\nx = \"a\"\ny = \"b\"\ncolour = \"red\"", 1)
            .plot
            .unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains('4'), "{errors:?}");
        let errors = read("type = \"pyramid\"\nx = \"a\"", 1).plot.unwrap_err();
        assert!(errors[0].contains("pyramid"), "{errors:?}");
    }

    #[test]
    fn a_text_panel_needs_no_data_and_a_table_is_the_cells_own() {
        let plot = read("type = \"text\"\ntitle = \"Note\"\ntext = \"Hi\"", 1)
            .plot
            .unwrap();
        assert_eq!(plot.kind, "text");
        assert!(read("type = \"table\"", 1).plot.is_err());
    }

    #[test]
    fn a_starter_plots_the_first_column_by_the_first_number() {
        let columns = [
            ("channel".to_string(), false),
            ("region".to_string(), false),
            ("revenue".to_string(), true),
        ];
        assert_eq!(
            starter(&columns),
            "type = \"bar\"\nx = \"channel\"\ny = \"revenue\""
        );
    }
}
