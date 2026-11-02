# Dashboard quality audit: findings

The results of the audit specified in [dashboard-quality-audit.md](dashboard-quality-audit.md), against `main` at `cac9c31`.

The window itself has not been looked at: everything below comes from reading the code, unit tests and paints in a test window. [Check by eye](#check-by-eye) lists what still needs a person.

## Fixed

| Severity | Behavior | Before | After | Test |
| --- | --- | --- | --- | --- |
| P0 | Uneven times on a `line` or `scatter` | Evenly spaced: one day and eight days looked alike | Placed by time, in time order, in dashboards and the results chart. TIMESTAMP in every precision, TIMESTAMPTZ, DATE and TIME all parse | `a_line_over_times_spaces_its_points_by_the_time_between_them`, `every_time_duckdb_renders_has_a_place_on_the_axis` |
| P0 | NULL reading on a dashboard `line` | The time vanished and the line ran across it | The time stays and the line breaks there, with a note. A lone reading shows as a dot | `a_null_reading_keeps_its_time_and_breaks_the_line`, `a_line_with_breaks_on_a_time_axis_paints` |
| P0 | Picking SQL NULL | Also matched the text `'NULL'` | Each selects its own rows. A result records which cells hold that text | `a_picked_null_and_the_text_null_select_their_own_rows`, `a_result_says_which_cells_are_sql_null` |
| P0 | Query cut short at the row or cell budget | Only tables and histograms said so; a pie's shares looked complete | Every dashboard chart says so | `every_chart_says_when_its_query_was_cut_short`, on a real query over the budget |
| P1 | A series vanishing under a filter | The ones after it changed color | A series keeps its color by name, also when it comes back. A new one takes an unused color | `a_series_keeps_its_color_while_others_come_and_go` |
| P1 | NaN or infinity among the numbers | One made the whole average NaN, with no note | Left out. Dashboards count them in a note; the results chart drops the row without one | `nan_and_infinity_do_not_reach_a_chart_and_are_counted`, `nan_and_infinity_leave_a_results_chart_row_out` |
| P1 | Series cap note | Counted rows: 13 series read as 112 | Counts series | `the_series_cap_counts_series_not_rows` |
| P1 | `open --state` | Source row count only | Each plot adds `scope`: `source_rows`, `query_truncated`, `plotted_points`, `series`, `x_scale` and the chart's `notice`. `rows` stays, so the schema version doesn't change | `the_state_says_what_a_plot_draws_of_its_query` |
| P1 | Area over uneven times | Evenly spaced, with nothing said | Still evenly spaced in dashboards, but a note says so. The kit's area chart can't place points by time | `an_area_over_uneven_times_says_it_is_evenly_spaced` |

## Behavior changes to review

- A `line` over times draws in time order, whatever the query's `ORDER BY`.
- A row with a NULL time is left off a time axis, with a note.
- A line breaks only at a NULL the query returned. Where a series has no row at all it still crosses, because breaking there would shatter a series that only has rows at some times. The wider spacing shows that gap.
- A dashboard that picks `NULL` on a column holding the text `'NULL'` used to get both, and now gets one.

## Already correct

Each of these was confirmed by its existing tests, and nothing changed.

| Behavior | Evidence |
| --- | --- |
| Missing days on a bar over dates are empty, not zero | `a_bar_over_dates_keeps_the_quiet_days` |
| Stacks add each side of zero separately | `a_stack_adds_up_each_side_of_zero_and_says_its_total` |
| A missing heatmap cell is empty, not zero | `a_heatmap_leaves_a_missing_cell_empty_and_caps_its_columns` |
| A truncated results-chart time series is redrawn from every row | `a_truncated_time_series_is_redrawn_from_every_row` |
| Dashboard states stay distinct: an empty result isn't a failure | New test `an_empty_result_is_not_a_failure_and_a_failure_is_not_empty` |
| Local use stays off the network, with the online base map off by default | `tests/network_paths.rs`, `scripts/check-offline.sh` in CI |

## Performance

Chart paint probes, 60 frames in a debug build. A release build's numbers will differ.

| Probe | `main` | This branch |
| --- | --- | --- |
| I1 line 12 × 1500 | 790 ms | 788 ms |
| I2 scatter 12 × 1500 | 495 ms | 494 ms |
| I3 area 12 × 1500 | 860 ms | 849 ms |
| I4 bars 200 × 3 | 52 ms | 53 ms |
| I5 map 20k points | 352 ms | 354 ms |

Allocations are unchanged. These probes don't use a time axis or NULL breaks, so those paths have no baseline. Each stroke between NULLs is its own path, and hovering a time axis scans its points, at most the chart's point cap.

## Not fixed: follow-ups

| Severity | Gap | Where |
| --- | --- | --- |
| P0 | Area charts stay evenly spaced and don't break at NULL. They read missing cells as `values[s]`, so a series with no row at some time draws a zero there. Needs hand-built area drawing | `src/spec/view.rs` `"area"` |
| P1 | Results chart: no even-spacing note on an area, and a NULL in one column drops the whole row without a note | `src/ui/chart.rs` `collect_plot_rows` |
| P1 | Bars and heatmaps over times that aren't whole days are evenly spaced with no note | `src/spec/plot.rs` |
| P1 | A chart draws NULL and the text `'NULL'` as one band, and a click on it picks the first row's kind. Only a table tells them apart | `src/spec/view.rs` `pick_row` |
| P1 | A card's `tone` is shown by color alone, with no word for it | `src/spec/view.rs`, the card's `tone` |
| P1 | Colors are stable within a plot, not between plots | `src/spec/prepare.rs` |
| P2 | Legends can't be clicked to filter, except a pie's | `src/ui/chart.rs` |

## Interactions compared with Dynatrace

P2, and nothing was implemented. DuckLocal charts hover, pick by clicking, clear picks and maximize. [Dynatrace's model](https://developer.dynatrace.com/design/data-visualizations/charts/ChartInteractions/) adds drag-to-zoom, wheel zoom, panning, selecting a range and reset, each with keyboard shortcuts.

Zoom on a time axis is the one worth doing next, now that the axis is real, since a dashboard otherwise needs its SQL edited to look closer. It is too large for this PR and deserves its own issue.

## Check by eye

Not covered by the tests:

1. A `line` over uneven dates: the spacing looks right, date labels don't collide, and hovering finds the point under the pointer, on default and largest text size, light and dark.
2. A `line` with a NULL: the break shows, a lone reading shows as a dot, and the note reads properly under the title.
3. Click a table cell showing NULL, in a column that also holds the text `'NULL'`: the dashboard narrows to the right rows.
4. Pick a filter that empties the first series of a multi-series bar and line: the rest keep their colors, including in the legend and tooltip.
5. A long or Chinese label in a tooltip and a legend; resize and maximize a chart while a filter is on.
6. A map with the online base map off and no network.
7. `ducklocal open --state` on a dashboard with a thinned scatter: `scope` matches the note on screen.
