# 快速上手

**[English](../getting-started.md)** · [文档](index.md)

## 环境要求

发布的构建版运行在 **macOS 12.0 及以上，仅支持 Apple silicon**。分发的磁盘映像已签名并公证，因此无需绕过 Gatekeeper 就能打开。

从源码构建需要 Rust stable，**1.85.1 或更新版本**，以及 C++ 工具链——DuckDB 由内置源码编译而来。首次构建要花一段时间，并占用几 GB 磁盘空间。

## 安装

**从磁盘映像安装。** 下载 `ducklocal-macos-arm64.dmg`，打开它，把 `DuckLocal.app` 拖到“应用程序”。

**从源码安装。**

```bash
git clone https://github.com/JetSquirrel/DuckLocal
cd DuckLocal
cargo build --release
./target/release/ducklocal
```

`cargo run` 也可以，迭代更快。因为有 DuckDB，首次构建很慢；之后只有 DuckLocal 自身会重新编译。

## 打开数据

DuckLocal 会打开你在命令行里指定的任何内容：

```bash
ducklocal ./logs/             # 递归打开文件夹下的所有数据文件
ducklocal ./billing.parquet   # 单个文件
ducklocal './data/*.csv'      # 通配符（加引号，避免被 shell 提前展开）
ducklocal warehouse.duckdb    # 或已有的 DuckDB 数据库
```

你可以一次指定多个路径，并且自由混合。

还有两种入口，二者等价：

- 标题栏中的 **打开数据…** 会打开一个对话框，你可以在其中输入或浏览路径。该对话框里的 **内存模式** 按钮会切换到一个干净的内存工作区，不挂载任何内容。
- **把文件或文件夹拖到窗口上。**

每个 CSV、TSV、Parquet、JSON 或 Excel 文件都会成为一个可查询的关系，每个文件都会被登记——下次启动时就是同一个工作区。详见[数据源](data-sources.md)。

## 运行查询

工作区打开时带有一个查询 Tab。输入 SQL 并按 **⌘↵**（Cmd+Enter）运行。**运行**、**格式化** 和 **EXPLAIN** 位于编辑器上方的工具栏中。

结果出现在下方的面板里，以表格或图表的形式呈现。

## 首次运行界面

当没有任何内容被挂载、也没有任何内容被登记时，工作区会显示三个按钮而不是编辑器：**打开文件…**、**打开文件夹…** 和 **新建查询**。选择前两个之一来挂载数据，或者选 **新建查询** 直接得到一个编辑器。

有一个小特性值得知道：在这个界面上，**⌘↵** 会显示编辑器，而不是运行任何东西。

## 接下来

- [数据源](data-sources.md) —— 格式、文件夹、通配符、数据库
- [SQL 编辑器](sql-editor.md) —— Tab、快捷键、EXPLAIN
- [结果与图表](results-and-charts.md) —— 你可以对结果做什么
- [设置与应用数据](settings-and-data.md) —— 所有内容的存放位置
