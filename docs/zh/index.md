---
layout: home
hero:
  name: DuckLocal
  text: 你的数据，你的工作台。
  tagline: 本地优先的数据查询与分析工作台，原生基于 DuckDB。
  image:
    src: /assets/logo.png
    alt: DuckLocal
  actions:
    - theme: brand
      text: 快速上手
      link: /zh/getting-started
    - theme: alt
      text: 下载 macOS 版
      link: https://github.com/JetSquirrel/DuckLocal/releases/latest
    - theme: alt
      text: GitHub
      link: https://github.com/JetSquirrel/DuckLocal
features:
  - title: 查询本地文件
    details: 将 CSV、Parquet、JSON 或整个文件夹打开为 DuckDB 视图。不用配置连接，也不上传数据。
    link: /zh/data-sources
    linkText: 探索数据源
  - title: 专注的 SQL 工作区
    details: 语法高亮、自动补全、格式化和多个查询 Tab，让你专注于 SQL。
    link: /zh/sql-editor
    linkText: 了解编辑器
  - title: 探索查询结果
    details: 筛选结果表格、复制单元格、导出 CSV 或 Parquet，并用内置图表探索数据。
    link: /zh/results-and-charts
    linkText: 结果与图表
---

# DuckLocal

**[English](../index.md)** · [项目 README](https://github.com/JetSquirrel/DuckLocal/blob/main/README.zh-CN.md)

本地优先的数据查询与分析工作台，原生基于 DuckDB。
直接指向你的文件即可——不用配置连接、不用建 schema，也不会上传任何数据。

要求 macOS 12 及以上、Apple 芯片。开源免费，Apache-2.0。
[下载 macOS 版](https://github.com/JetSquirrel/DuckLocal/releases/latest) ·
[在 GitHub 上查看](https://github.com/JetSquirrel/DuckLocal)

![DuckLocal 工作原理：CSV、Parquet、DuckDB 文件与 S3 兼容对象存储汇入同一个本地工作区](../assets/intro.jpg)

## 指南

| 指南 | 内容 |
| --- | --- |
| [快速上手](getting-started.md) | 安装 DuckLocal、打开你的第一批文件、运行第一条查询 |
| [数据源](data-sources.md) | 支持哪些文件格式，文件夹与通配符如何解析，数据库如何打开，视图命名，以及各项限制 |
| [S3 与 httpfs](s3.md) | 让 DuckLocal 指向 S3 兼容的存储桶，浏览其中的内容并查询对象 |
| [SQL 编辑器](sql-editor.md) | 查询 Tab、运行与格式化 SQL、EXPLAIN，以及自动补全 |
| [Schema 浏览与历史](schema-and-history.md) | 浏览表与列、生成 SELECT 语句、修改列的数据类型，以及复用历史查询 |
| [结果与图表](results-and-charts.md) | 结果表格、筛选、复制、导出，以及内置图表 |
| [设置与应用数据](settings-and-data.md) | DuckLocal 把文件保存在哪里，重启后哪些内容仍在，主题、语言，以及疑难排查 |
| [AI CLI 与官方 skill](cli.md) | 无界面运行 SQL、了解 JSON 契约、安装官方 agent skill |
| [分析面板](analysis-panel.md) | 打开一个由你编写的 JavaScript 面板窗口，使用同一个连接 |
| [开发](development.md) | 构建、测试、打包 `.app`，以及发布已签名并公证的版本 |

## 一览

- 本地 CSV / TSV / Parquet / JSON 文件或整个文件夹，打开即为视图——入口可以是
  命令行、文件对话框，或把文件拖放到窗口
- SQL 编辑器：语法高亮、自动补全、格式化，支持多个查询 Tab
- Schema 侧栏：浏览数据库、schema、表与列
- 结果表格：支持筛选、单元格复制、CSV 与 Parquet 导出，以及图表
- 查询历史、可选的 S3 支持、明暗主题、English 与 简体中文
