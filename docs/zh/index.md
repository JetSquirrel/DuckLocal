---
layout: home
title: "DuckLocal — 在 Mac 上用 SQL 查询本地 CSV、Parquet 与 Excel 文件"
titleTemplate: false
description: "DuckLocal 是基于 DuckDB 的免费开源 Mac 应用：拖入 CSV、Parquet、JSON 或 Excel 文件，直接用 SQL 查询。不用服务器、不用配置、不用上传。"
hero:
  name: DuckLocal
  text: 在 Mac 上直接用 SQL 查你的文件。
  tagline: 拖入 CSV、Parquet、JSON 或 Excel，用 DuckDB 查询。不用服务器，不用配置，不用上传。
  image:
    src: /assets/logo.png
    alt: DuckLocal
  actions:
    - theme: brand
      text: 快速上手
      link: /zh/getting-started
    - theme: alt
      text: 10 分钟上手教程
      link: /zh/tutorial
    - theme: alt
      text: 下载 macOS 版
      link: https://github.com/JetSquirrel/DuckLocal/releases/latest
features:
  - title: 文件在哪，就在哪查
    details: 单个文件、文件夹、通配符或 .duckdb 数据库，一打开就能查询——下次启动还在。
    link: /zh/data-sources
    linkText: 数据源
  - title: 数据不离开你的电脑
    details: 没有账号、没有遥测、没有云端。文件原地读取，从不复制或上传。
    link: /zh/settings-and-data
    linkText: 存了什么、存在哪里
  - title: 专注的 SQL 工作区
    details: 多个查询 Tab，基于你自己表结构的自动补全，一键格式化与 EXPLAIN。
    link: /zh/sql-editor
    linkText: SQL 编辑器
  - title: 从数字到图表
    details: 过滤结果表格、把结果画成图，并导出为 CSV 或 Parquet。
    link: /zh/results-and-charts
    linkText: 结果与图表
  - title: 为 AI agent 而生
    details: 无界面 CLI 与严格的 JSON 契约，外加一个官方 agent skill，按先看 schema 再分析的方式工作。
    link: /zh/cli
    linkText: CLI 与 skill
  - title: Dashboard 即文件
    details: 在 .dash 文件里声明查询与图表，或用脚本写一个定制视图，再导出成一个 HTML 页面分享出去。
    link: /zh/dashboards
    linkText: Dashboard（.dash）
---

<p align="center"><strong><a href="../">English</a></strong> · macOS 12+，Apple silicon · 开源免费（Apache-2.0）</p>

## 三步上手

1. **[下载](https://github.com/JetSquirrel/DuckLocal/releases/latest)**磁盘映像，把 DuckLocal 拖进“应用程序”。
2. 把一个数据文件——或整个文件夹——**拖到窗口上**。每个文件都会成为一个以文件名命名的视图。
3. 对一条查询按 **⌘↵**：

```sql
SELECT channel, sum(revenue) AS revenue
FROM sales
GROUP BY channel
ORDER BY revenue DESC;
```

第一次用？[快速上手](getting-started.md)讲安装和首次启动；[10 分钟上手教程](tutorial.md)用一个样例文件带你走一遍。

![DuckLocal 工作原理：CSV、Parquet、DuckDB 文件与 S3 兼容对象存储汇入同一个本地工作区](../assets/intro.jpg)

## 按需查找

| 我想… | 请看 |
| --- | --- |
| 安装 DuckLocal 并跑第一条查询 | [快速上手](getting-started.md) |
| 用样例数据边做边学 | [10 分钟上手教程](tutorial.md) |
| 打开文件夹、通配符、Excel 或 `.duckdb` 文件 | [数据源](data-sources.md) |
| 查询 S3 或兼容存储里的对象 | [S3 与 httpfs](s3.md) |
| 用好编辑器 | [SQL 编辑器](sql-editor.md) · [Schema 浏览与历史](schema-and-history.md) |
| 把结果画成图或导出 | [结果与图表](results-and-charts.md) |
| 让 AI agent 查询我的数据 | [CLI 与 agent skill](cli.md) |
| 用保存的查询搭一个 dashboard | [Dashboard（.dash）](dashboards.md) |
| 用脚本写一个定制视图 | [分析应用](analysis-app.md) |
| 解决某个不正常的问题 | [常见问题排查](troubleshooting.md) |
| 从源码构建或参与开发 | [开发指南](development.md) |
