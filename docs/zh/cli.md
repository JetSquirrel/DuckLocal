# AI CLI 与官方 skill

**[English](../cli.md)** · [文档](index.md)

## 无窗口运行

`ducklocal query` 执行 SQL，不初始化 GUI、历史、注册文件、语言设置或 GUI 全局连接。每次调用创建独立连接。无参数或文件/目录/glob 参数仍打开原有 GUI；名为 `query` 的 GUI 路径请写 `./query`。

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
ducklocal query --sql-file analysis.sql --limit 100
ducklocal query --sql-file - < analysis.sql
ducklocal query --database warehouse.duckdb --sql "SHOW TABLES"
```

- 必须且只能提供 `--sql SQL` 或 `--sql-file FILE`。文件必须为 UTF-8；`-` 从 stdin 读取。每次只允许一条 SQL。打开目标数据库或执行操作前用 DuckDB 的解析器验证；支持注释、引号中的分号、尾部分号，不会只执行脚本的一部分。
- `--database PATH` 默认以**只读**打开**已有文件**；省略则用新的内存数据库。`--read-write` 必须搭配 `--database`，显式允许写入/创建数据库，不创建父目录。打开失败不回退到内存。
- `--limit N` 为正整数，默认 1000，限制返回行数；另有 2,000,000 个单元格的预算。额外读取一行判断 `truncated`。这不限制计算量或 DuckDB 内部结果缓冲；单个大单元格仍可能占用很多内存。
- 重复/未知参数、缺失值、空 SQL、非法 limit 均报错。参数和值分开，不支持 `--flag=value`。相对路径基于进程工作目录，不是 SQL 文件目录。Shell 路径需引号；SQL 路径中的单引号双写（`'O''Brien.csv'`）；SQL 标识符用双引号。

**数据库只读不是文件系统或网络沙箱。** `COPY` 仍可写文件。执行前确认输出路径、覆盖文件、数据库修改、安装扩展和外部访问都符合用户授权。不自动安装扩展，不自动重试；授权后仍可显式执行 `INSTALL`/`LOAD`，已有扩展可能自动加载。失败命令也可能已经产生副作用（例如输出失败前已写入），重试前检查目标。

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

## 数据画像

`ducklocal profile` 回答的是「该画什么图、用什么刻度、怎么格式化数字」这一类在动手之前必须确定的问题。`DESCRIBE` 只告诉你某列是 `DOUBLE`；画像会告诉你它实际只用两位小数、最大值是中位数的一千倍、日期在中间断了十一天。

```bash
ducklocal profile sales.csv
ducklocal profile orders.parquet
ducklocal profile "my orders" --database warehouse.duckdb
```

TARGET 是数据文件（`csv`、`tsv`、`txt`、`parquet`、`json`、`ndjson`、`jsonl`），或者配合 `--database` 时是表名/视图名。带点的名字按 `schema.table` 拆开并逐段加引号，所以带空格或大写的名字可以照原样写。TARGET 指向不存在的东西是参数错误（退出码 2），不是 SQL 错误。

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

时间使用原始计数，不丢小数精度；日期/时间戳的无穷哨兵也保留为原始计数，不格式化。时区信息在 Arrow 元数据中。当前 DuckDB Arrow 封装无法区分嵌套 HUGEINT、UHUGEINT 和 DECIMAL(38,0)，遇到这类非空值会明确报错，不悄悄改符号或精度；请在 SQL 中显式 CAST 为 VARCHAR。未知不支持类型也会报错，不能当空结果。Union 只是活动值表示，不能用于无损往返恢复 union 标签。

错误时 stdout 留空，**stderr** 输出单个 JSON 对象：

```json
{"error":{"kind":"argument","message":"Provide exactly one of --sql and --sql-file"}}
```

退出码：**0** 成功，**2** 参数/语句数量错误，**1** SQL、数据库、I/O 或输出错误。错误 kind 为 `argument`、`sql`、`database`、`io`、`output`。先检查退出码再解析 stdout，报告完整性前检查 `truncated`。帮助和版本是纯文本例外。stdout 管道关闭等传输错误无法保证输出为空或完整。

## 安装官方 skill

标准 skill 位于 [`skills/ducklocal`](https://github.com/JetSquirrel/ducklocal/tree/main/skills/ducklocal)。从 checkout 复制到目标项目的 skill 目录（以 Claude Code 为例）：

```bash
# 在 DuckLocal 仓库根目录执行；把目标路径换成你的项目。
mkdir -p /path/to/your-project/.claude/skills
cp -R skills/ducklocal /path/to/your-project/.claude/skills/
```

目标已存在时先检查，避免覆盖本地修改。其他 agent 可将相同的 `SKILL.md` 与 `references/cli.md` 放入其支持的项目级 skill 目录。不修改全局配置，不需要 marketplace。skill 覆盖 schema 探查、有限预览、SQL 聚合、格式转换和数据库查询；不提供 S3 浏览、空间专用命令、会话记忆或 MCP 服务。
