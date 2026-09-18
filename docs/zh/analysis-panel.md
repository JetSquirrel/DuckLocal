# 分析面板（JavaScript）

**[English](../analysis-panel.md)** · [文档](index.md)

分析面板是工作区里的一个标签页，内容是你自己写的 JavaScript 应用。JavaScript 就是声明本身：
面板的 `render` 函数即界面描述，时间范围控件、多选 chips、KPI 卡片、图表与表格都由你组合。
面板通过下面的 host 函数读取应用已经打开的数据库——同样的表、视图与已附加文件。

它适合为数据做一个专门的界面：看板、只针对某张表的浏览器、每天早上跑一次的报表。面板像查询一样
开合，和 SQL 标签并列。

## 打开面板

以下任一种方式都会立刻加载目录：

- 标签条末尾的 **+**，然后选 **打开面板…**；
- 把面板目录拖到窗口上；
- 在命令行里指定：`ducklocal examples/analysis_app`。

无法作为面板的目录——文件、已不存在的路径、或没有 `main.js` 的文件夹——会在**标签页内**被拒绝，
并说明是哪个目录、缺了什么。取消选择或选错目录都不会丢掉正在工作的面板。

面板用标签自带的关闭按钮关闭，也能像查询标签一样重命名。已打开的面板目录会被记住：下次启动自动
恢复；如果某个目录已被移走或删除，会按名字报告出来，而不是悄悄忘记。

面板在自己的 DuckDB 连接上运行，连的是主窗口已经打开的那个数据库：两边看到同样的表、视图与已附加
文件，但互不等待——一个看板刷新六条语句，不会再把 SQL 编辑器冻住。第二个连接带不过来的是连接级状态：
SQL 编辑器里建的 `TEMP` 表、设的 `SET`，面板看不到，反之亦然。所有面板共用这一个连接，因此面板之间
仍然会互相排队。

## 查看定义

面板头部的 **查看定义** 会显示该目录与入口文件的源码（只读）。面板就是一个普通的 JavaScript
目录，这就是它的全部定义——没有第二份隐藏形式需要查看。

## 重新加载

保存面板目录下的 `.js` 或 `.mjs` 文件，面板就会重新加载。宿主每 250 毫秒扫描一次目录，并在最后一次
变更后等待 200 毫秒，因此一次保存多个文件只会触发一次重载。以点开头的文件、`node_modules/` 与
`target/` 会被跳过。只有当前显示的目录会被监控。

面板头部的 **重新加载** 可以随时手动触发同样的流程。

重载失败时，**上一个成功加载的面板会保留**，错误信息显示在它上方、就在这个标签页里。如果面板从未
成功加载过，错误信息会占据标签页主体。

## Host 函数

从 `ducklocal` 模块导入：

```js
import { catalog, query, panelDir, sqlLiteral, sqlIdentifier } from "ducklocal";
```

它们都在面板连接上执行——和主窗口同一个数据库——出错时抛出 JavaScript `Error`。除 `panelDir` 之外
都可以在任何时刻调用；`panelDir` 在面板加载期间作答。

### `catalog()`

```ts
catalog(): Promise<CatalogEntry[]>

interface CatalogEntry {
  database: string;          // 例如 "memory"
  schema: string;            // 例如 "main"
  name: string;              // 例如 "orders"
  kind: "table" | "view";
  estimated_rows: number | null;
  comment: string | null;
  columns: { name: string; type: string }[];   // type 为 DuckDB 类型，如 "DECIMAL(38,10)"
}
```

当前数据库中除 DuckDB 自身 catalog（`information_schema`、`pg_catalog`、`system`）之外的所有表与视图，
表排在视图之前。DuckDB 的标识符不区分大小写，且表与视图共用一个命名空间，所以用它拼 SQL 时请带上
schema 限定。

### `query(sql, limit?)`

```ts
query(sql: string, limit?: number): Promise<QueryResult>

interface QueryResult {
  columns: { name: string; type: string }[];
  rows: any[][];             // 每行一个数组，每个元素对应一列
  row_count: number;         // 实际返回的行数
  truncated: boolean;        // 行数上限提前截断时为 true
  elapsed_ms: number;
}
```

- `limit` 默认为 **1000**，会被限制在 1…100000。此外还有 2,000,000 个单元格的预算，因此很宽的结果
  返回的行数会少于 `limit`。**请务必读取 `truncated`。**
- `type` 是应用渲染的 Arrow 类型，例如 `Int64`、`Utf8`、`Decimal(38, 10)`、
  `Timestamp(Microsecond, None)`。
- 语句按原样 prepare 并执行；`query()` 不会拆分或校验多语句 SQL。

### `panelDir()`

```ts
panelDir(): string   // 面板自己的目录，绝对路径
```

面板不能读文件系统，所以这是它引用随面板一起分发的数据的方式——
`query("SELECT * FROM " + sqlLiteral(panelDir() + "/orders.csv"))` 不需要用户附加任何文件、
也不需要应用里打开任何东西。它在面板加载期间作答：请在 `init()` 里调用一次并保存结果。

### `sqlLiteral(value)`

```ts
sqlLiteral(value: string): string   // 例如 "O'Brien" -> 'O''Brien'
```

把字符串转义并加引号成 SQL 字符串字面量。凡是来自面板自身状态的值——筛选条件、渠道 id、日期——
都应该用它，而不是手工拼进 SQL。包含 `NUL` 字节会被拒绝，因为 SQL 字面量无法承载它。

### `sqlIdentifier(name)`

```ts
sqlIdentifier(name: string): string   // 例如 "my table" -> "my table"
```

同一件事的另一半：表名、视图名、列名**这类标识符**，加上双引号，让它被当作一个名字读取，并且严格按
写法匹配。标识符不是字符串字面量，而面板用来拼 SQL 的名字往往不是自己起的——`catalog()` 返回的是数据库
里实际有的东西，`my table`、`Order`、名字里带 `"` 的都会出现。空名字和含 `NUL` 字节的名字会被拒绝：
它们都不指向任何东西，加上引号只会把这件事掩盖掉。

```js
const entry = (await catalog()).find((table) => table.name === "orders");
const from = `${sqlIdentifier(entry.schema)}.${sqlIdentifier(entry.name)}`;
const rows = await query(`SELECT count(*) FROM ${from}`);
```

### 单元格编码

当“不丢信息”时，单元格就是普通 JSON——`null`、布尔、字符串，以及能被 IEEE double 精确表示的数值。
其余情况一律以带 `encoding` 字段的对象返回，与 `ducklocal query` 命令行的编码完全一致，因此不会有
数字被悄悄截断：

| 值 | 传入 JavaScript 的形式 |
| --- | --- |
| `NULL` | `null` |
| `BOOLEAN` | `true` / `false` |
| ±2^53−1 以内的整数 | number |
| 更大的整数（`HUGEINT`、`UBIGINT` 等） | `{ "encoding": "integer", "value": "170141183460469231731687303715884105727" }` |
| `DECIMAL` | `{ "encoding": "decimal", "value": "12345678901234567890.1234567890" }` |
| 非有限 `FLOAT`/`DOUBLE` | `{ "encoding": "float", "value": "inf" }` |
| `DATE` | `{ "encoding": "date", "unit": "Day", "value": "19783" }` |
| `TIMESTAMP` | `{ "encoding": "timestamp", "unit": "Microsecond", "value": "1709296496789000" }` |
| `TIME` | `{ "encoding": "time", "unit": "Microsecond", "value": "45296789000" }` |
| `INTERVAL` | `{ "encoding": "interval", "months": 0, "days": 1, "nanos": "0" }` |
| `BLOB`、`GEOMETRY` | `{ "encoding": "hex", "value": "00ff" }` |
| `LIST` / `ARRAY` | 数组 |
| `STRUCT` | `{ "encoding": "struct", "fields": [["name", <value>], …] }` |
| `MAP` | `{ "encoding": "map", "entries": [[<key>, <value>], …] }` |
| `UNION` | `{ "encoding": "union-value", "value": <value> }` |

`DATE` 的 `19783` 是自 1970-01-01 起的天数；`TIMESTAMP` 与 `TIME` 是按 `unit` 计的纪元或当日零点
以来的计数。这些是存储值而非格式化文本：请自行格式化，若想要字符串形式，请在 SQL 中把列
`CAST` 成 `VARCHAR`。

## 可用组件

面板使用与 shell 相同的组件目录，从 `gpui-component` 导入：`GroupBox`（带标题的卡片）、
`Progress`（条）、`Toggle`、`Badge`、`Tag`、`Alert`、`Empty`、`Collapsible`、
`DescriptionList`、`DataTable` 与 `DataTableState`、`Table` 家族、`Sidebar`、`Resizable`、
`Scroll`，以及图表 `BarChart`、`LineChart`、`AreaChart`、`PieChart`、`RadarChart`。
布局原语（`div`、`h_flex`、`v_flex`、`Button`）来自 `gpui-kit` 与 `gpui-base`。

没有 `ToggleGroup`：分段控件或多选 chips 用一组 `Toggle` 组合而成。仓库里的
[`examples/analysis_app`](https://github.com/JetSquirrel/DuckLocal/tree/main/examples/analysis_app)
就是完全用这些搭出来的一份完整看板，最适合拿来照抄。

每个面板目录都带一份 `jsconfig.json` 和生成的 `gpui-kit.d.ts`。保存之前，先按运行时真正
会给它的 API 检查一遍：

```sh
npx --yes -p typescript tsc -p <面板目录>/jsconfig.json --noImplicitAny false
```

它会报出组件上并不存在的方法——这类错误运行时只会在渲染时、在面板自己的错误区域里拒绝，
而那里不会写进应用日志。它也有盲区：`gpui-base` 的原语（`div`、`h_flex`、`v_flex`、
`Button.new`）类型很松，写错方法这里查不出来，仍然只能在渲染时才暴露。

## SQL 没有沙箱

`query()` 在应用自己的数据库上执行，**拥有应用与用户的权限**。面板能做的事和 SQL 编辑器完全一样，
包括 `COPY` 写文件、`ATTACH` 其他数据库、`INSTALL`/`LOAD` 扩展，以及查询已配置好的 S3 视图。请把面板
里的 JavaScript 当作你主动选择运行的代码，就像对待一个 shell 脚本那样。

面板拿不到的，是进程的其他能力：

- **没有文件系统、网络、进程与环境变量模块。** 除非宿主授予，否则脚本无法使用 `fs`、`net`、
  `process` 等模块；DuckLocal 一律不授予。
- **拿不到 S3 凭据。** S3 浏览器的密钥保存在应用自己的 Rust 状态里，由 DuckLocal 的签名客户端使用，
  从不会写入 DuckDB 连接，host 模块也无法访问。不过，如果用户自己的连接配置了 httpfs，面板依然可以
  执行使用它的 SQL。
- **host 模块就是全部接口。** 只有上面几个函数会跨到 Rust 一侧。

## 已知限制

- **标签页里的面板没有窗口级浮层。** shell 的对话框、面板、toast 与 tooltip 浮层，只有在 shell 自己的
  根视图是窗口首视图时才能被找到。而在标签页里，这个位置由应用的根视图占据——否则应用自己的对话框
  会出问题——因此 `window.open_dialog`、`window.open_sheet`、`window.push_toast` 会抛出
  `TypeError`，内容是 *needs a ShellRoot as the window's first view*，和你自己代码里的其他错误一样会
  显示出来。tooltip 是例外，也是唯一静默的一个：它由 hover 监听器挂上，那里没有地方抛错，所以面板里的
  tooltip 只是永远不出现。请把面板界面设计在自身范围内——用展开区域代替模态框。
- **重新加载由 DuckLocal 自己实现，而不是 shell 的 watcher。** gpui-shell 的热重载依赖
  `runtime.watch` / `runtime.refresh`，它们只对运行时自己创建的 `ShellRoot` 生效；嵌入宿主走不到这条
  路径，因此 DuckLocal 自己监控目录，并通过 `load_application` / `mount_application` 重载。代价是：
  只监控 `.js`/`.mjs` 文件，面板读取的其他文件不会被监控。
- **重载在主线程上编译。** 脚本运行时不可跨线程传递，因此面板的挂载发生在主线程；模块图很大时，
  加载期间窗口会卡顿。首次加载会推迟到标签页的第一帧之后，所以无论哪种情况标签页都会立刻出现。
- **所有面板共用一个连接，且不能取消。** 面板不再阻塞主窗口，但仍然互相阻塞：它们共用一个连接、
  一把锁，所以一个面板里的长查询会拖住其他面板。做不到一个面板一个连接——host 函数拿到的是参数，
  不是调用者，`query()` 执行的那一刻没有任何东西能说明是哪个面板在问。取消执行中的查询同样没有暴露：
  请把面板的语句写得有界，而不是指望中途叫停。
- **`row_count` 是返回的行数，不是匹配的行数。** 当 `truncated` 为 `true` 时，后续行不可得；需要计数
  请在 SQL 里聚合，而不是在 JavaScript 里数。
