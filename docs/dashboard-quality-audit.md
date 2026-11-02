# Dashboard quality audit (Codex work package)

**Status:** task specification; **implementation, verification, and findings are assigned to Codex** via the pull-request conversation.

**Goal:** make DuckLocal dashboard charts trustworthy and efficient for real investigations. Prioritize accurate interpretation of data, clear states, and reproducible interactions—not adding more chart types or redesigning the app.

## Baseline: inspect before changing anything

DuckLocal already ships `.dash` charts (`line`, `bar`, `stacked_bar`, `area`, `scatter`, `pie`, `heatmap`, `histogram`, `map`), tables, cards, text, cross-filters, number formats, and `ducklocal check`. The results panel also has a Chart tab with column encoding and a Copy-as-`.dash` path.

Inspect these modules and their existing tests; confirm actual behavior before reporting a gap:

- `src/spec/prepare.rs` — cap/aggregate/resample/empty-band semantics and warnings.
- `src/spec/plot.rs`, `src/spec/view.rs` — axes, legends, tooltips, interaction, and dashboard states.
- `src/spec/filter.rs`, `src/spec/model.rs`, `src/spec/mod.rs` — filter binding, spec validation and execution.
- `src/ui/chart.rs`, `src/ui/results.rs`, `src/ui/encoding.rs`, `src/ui/geo.rs` — results-chart/data fidelity and map presentation.
- `src/spec/format.rs`, `src/ui/palette.rs` — units, deltas, palettes.
- `tests/cli.rs`, `examples/`, `src/perf_probe.rs` — existing regression coverage and performance probes.

**Don't reproduce existing features as new work.** First produce a short finding matrix: `behavior | current implementation | evidence/test | severity | needed change`.

## P0 — Data fidelity and truthful communication

1. **Correct subset/aggregation semantics.** Compare chart calculations with full DuckDB SQL aggregates when the results grid is limited, a series or category is capped, or points are thinned. Never imply a sampled/partial plot is a complete result. Where the current logic already discloses truncation, retain it and add tests.
2. **Temporal data.** Test irregular timestamps, long gaps, duplicates, non-uniform periods, timezone handling and null points. Do not invent continuity or hide missing intervals. Labels/hover values must reflect the actual temporal data—not just equidistant text buckets when a true time scale is required.
3. **Numeric data.** Test negative-only/mixed-sign values, true zero vs NULL/missing, extremely small and large values, all-identical values, percent-as-fraction vs percent-points, scientific notation, nonfinite inputs, and currency/delta signs. Check stacked bars and KPI `down_is_good` semantics.
4. **Consistent selection/filter semantics.** Clicking a chart, dashboard table or legend must affect the intended source rows, not accidentally select an aggregated/renamed label. Preserve the human's choice across rerender where appropriate, and verify `ducklocal check` detects invalid filter queries.
5. **Color and identity.** Preserve stable series↔color mapping across sort/refresh, light/dark themes and related plots. Use meaningfully labeled status tones; do not convey a critical state through color alone.

## P1 — Usability and accessible dashboard states

6. Distinguish **Loading / Empty / Error / Valid-but-truncated** in both the dashboard and results-chart paths; provide the next action for errors where known. An empty successful result is not a query failure.
7. Audit tooltips, legends, axes, overflow and long/CJK labels; ensure units and time ranges are explicit. Make underlying SQL, filters and source references inspectable so the user and an agent can explain what was drawn.
8. Verify hover/selection behavior on dense series, resize, maximize and cross-filter redraw. Keyboard affordances and focus should not regress; document interaction gaps.
9. Check representative map cases with the online basemap disabled by default; no unexpected network requirement for local maps.

## P2 — Interaction improvements, only when justified by findings

10. Compare current interactions with Dynatrace's **explore/zoom/pan/select/reset** model. Implement the smallest high-value change only if it solves a demonstrable investigation workflow gap; otherwise record a follow-up issue. Do not ship a new heavyweight chart framework.

## Required evidence / fixtures

Use small deterministic synthetic fixtures plus existing `examples/` (prefer extending tests rather than introducing new external dependencies). The matrix should include:

| Case | Expected property |
| --- | --- |
| Time series with a missing day and irregular intervals | Gap visible or explicitly indicated; axis truthful |
| 100k+ rows / more series than the cap | Full-data aggregation or explicit partial-result disclosure |
| All NULL, all zero, negative-only and very large/small numbers | Correct values, labels and safe rendering |
| Category labels with CJK / long text | Readable tooltips or accessible alternative; no wrong category mapping |
| Cross-filter from a bar/stack/table | Target SQL predicate matches the original selected value |
| Empty, SQL error, waiting-for-result states | Different meaningful UI states |
| Fast hover and 20k map / 12×1500 scatter | No serious regression against existing perf-probe baselines |
| Disconnected map tiles / disabled online basemap | Local chart behavior works without network |

## Codex deliverables

- [ ] Audit findings with **code references and verified reproductions**, ordered P0/P1/P2. Include items already correct; avoid speculative findings.
- [ ] Add or extend automated tests for every confirmed P0 behavior and important P1 states.
- [ ] Implement *small, scoped* correctness/UI fixes that the tests justify. No broad visual rewrite.
- [ ] Run `cargo fmt --check`, `cargo test --locked`, `cargo clippy --all-targets --locked` where environment allows; report exact failures/limitations.
- [ ] Add a concise before/after behavior summary, screenshots or a manual verification checklist for visual-only changes. Report platform limitations honestly.
- [ ] Separate larger chart interaction work into follow-up issues rather than expanding this PR indefinitely.

## Design references (principles, not code to copy)

- Dynatrace Strato data visualizations: https://developer.dynatrace.com/design/data-visualizations/
- Chart states: https://developer.dynatrace.com/design/data-visualizations/charts/ChartStates/
- Chart interactions: https://developer.dynatrace.com/design/data-visualizations/charts/ChartInteractions/
- Chart coloring: https://developer.dynatrace.com/design/data-visualizations/charts/ChartColoring/
- Cloudflare Analytics: https://developers.cloudflare.com/analytics/custom-dashboards/
- Supabase Explorer/Notebooks: https://supabase.com/blog/explorer-and-notebooks-investigations-your-team-can-rerun

## Scope and review policy

- **Owner:** Codex for inspection, tests and focused fixes; human maintainer reviews and merges.
- Do not introduce remote data upload, in-app LLM chat, new chart types, custom chart libraries or new dashboard DSL commands.
- Keep GUI and agent/CLI output aligned: what the chart shows and what `open --state`/`check` reports must not contradict each other.
- Link any findings that affect agent-generated `.dash` reviews to #64 / #65.
