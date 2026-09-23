---
description: "用 ducklocal CLI 无界面执行 DuckDB SQL：结构化 JSON 输出、Markdown 表格、数据概览、文件转换、dashboard 校验与官方 AI agent skill。"
---

# AI CLI 与官方 skill

**[English](../cli.md)** · [文档](index.md)

## 无窗口运行

`ducklocal query` 执行 SQL，不初始化 GUI、历史、注册文件、语言设置或 GUI 全局连接。每次调用创建独立连接。无参数或文件/目录/glob 参数仍打开原有 GUI；名为 `query`、`profile`、`export`、`check`、`dash` 的 GUI 路径请写 `./query`、`./profile`、`./export`、`./check`、`./dash`。

`ducklocal export` 是唯一会启动窗口平台的子命令：分析应用要渲染，渲染需要窗口。它打开一个隐藏窗口、绘制一帧，在应用不再查询后退出，屏幕上不会出现任何东西。

当前发布范围仍是 **macOS 12 及以上、Apple 芯片**，不依赖额外 DuckDB CLI。包含此功能的 app 可直接调用内部二进制：

```bash
/Applications/DuckLocal.app/Contents/MacOS/ducklocal --help
/Applications/DuckLocal.app/Contents/MacOS/ducklocal --version
```

也可从本仓库 `cargo build --locked`，运行 `./target/debug/ducklocal`。以下示例假定该二进制在 PATH 中，名称为 `ducklocal`。旧版可能没有 `query`，先检查 help。JSON 和 Parquet 已编入二进制，不在查询时下载。

## 查询选项

```bash
ducklocal --help
ducklocal --version
ducklocal query --help
ducklocal query --sql "SELECT 1 AS n"
ducklocal query --sql "SELECT 1 AS n" --format md
ducklocal query --sql-file analysis.sql --limit 100
ducklocal query --sql-file - < analysis.sql
ducklocal query --database warehouse.duckdb --sql "SHOW TABLES"
```

- 必须且只能提供 `--sql SQL` 或 `--sql-file FILE`。文件必须为 UTF-8；`-` 从 stdin 读取。每次只允许一条 SQL。打开目标数据库或执行操作前用 DuckDB 的解析器验证；支持注释、引号中的分号、尾部分号，不会只执行脚本的一部分。
- `--database PATH` 默认以**只读**打开**已有文件**；省略则用新的内存数据库。`--read-write` 必须搭配 `--database`，显式允许写入/创建数据库，不创建父目录。打开失败不回退到内存。
- `--limit N` 为正整数，默认 1000，限制返回行数；另有 2,000,000 个单元格的预算。额外读取一行判断 `truncated`。这不限制计算量或 DuckDB 内部结果缓冲；单个大单元格仍可能占用很多内存。
- `--format json`（默认）是下面的契约；`--format md` 把同一份结果渲染成 Markdown 表格，供文档或 agent 阅读。其余行为完全一致：同样的 SQL、同样的限制、同样的错误。
- 重复/未知参数、缺失值、空 SQL、非法 limit 均报错。参数和值分开，不支持 `--flag=value`。相对路径基于进程工作目录，不是 SQL 文件目录。Shell 路径需引号；SQL 路径中的单引号双写（`'O''Brien.csv'`）；SQL 标识符用双引号。

**数据库只读不是文件系统或网络沙箱。** `COPY` 仍可写文件。执行前确认输出路径、覆盖文件、数据库修改、安装扩展和外部访问都符合用户授权。不自动安装扩展，不自动重试；授权后仍可显式执行 `INSTALL`/`LOAD`，已有扩展可能自动加载。失败命令也可能已经产生副作用（例如输出失败前已写入），重试前检查目标。

### Markdown 输出

`--format md` 是一种渲染，不是第二份契约：值就是 JSON 里的那些值，按结果表格展示它们的方式写出。

| 值 | 渲染结果 |
| --- | --- |
| `NULL` | `NULL` |
| 整数、DECIMAL、浮点 | 值本身携带的全部数字——`DECIMAL(38,10)` 保留其标度，`NaN`/`inf`/`-inf` 原样写出 |
| `DATE` | `YYYY-MM-DD` |
| `TIMESTAMP` | `YYYY-MM-DD HH:MM:SS[.ffffff]`，只在确有小数时带小数。不附加时区；原始计数在 `--format json` 里 |
| `TIME` | `HH:MM:SS[.ffffff]` |
| `INTERVAL` | `1 months 2 days 3000 ns` |
| `BLOB`、`GEOMETRY` | `0x00ff`，过长时以 `…` 省略 |
| `LIST`、`STRUCT`、`MAP` | `[1, 2]`、`{x: 1}`，超过 120 字符省略 |

单元格里的 `|` 会被转义、换行变成 `<br>`，所以值不会破坏所在表格。零行结果后跟 `_0 rows._`；被 `--limit` 或单元格预算截断的结果后跟 `_Truncated at N rows: …_`——预览不该读起来像完整答案。错误行为不变：JSON 写到 stderr，stdout 为空。

## 探查与转换

确认文件实际存在后：

```bash
ducklocal query --sql "DESCRIBE SELECT * FROM 'sales.csv'"
ducklocal query --sql "SELECT * FROM 'sales.csv'" --limit 20
ducklocal query --sql "SELECT sum(amount) AS total FROM 'sales.csv'"
# 仅在确认此输出路径已获授权后运行：
ducklocal query --sql "COPY (SELECT * FROM 'sales.csv') TO 'sales.parquet' (FORMAT PARQUET)"
ducklocal query --sql "SELECT count(*), sum(amount) FROM 'sales.parquet'"
ducklocal query --sql "DESCRIBE SELECT * FROM 'events.json'"
```

先 DESCRIBE，不猜 `amount` 等列名。完整统计应在 SQL 内聚合，不能累加截断预览。CLI 不共享 GUI 注册视图，也不共享其他调用的内存表；需要持久化时显式使用数据库：

```bash
# 分两次调用执行，每次只有一条语句；写入必须获得授权。
ducklocal query --database warehouse.duckdb --read-write --sql "CREATE TABLE sales AS SELECT * FROM 'sales.csv'"
ducklocal query --database warehouse.duckdb --sql "SELECT count(*) FROM sales"
```

数据库锁冲突直接报错。关闭其他写入者（包括 GUI），或使用授权的副本；不要删锁文件、不要偷偷换成别的数据库。

## JSONL 与嵌套 JSON

`read_ndjson_objects` 是换行分隔 JSON（每行一个对象，`.jsonl`/`.ndjson`）的读取函数；`read_json`/`read_json_auto` 读取的是 JSON 文档（一个数组或单个对象）。读 JSONL 文件时请显式写出读取函数，不要依赖自动探测。`json_extract_string(col, '$.a.b[0].c')` 沿嵌套路径——对象键与数组下标——取值并返回 VARCHAR：

```bash
ducklocal query --sql "SELECT json_extract_string(event, '$.payload.items[0].sku') AS sku, count(*) AS n FROM read_ndjson_objects('events.jsonl') GROUP BY 1 ORDER BY n DESC"
```

叶子不是字符串、想要 JSON 形式时改用 `json_extract`。

## 数据画像

`ducklocal profile` 回答的是「该画什么图、用什么刻度、怎么格式化数字」这一类在动手之前必须确定的问题。`DESCRIBE` 只告诉你某列是 `DOUBLE`；画像会告诉你它实际只用两位小数、最大值是中位数的一千倍、日期在中间断了十一天。

```bash
ducklocal profile sales.csv
ducklocal profile orders.parquet
ducklocal profile "my orders" --database warehouse.duckdb
```

TARGET 是数据文件（`csv`、`tsv`、`txt`、`parquet`、`json`、`ndjson`、`jsonl`），或工作簿（`xlsx`、`xls`、`xlsb`、`ods`，其第一个工作表会被导入为 TEMP 表再画像），或者配合 `--database` 时是表名/视图名。带点的名字按 `schema.table` 拆开并逐段加引号，所以带空格或大写的名字可以照原样写。TARGET 指向不存在的东西是参数错误（退出码 2），不是 SQL 错误。

返回一个 JSON 对象：`target`、`relation`（统计实际执行的 SQL 关系）、`row_count`、`elapsed_ms`，以及按关系自身列序排列的 `columns`。每列包含：

| 字段 | 含义 |
| --- | --- |
| `name`、`type` | 与 `DESCRIBE` 一致 |
| `nulls` | 该列为 `NULL` 的行数 |
| `distinct` | 精确值，不是估算 |
| `unique` | 当 `distinct` 等于 `row_count` 时出现且为 `true` |
| `min`、`max` | 以文本给出，`DECIMAL` 因此不丢位数 |
| `decimals` | 小数点后**实际用到**的位数，不是声明的 scale |
| `median` | 该列真实存在的一个值，不是两个值之间的插值 |
| `max_over_median` | 最大值是中位数的多少倍 —— 决定用线性刻度还是对数刻度的那个数 |
| `covered_days`、`span_days`、`missing_days` | 该列出现过的天数、首尾之间的天数、以及两者之差：`0` 表示连续，非零就是折线会直接画过去的空洞 |

`decimals`、`median`、`max_over_median` 只出现在数值列，日期三项只出现在 DATE/TIMESTAMP 列。`LIST`、`STRUCT`、`MAP`、`UNION` 列只报 `nulls` —— 这些类型没有定义 `min`/`max`。

统计是精确的，会完整扫描整个关系，所以一次画像等于一次全表扫描。`--limit` 对它不适用：对样本做画像不算画像。

## 把应用导出为独立 HTML

`ducklocal export` 回答的是「那个应用当时显示的是什么」，给没有 DuckLocal 的人看。它把应用跑一次——同一套运行时、同一个 host 模块、与 `query` 相同的数据库规则——然后把应用 `query()` 发出的语句和结果写进一个自包含的 HTML 文件。

```bash
ducklocal export --html examples/analysis_app
ducklocal export --html --out report.html --database warehouse.duckdb apps/sales
```

- `--html` 必填，也是目前唯一的格式。APP 是应用目录（其中含 `main.js`）或指向该 `main.js`，按 GUI 的解析方式解析；两者都不是则报参数错误（退出码 2）。
- `--out FILE` 指定输出路径，默认写到工作目录下的 `./<应用目录名>.html`。目标已存在时必须加 `--force`，且这项检查发生在运行应用之前——拒绝不花任何代价。
- `--database PATH` 与 `--read-write` 的含义与 `query` 完全一致：已有文件默认只读，省略则为内存库。应用就在这条连接上运行，所以需要写库的应用要加 `--read-write`。
- `--timeout SECONDS` 为正整数，默认 15：应用停止查询片刻后捕获结束，或到达时限结束，以先到者为准。
- 该命令会启动窗口平台：应用要渲染，渲染需要窗口。窗口是隐藏的、不会出现。

stdout 为单个 JSON 对象：

```json
{"html":"/abs/report.html","app":"/abs/app","queries":2,"rows":212,"app_errors":0,"captured_ms":630,"stop_reason":"settled"}
```

`queries` 是捕获到的语句数，`rows` 是其中成功语句的行数合计，`app_errors` 是应用运行期间记录的报错条数。`stop_reason` 为 `settled` 表示应用自己安静了下来，`deadline` 表示被 `--timeout` 时限截断——此时报告可能缺少语句，HTML 中会出现明显的警告说明这一点。

报告包含：应用目录、所用数据库、导出时间，以及每条语句一节——SQL、列名与 Arrow 类型、结果表格，并在结果是「每行一个名称 + 一个数值」时附一张柱状图。`catalog()` 调用会成为一节表/视图/列清单。同一语句被反复执行只出现一次，展示最后一次结果。

报告**不包含**应用自身的布局与图表。应用画的是原生组件，柱状图的数值是在布局时决定的、没有任何可读出的描述——所以导出给出的是应用所依据的数据，而不是它搭出的界面。筛选、开关和后续刷新同样不在其中，文件里也没有 JavaScript。它是应用加载状态的快照，报告开头就写明了这一点。

应用的 JavaScript 权限与平时完全一样：`query()` 可以 `COPY`、`ATTACH`、写文件。导出不是沙箱。

退出码：**0** 已写出文件；**2** 命令行有误、应用路径指不到东西、目标文件已存在；**1** 应用加载失败（不写文件），或应用加载后失败（仍写出报告，错误记在报告里，stderr 消息会指明文件路径）。应用类失败的错误 kind 为 `app`。

## 校验 dashboard 规格文件

格式本身——block、属性、图表类型、SQL 规则与常见信息——见 [Dashboard（.dash）](dashboards.md)；本节讲这个命令。

`.dash` 文件以声明的方式描述一个 dashboard——像 Terraform 声明基础设施那样，用 block 写查询和图表，用引用连接它们——而不是像分析应用那样用脚本来画。两种格式并存：规格文件覆盖「查询 + 标准图表」，人和 agent 都容易写、容易 diff；需要定制布局和交互时仍用应用。

```hcl
query "latency" {
  sql = <<SQL
    SELECT timestamp, service, avg(latency) AS latency
    FROM logs
    GROUP BY timestamp, service
  SQL
}

plot "latency" {
  type   = "line"
  query  = query.latency
  x      = timestamp
  y      = latency
  series = service
}
```

```bash
ducklocal check dashboard.dash
ducklocal check dashboard.dash --database warehouse.duckdb
```

`query` block 只含一个 `sql` 属性——一条只读语句，heredoc 或字符串。接受 `SELECT`、`WITH`、`FROM` 开头、`VALUES`、`SHOW`、`DESCRIBE`、`SUMMARIZE` 与 `PIVOT`；任何可能写入的语句——DDL、DML、`COPY`、`ATTACH`、`INSTALL`——都会报诊断，GUI 也拒绝执行，因为 dashboard 的查询在文件一打开时就会运行。`plot` block 含 `type`（`line`、`bar`、`area`、`scatter`、`table` 之一）、`query`（指向同文件某个 query block 的引用，如 `query.latency`）、`x` 和 `y`（结果列名，可写裸标识符或带引号的字符串；`table` 类型不需要 `y`），以及可选的 `series`、`title`。`#` 和 `//` 注释到行尾。这就是全部语法：没有函数、没有条件、没有插值。

不带 `--database` 时校验完全静态——表不需要存在，什么都不会执行。每条查询的 SQL 由真正的 DuckDB parser 在一次性连接上校验，与 `query` 执行前的校验相同。带 `--database PATH`（已存在的文件，只读打开）时，每条查询还会像打开 dashboard 时那样真正执行一遍（只读，同一条代码路径、同样的行数上限），并把每个 plot 的 `x`/`y`/`series` 与查询实际返回的列逐一核对；对 `table` 以外的类型，`y` 不是数值列也是错误。之所以要执行而不只是规划：有些错误——比如当前构建做不了的类型转换、数据里某个转不过去的值——只有真正读到行时才会出现，只规划的 check 会放过它们，交出一个画不出来的 dashboard。查询会完整执行，所以对大库做 check 的耗时和打开 dashboard 相当。

成功时输出一个 JSON 对象，列出文件中的查询（给了数据库时附带结果列）和图表。规格文件的错误是退出码 2、kind 为 `spec`，每条诊断一行 `文件:行号: 消息`——一次列出全部，而不是第一条。数据库或 I/O 错误是退出码 1。

同一个文件也可以在 GUI 里作为 dashboard 标签页打开——`ducklocal dashboard.dash`，或把文件拖到窗口上——每个 plot 位于可拖动调整高度的竖直堆叠中，画不出来的 plot 会在自己的格子里显示原因。见[应用指南](analysis-app.md)。

## 用 LSP 编辑 dashboard 规格文件

`ducklocal lsp` 是一个面向 `.dash` 文件的 Language Server Protocol 服务器，走 stdio，供编辑器启动。它把 GUI 自带规格编辑器的能力带给任何支持 LSP 的编辑器：与 `check` 相同的诊断，在每次打开和修改时推送（全量文档同步，严重级别全部是 Error——它们是错误而不是风格提示）；block 类型、属性名、图表类型和查询名的补全，每项带一个替换正在输入单词的 text edit；block、属性和引用的悬停文档；以及从 `query.name` 引用跳到对应 query block 名字的 definition。带 `--database PATH` 时，每条查询的 SQL 还会由真正的 DuckDB parser 校验，与 `check --database` 相同。

把编辑器的通用 LSP 支持指向 `ducklocal lsp`（作用于 `*.dash` 文件）即可——命令不接受文档参数，编辑器通过 stdio 与它通信。Neovim（0.10+）示例：

```lua
vim.api.nvim_create_autocmd("FileType", {
  pattern = "dash",
  callback = function(event)
    vim.lsp.start({
      name = "ducklocal",
      cmd = { "ducklocal", "lsp" },
      root_dir = vim.fs.dirname(vim.api.nvim_buf_get_name(event.buf)),
    })
  end,
})
```

VS Code 里任何通用 LSP 扩展都可以用命令 `ducklocal`、参数 `["lsp"]`、文档选择器 `dash` 完成同样配置。退出码：正常 `shutdown`/`exit` 为 **0**；客户端未按流程退出为 **1**；命令行错误或协议握手失败为 **2**。

## JSON 契约

成功时 stdout 为单个 JSON 对象，末尾换行：

```json
{"columns":[{"name":"n","type":"Int32"}],"rows":[[1]],"row_count":1,"truncated":false,"elapsed_ms":1}
```

`columns` 和每行都保留列顺序与重复列名。`type` 是 **Arrow debug 表示**，不是 DuckDB 原始 SQL 类型名（HUGEINT 与 DECIMAL 等可能使用相同 Arrow 表示）。`row_count` 为返回行数，不是完整匹配数或修改行数。`elapsed_ms` 包含准备、执行、行编码，不含读取 SQL 文件及建立连接。零行仍有列信息。DDL/DML 按真实结果输出，包括 `Count` 列：INSERT/COPY 通常返回 `[[N]]`，CREATE TABLE 通常返回零行；不按 SQL 首词猜测。

单元格编码：

| SQL 值 | JSON 编码 |
| --- | --- |
| NULL | `null`，不是字符串 `"NULL"` |
| 布尔、有限浮点数 | JSON 布尔/数字 |
| ±9,007,199,254,740,991 范围内整数 | JSON 数字 |
| 更大整数（含 HUGEINT/UHUGEINT） | `{"encoding":"integer","value":"9007199254740992"}` |
| Decimal | `{"encoding":"decimal","value":"123.450"}`，精确十进制文本 |
| 文本、enum | JSON 字符串 |
| Blob、geometry | `{"encoding":"hex","value":"00ff"}`，小写十六进制；geometry 为 WKB |
| Date | `{"encoding":"date","unit":"Day","value":"1"}`，自 1970-01-01 起天数 |
| Timestamp | `{"encoding":"timestamp","unit":"Nanosecond","value":"123456789"}`，自 Unix epoch 起有符号计数；也可能是 Second/Millisecond/Microsecond |
| Time | `{"encoding":"time","unit":"Microsecond","value":"123456"}`，自午夜起计数 |
| Interval | `{"encoding":"interval","months":1,"days":2,"nanos":"3000"}` |
| NaN、正/负无穷 | `{"encoding":"float","value":"NaN"}` / `"inf"` / `"-inf"` |
| List、array | 递归编码的 JSON 数组，保留嵌套 null |
| Struct | `{"encoding":"struct","fields":[["name",VALUE],...]}` |
| Map | `{"encoding":"map","entries":[[KEY,VALUE],...]}`，键保留类型，不转换为 JSON 对象 |
| Union | `{"encoding":"union-value","value":VALUE}`，仅活动值，不保留成员标签 |

时间使用原始计数，不丢小数精度；日期/时间戳的无穷哨兵也保留为原始计数，不格式化。时区信息在 Arrow 元数据中。当读输出的是人而不是程序时——报表、看板——在 SQL 里 `CAST(made_at AS VARCHAR)` 可以得到 ISO 文本而不是计数；`--format md` 无需 cast 就已把日期和时间戳渲染得可读。当前 DuckDB Arrow 封装无法区分嵌套 HUGEINT、UHUGEINT 和 DECIMAL(38,0)，遇到这类非空值会明确报错，不悄悄改符号或精度；请在 SQL 中显式 CAST 为 VARCHAR。未知不支持类型也会报错，不能当空结果。Union 只是活动值表示，不能用于无损往返恢复 union 标签。

错误时 stdout 留空，**stderr** 输出单个 JSON 对象：

```json
{"error":{"kind":"argument","message":"Provide exactly one of --sql and --sql-file"}}
```

退出码：**0** 成功，**2** 参数/语句数量错误，**1** SQL、数据库、I/O 或输出错误。错误 kind 为 `argument`、`sql`、`database`、`io`、`output`，`check` 的 `spec`，以及 `export` 的 `app`。先检查退出码再解析 stdout，报告完整性前检查 `truncated`。帮助和版本是纯文本例外。stdout 管道关闭等传输错误无法保证输出为空或完整。

## 安装官方 skill

标准 skill 位于 [`skills/ducklocal`](https://github.com/JetSquirrel/ducklocal/tree/main/skills/ducklocal)。从 checkout 复制到目标项目的 skill 目录（以 Claude Code 为例）：

```bash
# 在 DuckLocal 仓库根目录执行；把目标路径换成你的项目。
mkdir -p /path/to/your-project/.claude/skills
cp -R skills/ducklocal /path/to/your-project/.claude/skills/
```

目标已存在时先检查，避免覆盖本地修改。其他 agent 可将相同的 `SKILL.md` 与 `references/cli.md` 放入其支持的项目级 skill 目录。不修改全局配置，不需要 marketplace。skill 覆盖 schema 探查、有限预览、SQL 聚合、格式转换和数据库查询；不提供 S3 浏览、空间专用命令、会话记忆或 MCP 服务。
