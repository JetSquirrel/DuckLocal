# DuckLocal

<img src="assets/logo.png" width="128" alt="DuckLocal logo">

**[English](README.md)**

本地 · 快速 · 简单的 DuckDB 桌面客户端，基于 GPUI Kit 构建。

![DuckLocal 介绍](assets/intro.png)

## 功能

- SQL 编辑器：语法高亮、自动补全、一键格式化，支持多查询 Tab
- ⌘↵（Cmd+Enter）运行查询，EXPLAIN 查看查询计划
- Schema 侧栏：浏览数据库 / schema / 表 / 列，一键生成 SELECT 查询，可在对话框中修改列的数据类型
- 将本地 CSV/TSV/Parquet/JSON 文件挂载为可查询的视图；可选 S3 支持（httpfs）
- 结果表格支持筛选、单元格复制、CSV/Parquet 导出和内置图表
- 查询历史，单击回填编辑器
- 明暗主题切换

## 运行

```bash
cargo run
```

## 打包 macOS 应用

```bash
./scripts/bundle.sh
# 生成 target/release/DuckLocal.app
```

## 开源协议

[Apache-2.0](LICENSE) · Copyright © 2026 JetSquirrel
