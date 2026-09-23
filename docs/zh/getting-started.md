# 快速上手

**[English](../getting-started.md)** · [文档](index.md)

DuckLocal 是一个桌面应用，让你直接用 SQL 查询本机上的数据文件。不需要启动服务器、不需要配置连接，也不需要注册账号——指向文件，就能开始查询。本页带你从下载走到第一条查询结果，大约五分钟。

## 1. 安装 {#install}

需要 **macOS 12 及以上、Apple silicon 芯片**（M1 或更新）。

1. 从最新版本下载 [`ducklocal-macos-arm64.dmg`](https://github.com/JetSquirrel/DuckLocal/releases/latest)。
2. 打开磁盘映像，把 **DuckLocal** 拖进“应用程序”。
3. 从“应用程序”或聚焦搜索启动 DuckLocal。

磁盘映像已经过 Apple 签名和公证，打开时不需要任何绕过 Gatekeeper 的操作。

::: details 改为从源码构建
需要 Rust stable 1.85.1 或更新版本，以及 C++ 工具链（Xcode Command Line Tools）。DuckDB 由内置源码编译，首次构建需要几分钟，并占用几 GB 磁盘空间。

```bash
git clone https://github.com/JetSquirrel/DuckLocal
cd DuckLocal
cargo build --release
./target/release/ducklocal
```

可执行文件是 `./target/release/ducklocal`，本指南中写 `ducklocal` 的地方都可以用它代替。打包 `.app` 的方法见[开发指南](development.md)。
:::

## 2. 添加 `ducklocal` 命令（可选） {#add-command}

应用里的所有功能都不需要终端。如果你还想从命令行打开数据，或者让 AI agent 通过 [CLI](cli.md) 执行查询，就把应用内的可执行文件链接到 `PATH` 上：

```bash
sudo mkdir -p /usr/local/bin
sudo ln -sf /Applications/DuckLocal.app/Contents/MacOS/ducklocal /usr/local/bin/ducklocal
ducklocal --version
```

这个链接指向应用本身，所以在“应用程序”里更新 DuckLocal 后，命令也随之更新。要删除它：`sudo rm /usr/local/bin/ducklocal`。

## 3. 打开数据

首次启动会显示一个标题为 **把数据拖进来** 的界面。以下任意一种方式都能导入数据：

- 把 CSV、TSV、Parquet、JSON 或 Excel 文件——或整个文件夹——**拖到窗口上**。
- 点击 **打开文件…** 或 **打开文件夹…**。
- 在终端里写出路径：

  ```bash
  ducklocal ./sales.csv         # 单个文件
  ducklocal ./logs/             # 递归打开文件夹下的所有数据文件
  ducklocal './data/*.parquet'  # 通配符——加引号，避免被 shell 提前展开
  ducklocal warehouse.duckdb    # 已有的 DuckDB 数据库
  ```

每个文件都会成为一个以文件名命名的视图——`sales.csv` 变成 `sales`——并连同列和行数一起出现在侧边栏的 **本地文件** 下。DuckLocal 在原位置读取文件，不复制，也不上传。

打开过的文件会被记住，下次启动时就是同一个工作区。格式、文件夹、通配符和数据库的细节见[数据源](data-sources.md)。

::: tip 手头没有数据？
[10 分钟上手教程](tutorial.md)会用一个小样例文件一步步带你走一遍。
:::

## 4. 运行查询

点击 **新建查询**（或者直接打开数据——编辑器会自动出现）。输入 SQL，按 **⌘↵**（Cmd+Enter）：

```sql
SELECT * FROM sales LIMIT 20;
```

结果出现在编辑器下方的面板里。切换到 **图表** 可以快速看个大概，也可以导出为 CSV 或 Parquet。编辑器上方的工具栏有 **运行**、**格式化** 和 **EXPLAIN**。

一个值得知道的捷径：在侧边栏里，点击表旁边的播放按钮会生成现成的 `SELECT * … LIMIT 100`，点击列名则只查询这一列。

## 接下来

- [10 分钟上手教程](tutorial.md) —— 用样例数据走一遍
- [数据源](data-sources.md) —— 格式、文件夹、通配符、数据库
- [SQL 编辑器](sql-editor.md) —— Tab、快捷键、自动补全、EXPLAIN
- [结果与图表](results-and-charts.md) —— 过滤、导出、图表
- [常见问题排查](troubleshooting.md) —— 遇到不对劲的地方时
