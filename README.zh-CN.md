# DuckLocal

<img src="assets/logo.png" width="128" alt="DuckLocal logo">

**[文档](docs/zh/index.md)** · **[English](README.md)**

本地优先的数据查询与分析工作台，原生基于 DuckDB。直接指向你的文件即可——不用配置连接、不用建 schema，也不会上传任何数据。

![DuckLocal 介绍](assets/intro.png)

## 打开数据

```bash
ducklocal ./logs/             # 递归打开文件夹下的所有数据文件
ducklocal ./billing.parquet   # 单个文件
ducklocal './data/*.csv'      # 通配符
ducklocal warehouse.duckdb    # 或已有的 DuckDB 数据库
```

每个 CSV / TSV / Parquet / JSON 文件在窗口打开时就是一个可查询的视图。也可以把文件或文件夹拖进窗口，或用文件对话框选择；它们会被记住，下次启动直接回到同一个工作区。一次打开请求最多挂载 256 个文件。

数据只留在这台机器上：不上传、不需要账号。

## 功能

- 把本地 CSV / TSV / Parquet / JSON 文件或整个文件夹挂载为可查询视图：命令行、文件对话框、拖放三种入口
- SQL 编辑器：语法高亮、自动补全、一键格式化，支持多查询 Tab
- ⌘↵（Cmd+Enter）运行查询，EXPLAIN 查看查询计划
- Schema 侧栏：浏览数据库 / schema / 表 / 列，一键生成 SELECT 查询，可在对话框中修改列的数据类型
- 结果表格支持筛选、单元格复制、CSV/Parquet 导出和内置图表
- 查询历史，单击回填编辑器
- 可选 S3 支持（httpfs），凭据仅当前会话有效
- 明暗主题切换，中英文界面

## AI CLI 与官方 skill

无窗口执行一条 SQL，不读取 GUI 历史或设置：

```bash
ducklocal --help
ducklocal query --sql "DESCRIBE SELECT * FROM 'sales.csv'"
ducklocal query --sql "SELECT * FROM 'sales.csv'" --limit 20
ducklocal query --database warehouse.duckdb --sql "SHOW TABLES"
```

结果为结构化 JSON，显式标记截断并保留数值精度；`--format md` 把同样的值输出为 Markdown 表格，便于写进文档引用。文件数据库默认只读，写入需 `--read-write`；只读不是文件系统/网络沙箱，`COPY` 仍可写文件。

分析面板可以导出为一个独立 HTML 文件——面板 `query()` 发出的语句及其结果——方便发给没有 DuckLocal 的人：

```bash
ducklocal dash export --html examples/analysis_app
```

转换、stdin、输出编码、面板导出和安全说明见 [CLI 指南](docs/zh/cli.md)。

[官方 agent skill](skills/ducklocal/SKILL.md) 教 AI 先查 schema，再进行 SQL 分析及验证格式转换。复制到目标项目支持的 skill 目录即可，例如在本仓库执行：

```bash
mkdir -p /path/to/your-project/.claude/skills
cp -R skills/ducklocal /path/to/your-project/.claude/skills/
```

目标已存在时先检查，避免覆盖。不修改全局配置。当前发布范围为 macOS 12+、Apple 芯片；app 内二进制位于 `/Applications/DuckLocal.app/Contents/MacOS/ducklocal`。先检查 help/version 是否支持 CLI，或 `cargo build --locked` 后用 `./target/debug/ducklocal`。

## 文档

完整指南在 [`docs/`](docs/zh/index.md)：[快速上手](docs/zh/getting-started.md)、[数据源](docs/zh/data-sources.md)、[S3 与 httpfs](docs/zh/s3.md)、[SQL 编辑器](docs/zh/sql-editor.md)、[Schema 浏览与历史](docs/zh/schema-and-history.md)、[结果与图表](docs/zh/results-and-charts.md)、[设置与应用数据](docs/zh/settings-and-data.md)、[开发](docs/zh/development.md)。

这些页面同时会以 VitePress 站点形式发布。需要 Node.js 22 及以上（推荐 24 LTS）：`npm --prefix docs ci` 安装依赖，`npm --prefix docs run build` 生成到 `target/docs-site`，`npm --prefix docs run dev` 本地开发。CI 在 `main` 分支的 `docs/` 有改动时自动部署到 GitHub Pages。详见[开发](docs/zh/development.md#文档站)。

## 运行

```bash
cargo run                     # 启动空工作区
cargo run -- ./data/logs/     # 或直接打开数据
```

## 打包 macOS 应用

```bash
./scripts/bundle.sh
# 生成 target/release/DuckLocal.app
```

发布的安装包要求 macOS 12 及以上、Apple 芯片。`bundle.sh` 不做签名；正式发布走 `scripts/package-macos.sh`，它会签名并对 dmg 做公证。详见[开发](docs/zh/development.md)。

## 开源协议

[Apache-2.0](LICENSE) · Copyright © 2026 JetSquirrel
