# 10 分钟上手教程

**[English](../tutorial.md)** · [文档](index.md)

跟着做一遍：打开一个小的销售数据文件，问它几个问题，把答案画成图，再导出一份。本教程假设你已经装好了 DuckLocal——如果还没有，先看[快速上手](getting-started.md)。

## 1. 获取样例文件

下载 [sales.csv](/samples/sales.csv)——两周的虚构订单数据，共 224 行。也可以在终端里下载：

```bash
curl -LO https://jetsquirrel.github.io/DuckLocal/samples/sales.csv
```

它有五列：

| 列 | 类型 | 示例 |
| --- | --- | --- |
| `date` | DATE | `2026-09-01` |
| `channel` | VARCHAR | `Web`、`Mobile app`、`Store`、`Phone` |
| `region` | VARCHAR | `North`、`South`、`East`、`West` |
| `orders` | BIGINT | `72` |
| `revenue` | DOUBLE | `4181.04` |

这些都不需要你告诉 DuckLocal：DuckDB 会读取表头并自行推断类型。

## 2. 打开文件

把 `sales.csv` 拖到 DuckLocal 窗口上。（也可以点击 **打开文件…**；如果你已经[添加了命令](getting-started.md#add-command)，还可以运行 `ducklocal sales.csv`。）

会弹出通知 **已创建视图 sales**。侧边栏的 **本地文件** 下出现了 `sales` 和它的 224 行；展开它可以看到各列及其类型。

## 3. 看看数据

在侧边栏里，点击 `sales` 旁边的播放按钮。编辑器会填入一条针对它的 `SELECT * … LIMIT 100`——按 **⌘↵** 运行。

编辑器下方的表格显示了这些行。试试上方的过滤框：输入 `Store` 并回车，只保留包含它的行。过滤只作用于已经加载的行，不会重新查询。

## 4. 提一个问题

把 SQL 换成下面这条，按 **⌘↵**：

```sql
SELECT channel, round(sum(revenue), 2) AS revenue
FROM sales
GROUP BY channel
ORDER BY revenue DESC;
```

| channel | revenue |
| --- | --- |
| Mobile app | 290549.76 |
| Web | 289544.8 |
| Store | 240067.6 |
| Phone | 82511.2 |

现在把结果面板从 **结果** 切到 **图表**。因为第一列是文本，你会得到每个渠道一根柱子，高度取第一个数值列。

::: tip 自动补全
输入时，编辑器会先提示侧边栏里的表名和列名，然后是 DuckDB 函数和 SQL 关键字——输入 `rev`，第一个候选就是 `revenue`。
:::

## 5. 画一条趋势

图表直接取查询返回的行来画，所以想怎么画，就把结果整理成什么形状。每个渠道一列，就得到每个渠道一条线：

```sql
SELECT
  date,
  round(sum(revenue) FILTER (WHERE channel = 'Web'), 2)        AS web,
  round(sum(revenue) FILTER (WHERE channel = 'Mobile app'), 2) AS mobile,
  round(sum(revenue) FILTER (WHERE channel = 'Store'), 2)      AS store
FROM sales
GROUP BY date
ORDER BY date;
```

第一列是日期时，**图表** 会画出随时间变化的面积图，每个数值列一条序列。规则和限制见[结果与图表](results-and-charts.md)。

## 6. 导出结果

趋势结果还在面板里时，点击 **导出 CSV**。对话框默认填入 `~/Desktop/export.csv`；按需修改路径后确认。

导出会重新执行查询，所以文件里是完整结果，而不只是表格里看得到的部分。**如果文件已存在，会直接覆盖，不会询问。**

## 7. 明天还能找到

打开侧边栏的 **查询历史** 标签：你运行过的每条查询都在这里，失败的也在。点击一条可以把它放回编辑器——注意这会替换编辑器里原有的内容。

退出 DuckLocal 再重新启动。`sales` 仍然在 **本地文件** 下：打开过的文件按路径记住，每次启动时重新读取，所以 CSV 改过之后，下次查询就能看到新内容。

## 8. 附加：在终端里问同一个问题

如果你添加了 `ducklocal` 命令，CLI 可以不开窗口直接回答——写脚本或交给 AI agent 都很方便：

```bash
ducklocal query --format md --sql "
  SELECT channel, round(sum(revenue), 2) AS revenue
  FROM 'sales.csv' GROUP BY channel ORDER BY revenue DESC"
```

去掉 `--format md` 就得到结构化的 JSON。入门请看 [CLI 指南](cli.md)和官方 agent skill。

## 接下来

- 用 DuckLocal 打开你自己的文件或文件夹——见[数据源](data-sources.md)
- 查询 S3 兼容存储里的文件——见 [S3 与 httpfs](s3.md)
- 用保存的查询搭一个 dashboard——见[分析应用与 Dashboard](analysis-app.md#dashboard-specs-dash)
