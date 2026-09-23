---
description: "用 .dash 文件编写 DuckLocal dashboard——基于 DuckDB SQL 的 query 与 plot block——在标签页中打开，用 ducklocal check 校验，并借助 LSP 编辑。"
---

# Dashboard（.dash）

**[English](../dashboards.md)** · [文档](index.md)

`.dash` 文件是用数据写成的 dashboard：几个装着 SQL 的 **query** block，加上把结果画出来的 **plot** block。不需要写代码，所以人或 AI agent 都能写出来，在 diff 里审阅，并在任何人打开之前先校验一遍。

标准图表与表格、基于保存好的查询——用 `.dash` 文件。需要定制布局或交互（按钮、过滤器、自己的组件）时，改写一个[分析应用](analysis-app.md)。

## 完整示例

这个 dashboard 读取的是打开[教程样例文件](tutorial.md#sample-file)后得到的 `sales` 视图。可下载为 [sales.dash](https://jetsquirrel.github.io/DuckLocal/samples/sales.dash)。

```hcl
query "by_channel" {
  sql = <<SQL
    SELECT channel, round(sum(revenue), 2) AS revenue
    FROM sales
    GROUP BY channel
    ORDER BY revenue DESC
  SQL
}

query "daily" {
  sql = <<SQL
    SELECT date, channel, round(sum(revenue), 2) AS revenue
    FROM sales
    GROUP BY date, channel
    ORDER BY date
  SQL
}

plot "revenue_by_channel" {
  type  = "bar"
  query = query.by_channel
  x     = channel
  y     = revenue
  title = "Revenue by channel"
}

plot "daily_revenue" {
  type   = "line"
  query  = query.daily
  x      = date
  y      = revenue
  series = channel
  title  = "Daily revenue, per channel"
}

plot "daily_table" {
  type  = "table"
  query = query.daily
  x     = date
  title = "The numbers behind it"
}
```

无论有多少个 plot 用到它，每个 query 只执行一次：`daily` 同时供给折线图和表格。

## 打开 dashboard

以下任意方式都会把 `.dash` 文件作为 dashboard 标签页打开，与查询并列：

- 标签栏末尾的 **+**，再选 **打开 Dashboard…**；
- 把文件拖到窗口上；
- 在命令行里指定：`ducklocal sales.dash`。

各个 plot 竖直堆叠，拖动分隔条可调整高度。画不出来的 plot（查询失败，或缺少某列）会在自己的格子里显示原因，其余部分照常绘制。打开的 dashboard 下次启动时会恢复，最近打开过的会列在侧栏的 **仪表盘** 下。

工具栏的 **查看源码** 会把标签页切换成该文件的编辑器，带语法高亮（heredoc 里的 SQL 与 SQL 编辑器同样着色）、补全和行内诊断。**保存** 或 **⌘S** 写回文件并重新绘制。保存后若不再通过校验，屏幕上仍保留上一次能用的 dashboard，并在上方显示原因；其他程序修改了文件时也是如此。**重新加载** 会重读文件并重新执行所有查询。

## Block

文件由一连串 block 组成，每个 block 是类型、带引号的名字和主体：

```hcl
query "name" { ... }
plot "name" { ... }
```

同一类中名字必须唯一：两个 query 不能同名，两个 plot 也不能——但一个 query 和一个 plot 可以同名。

### query

| 属性 | 必填 | 取值 |
| --- | --- | --- |
| `sql` | 是 | 一条只读 SQL 语句，写成 [heredoc](#heredoc) 或字符串 |

query block 只包含 `sql`。

### plot

| 属性 | 必填 | 取值 |
| --- | --- | --- |
| `type` | 是 | `"line"`、`"bar"`、`"area"`、`"scatter"` 或 `"table"`——带引号的字符串 |
| `query` | 是 | 指向某个 query block 的引用：`query.by_channel` |
| `x` | 是 | 横轴上的列。`table` 显示整个结果，所以它的 `x` 只需是其中任意一列 |
| `y` | 是（`table` 除外） | 要画的列；必须是数值类型 |
| `series` | 否 | 按这一列的取值把行拆成多条序列 |
| `title` | 否 | plot 的标题，带引号的字符串 |

`x`、`y`、`series` 指的是查询结果中的列，匹配时不区分大小写。普通列名直接写（`x = date`）；不是普通标识符的列名加引号（`y = "revenue (EUR)"`）。

### 图表类型

| 类型 | 画出 |
| --- | --- |
| `line` | 每条序列一条线。多于一条序列时显示为浅色填充的面积 |
| `area` | 每条序列一块填充面积 |
| `bar` | 每行一根柱子。带 `series` 时，每条序列一张小柱状图，上下堆叠 |
| `scatter` | 散点。多于一条序列时画成不带标记的线 |
| `table` | 把查询结果显示为表格；不使用 `y` 和 `series` |

行按查询返回的顺序绘制，所以给图表供数的查询要以 `ORDER BY` 结尾。

## 取值

| 形式 | 示例 | 用于 |
| --- | --- | --- |
| 字符串 | `"Revenue by channel"` | `type`、`title`、带引号的列名、简短的 `sql` |
| Heredoc | `<<SQL` … `SQL` | `sql` |
| 引用 | `query.daily` | `query` |
| 裸名字 | `channel` | `x`、`y`、`series` |

字符串在行尾结束，支持 `\n`、`\t`、`\"` 和 `\\`。

`#` 和 `//` 开始一段注释，直到行尾。属性之间不需要分隔符；换行只是空白。

这就是全部语法：没有变量、没有函数、没有条件、没有插值。

### Heredoc {#heredoc}

heredoc 用来写跨行文本——实际上就是 SQL：

```hcl
  sql = <<SQL
    SELECT 1
  SQL
```

- `<<` 后面的分隔符可以是任意单词（按惯例用 `SQL`），同一行里它后面不能再有别的内容。
- heredoc 在第一行内容恰好等于分隔符（允许前后有空格）的地方结束。
- 结束行的缩进会从每一行文本中去掉，所以 SQL 可以跟着 block 缩进，而不把缩进带进 SQL。

## SQL 规则 {#sql-rules}

dashboard 在打开、重新加载、保存时，以及 DuckLocal 启动并恢复该标签页时都会执行查询——所以查询只能**读**：

- **允许：** `SELECT`、`WITH …`、`FROM` 开头的查询、`VALUES`、`SHOW`、`DESCRIBE`、`SUMMARIZE`，以及写明 `IN (…)` 列表的 `PIVOT`。
- **拒绝：** 任何可能改动数据或文件的语句——`CREATE`、`INSERT`、`UPDATE`、`DELETE`、`DROP`、`COPY`、`ATTACH`、`INSTALL`、`LOAD` 等。打开别人发来的 `.dash` 文件不会改动你的数据。
- **每个 query 一条语句。** 不写 `IN (…)` 列表的 `PIVOT` 算作两条，因为 DuckDB 要先执行一条语句找出取值，再执行透视本身；请把取值列出来。

查询在窗口自己的连接上执行，所以能看到 SQL 编辑器能看到的一切：打开的文件、挂载的数据库，以及 `TEMP` 表等连接级状态。

### SQL 里的路径

`FROM 'sales.csv'` 这样的相对路径，是相对于 DuckLocal 的工作目录解析的——也就是你运行 `ducklocal` 时所在的文件夹；从访达或程序坞启动时则是 `/`。想让 dashboard 无论怎样打开都能用：

- 改为查询**视图**——先打开一次文件，然后用它得到的视图名（`FROM sales`），上面的示例就是这样做的；或者
- 使用**绝对路径**（`FROM '/Users/me/data/sales.csv'`）。

## 限制

| 限制 | 结果 |
| --- | --- |
| 每个 plot 12 条序列 | 多出的序列不画，并有提示说明原本有多少条 |
| 每条序列 1,500 个点 | 按分桶取平均压缩到上限以内 |
| 200 根柱子 | 多出的柱子不画 |

## 校验 dashboard

`ducklocal check` 不开窗口就能校验文件——把 dashboard 交给别人之前应该先跑一遍：

```bash
ducklocal check sales.dash
ducklocal check sales.dash --database warehouse.duckdb
```

- **不带 `--database`** 时是静态检查：语法、名字与引用、必填属性，以及用 DuckDB 自己的 parser 检查每个查询的 SQL。什么都不执行，也不需要任何表存在。
- **带 `--database PATH`** 时，每个查询还会在该数据库上（以只读方式打开）实际执行，并检查每个 plot 的 `x`、`y`、`series` 是否是查询真正返回的列——包括 `y` 是否为数值。

成功时输出查询与 plot 的 JSON 摘要（给了数据库时还包括结果列）。有错误时以状态 **2** 退出，一次列出所有问题，每条一行 `文件:行号: 信息`；数据库或文件错误以 **1** 退出。细节见 [CLI 指南](cli.md#校验-dashboard-规格文件)。

### 常见信息

| 信息 | 解决 |
| --- | --- |
| `type is a string in quotes` | 写 `type = "line"`，而不是 `type = line` |
| `Unknown plot type: "pie"; one of line, bar, area, scatter, table` | 使用列出的类型之一 |
| `plot "p" requires y` | 除 `table` 外的类型都需要 `y` |
| `plot "p" draws query "q", which the file does not define` | 补上这个 query block，或改正 `query = query.…` 里的名字 |
| `query names a query block: query = query.some_name` | `query` 属性要写引用，不是字符串 |
| `plot "p" draws y = "channel" (VARCHAR), which is not numeric` | 让 `y` 指向数值列，或改用 `type = "table"` |
| `a dashboard query must be a single read-only statement` | 写入操作放到 SQL 编辑器里做；见 [SQL 规则](#sql-rules) |
| `Unterminated heredoc: no line is exactly SQL` | 在单独一行写上结束分隔符 |

## 在你自己的编辑器里编辑

`ducklocal lsp` 是 `.dash` 文件的 language server：输入时的诊断，block 类型、属性、图表类型和查询名的补全，悬停文档，以及从 `query.name` 跳转到对应 block 的定义跳转。Neovim 与 VS Code 的配置见 [CLI 指南](cli.md#用-lsp-编辑-dashboard-规格文件)。
