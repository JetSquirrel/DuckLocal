# DuckLocal

<img src="assets/logo.png" width="128" alt="DuckLocal logo">

**[ducklocal.app](https://ducklocal.app/zh/)** · **[文档](https://ducklocal.app/docs/zh/)** · **[English](README.md)**

本地优先的数据查询与分析工作台，原生基于 DuckDB。直接指向你的文件即可——不用配置连接、不用建 schema，也不会上传任何数据。

![DuckLocal 介绍](assets/intro.png)

## 打开数据

```bash
ducklocal ./logs/             # 递归打开文件夹下的所有数据文件
ducklocal ./billing.parquet   # 单个文件
ducklocal './data/*.csv'      # 通配符
ducklocal warehouse.duckdb    # 或已有的 DuckDB 数据库
```

CSV / TSV / Parquet / JSON / Excel 文件在窗口打开时即可查询。也可以拖进窗口或用文件对话框选择；工作区会被记住，下次启动直接恢复。

数据只留在这台机器上，不需要账号。仅有的可选联网是地图图表的 OpenStreetMap 底图（「视图 → 在线底图」，默认关闭），以及你主动配置的 S3。

## 功能

- SQL 编辑器：语法高亮、自动补全、格式化、多 Tab；macOS 使用 Cmd+Enter、Windows/Linux 使用 Ctrl+Enter 运行，EXPLAIN 查看计划
- Schema 侧栏：浏览表和列、一键生成 SELECT、修改列类型
- 结果表格：筛选、复制、CSV/Parquet 导出、内置图表
- `.dash` 文件定义的 Dashboard，支持交叉筛选
- 查询历史；通过 httpfs 支持 S3（凭据仅当前会话有效）
- 明暗主题、四档界面大小、中英文界面

## 面向 Agent 的 CLI

同一个二进制可以无窗口运行并输出结构化 JSON，AI agent 不用打开界面就能探索数据：

```bash
ducklocal schema ./data/                                  # 列出可查询的内容及列
ducklocal query --sql "SELECT * FROM 'sales.csv'" --limit 20
ducklocal check dashboard.dash                            # 校验 dashboard 文件
ducklocal open --run --sql "SELECT ..."                   # 把结果交给正在运行的窗口
ducklocal open --state                                    # 读取窗口当前显示的内容
```

数据库文件默认只读，写入需加 `--read-write`。全部命令和参数见 [CLI 指南](https://ducklocal.app/docs/zh/cli)。

[官方 agent skill](skills/ducklocal/SKILL.md) 教 AI 先查 schema，再做 SQL 分析和格式转换。复制到你的项目即可：

```bash
mkdir -p /path/to/your-project/.claude/skills
cp -R skills/ducklocal /path/to/your-project/.claude/skills/
```

## 文档

使用指南、上手教程和问题排查见 **[ducklocal.app/docs](https://ducklocal.app/docs/zh/)**（源码：[JetSquirrel/ducklocal-site](https://github.com/JetSquirrel/ducklocal-site)）。

## 开发

```bash
cargo run                     # 启动空工作区
cargo run -- ./data/logs/     # 或直接打开数据
./scripts/bundle.sh           # 生成 target/release/DuckLocal.app（未签名）
```

发布打包支持 macOS 12+（Apple 芯片）、Linux x86_64 和 Windows x86_64；`scripts/package-macos.sh` 生成签名并公证的 dmg。详见[开发文档](https://ducklocal.app/docs/zh/development)。

### Windows

```powershell
cargo build --release --locked
python scripts/check-windows.py target/release/ducklocal.exe
```

生成 `target/release/ducklocal.exe`：双击打开 GUI 时不创建控制台黑框，EXE 和任务栏使用内嵌的多尺寸鸭子图标。CLI 会连接调用方的控制台，保留重定向、管道和 LSP 的标准输入输出。PowerShell 中需要等待完成或获取退出码时，可使用 `Start-Process -Wait -PassThru`；捕获 JSON 时可以使用管道，如 `./ducklocal.exe query --sql "SELECT 42" | Out-String`。

体积优先的可选构建：`cargo build --profile compact --locked`，输出到 `target/compact/ducklocal.exe`。该配置对 Rust 代码使用 `s` 优化，同时保持 DuckDB C++ 引擎的 `opt-level=3`、LTO 和符号剥离。默认 release 使用完整的性能优化；两种配置的实际大小和速度需在目标设备上比较。

重新生成 Windows 图标：安装 Pillow 后运行 `python scripts/make-icons.py --windows`。正常构建使用已提交的 ICO，不需要 Python 或 Pillow；MSVC 构建需要 Visual Studio C++ 工具和 Windows SDK。

性能探针：`cargo test --release perf_g_ -- --nocapture --test-threads=1` 比较分类图表分配，`perf_h_` 比较长文本自动列宽；统计包含基线与优化后的时间、分配次数及字节数。

## 开源协议

[Apache-2.0](LICENSE) · Copyright © 2026 JetSquirrel
