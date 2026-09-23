---
layout: home
title: "DuckLocal — query local CSV, Parquet and Excel files with SQL on your Mac"
titleTemplate: false
description: "DuckLocal is a free, open-source Mac app built on DuckDB: drop in CSV, Parquet, JSON or Excel files and query them with SQL. No server, no setup, no upload."
hero:
  name: DuckLocal
  text: SQL on your files, right on your Mac.
  tagline: Drop in CSV, Parquet, JSON or Excel and query it with DuckDB. No server, no setup, no upload.
  image:
    src: /assets/logo.png
    alt: DuckLocal
  actions:
    - theme: brand
      text: Get started
      link: /getting-started
    - theme: alt
      text: Try the 10-minute tutorial
      link: /tutorial
    - theme: alt
      text: Download for macOS
      link: https://github.com/JetSquirrel/DuckLocal/releases/latest
features:
  - title: Query files where they are
    details: A file, a folder, a glob or a .duckdb database becomes queryable the moment you open it — and stays there next launch.
    link: /data-sources
    linkText: Data sources
  - title: Nothing leaves your machine
    details: No account, no telemetry, no cloud. Files are read in place and never copied or uploaded.
    link: /settings-and-data
    linkText: What is stored, and where
  - title: A focused SQL workspace
    details: Query tabs, autocompletion from your own tables, one-click formatting and EXPLAIN.
    link: /sql-editor
    linkText: The SQL editor
  - title: From rows to a picture
    details: Filter the grid, chart the result, and export it as CSV or Parquet.
    link: /results-and-charts
    linkText: Results and charts
  - title: Built for AI agents
    details: A headless CLI with a precise JSON contract, plus an official agent skill for schema-first analysis.
    link: /cli
    linkText: CLI and skill
  - title: Dashboards as files
    details: Declare queries and plots in a .dash file, or script a custom view and export it as one HTML page to share.
    link: /dashboards
    linkText: Dashboards (.dash)
---

<p align="center"><strong><a href="./zh/">中文文档</a></strong> · macOS 12+ on Apple silicon · Free and open source (Apache-2.0)</p>

## Up and running in three steps

1. **[Download](https://github.com/JetSquirrel/DuckLocal/releases/latest)**
   the disk image and drag DuckLocal to Applications.
2. **Drop a data file** — or a whole folder — onto the window. Each file
   becomes a view named after it.
3. **Press ⌘↵** on a query:

```sql
SELECT channel, sum(revenue) AS revenue
FROM sales
GROUP BY channel
ORDER BY revenue DESC;
```

New here? [Getting started](getting-started.md) covers installation and the
first launch; [Your first 10 minutes](tutorial.md) is a guided tour with a
sample file.

![How DuckLocal works: CSV, Parquet and DuckDB files, plus S3-compatible object storage, all feeding one local workspace](assets/intro.jpg)

## Find your way

| I want to… | Read |
| --- | --- |
| Install DuckLocal and run a first query | [Getting started](getting-started.md) |
| Learn by doing, with sample data | [Your first 10 minutes](tutorial.md) |
| Open folders, globs, Excel or a `.duckdb` file | [Data sources](data-sources.md) |
| Query objects in S3 or a compatible store | [S3 and httpfs](s3.md) |
| Get more out of the editor | [SQL editor](sql-editor.md) · [Schema and history](schema-and-history.md) |
| Chart or export a result | [Results and charts](results-and-charts.md) |
| Let an AI agent query my data | [CLI and agent skill](cli.md) |
| Build a dashboard from saved queries | [Dashboards (.dash)](dashboards.md) |
| Script a custom view over my data | [Analysis apps](analysis-app.md) |
| Fix something that is not working | [Troubleshooting](troubleshooting.md) |
| Build DuckLocal from source or contribute | [Development](development.md) |
