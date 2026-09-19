# DuckLocal 反馈：用 dash export 绘制 AIOps 赛事仪表盘的卡点记录

来源：2026-09-19，Agent（Kimi Code CLI）用 `ducklocal 0.1.0`（target/release）为
`/Users/admin/side-proj/aiops-challege` 项目构建仪表盘面板并导出 HTML。
产出：`work/dashboard/main.js` + `work/dashboard.html`（6 statements / 95 rows / 0 errors / 687ms）。
最终成功了，但过程里有几个真实的卡点，按耗时排序。

## 卡点 1（最大）：Panel 编写 API 完全没有文档

- `skills/ducklocal/SKILL.md` 只覆盖 `query` / `profile` / `dash export` 三个 CLI 动词；
  "analysis panel" 怎么写 —— main.js 是 GPUI View、可用 imports 是
  `gpui-kit` / `gpui-base` / `gpui-component` / `ducklocal` 四个裸说明符、
  `query(sql, limit)` / `panelDir()` / `sqlLiteral()` 的签名与语义 —— 一个字都没有。
- 唯一学习路径是通读 `examples/analysis_app/main.js`（689 行）。对于
  「我只想要个 HTML 报表」的用户，这个认知负担过重。
- `gpui-kit`、`gpui-kit-design-guides` 两个 skill 与 ducklocal skill 之间没有互相引用，
  发现它们要靠 `ls skills/`。
- **建议**：SKILL.md 增加一节 "Authoring a panel"（最小可运行骨架 20 行 + query() 契约
  + 导出时哪些东西会/不会进 HTML），或提供 `ducklocal dash init <dir>` 脚手架命令。

## 卡点 2：dash export 的产出语义要试一次才知道

- 文档原文："The report holds the panel's data — tables, and a bar chart where a result
  is a name and a number per row — never its layout or interactive state."
  实际含义（试验后才知道）：**HTML 只含 `query()` 捕获的结果集**，按调用顺序编号为
  "Statement 1..N"；面板 render 出来的 KPI 卡片、JS 常量渲染的表格**全部不出现**。
- 我的仪表盘里「v38 删除批解码状态表」是 JS 常量，第一版导出直接缺失。
   workaround：把常量改写成 `SELECT * FROM (VALUES (...), ...) AS t(...)` 发一次 query()，
  纯粹为了让它被捕获。能工作，但说明导出模型对作者不直观。
- **建议**：①文档明确 "only `query()` results are exported, in call order"；
  ②给 statement 加标题机制（如 `query(sql, { title })` 或 SQL 注释约定 `-- title: …`），
  现在每节标题只有 "Statement N"，多节报表可读性差；③考虑导出 panel 声明的静态段。

## 卡点 3：柱状图触发规则不透明

- "a name and a number per row" —— 哪列当 label、哪列当 value、>2 列时是否退化为表格，
  没有写明。我的 `(label, count)` 两列恰好触发了内联 SVG 条形图（效果不错）；
  8 列 × 34 行的历史表自动成了 `<table>`（也符合预期），但规则是猜出来的。
- **建议**：一句话写清判定规则（如 "恰好两列且第二列为数值 → 横向条形图"）。

## 卡点 4：JSONL / 嵌套 JSON 没有指引

- 真实数据是 JSONL 且字段嵌套（`$.fault_category.major_category`、
  `$.root_cause_top5[0].network_element_id`）。SKILL.md 的例子全是扁平 CSV。
  用哪个 reader（`read_json` vs `read_ndjson_objects` vs `read_json_auto`）、
  嵌套字段用 `json_extract_string` + JSON path —— 全靠使用者自己懂 DuckDB。
- **建议**：cli.md 加一小节 JSONL 配方（read_ndjson_objects + json_extract_string 示例）。

## 卡点 5：时间列编码与面板脚手架

- TIMESTAMP/DATE 在 JSON 输出里是 `{"encoding":"timestamp","value":"<纳秒计数>"}`；
  示例面板被迫 `CAST(... AS VARCHAR)`。SKILL.md 列了编码表但没给
  「想直接给人看就 CAST 成字符串」的结论性建议 —— 仪表盘场景几乎必踩。
- `jsconfig.json` / `gpui-kit.d.ts` 要从 examples 手动复制到新面板目录。
  （jsconfig 里的注释解释了为什么不覆盖已存在文件，这个设计好；
  但没有 init 命令，复制这步要靠人肉发现。）

## 做得好的地方（保持）

- JSON 输出契约干净：`columns/rows/row_count/truncated/elapsed_ms`，
  错误走 stderr + 退出码区分 argument/sql/database/io/panel，脚本化非常友好。
- `profile` 的 `decimals` / `max_over_median` / `missing_days` 是别处没有的实用设计。
- dash export 一次跑成：自包含 HTML、内联 SVG 条形图、light/dark 双主题 CSS、
  每条 statement 附 SQL 原文 + 耗时 + 列类型，可审计性好。
- `--out` 目标已存在时拒绝覆盖、需显式 `--force`；只读默认 + COPY 副作用警告到位。

## 补充（聚焦版返工时又踩到的两个坑，2026-09-20）

6. **柱状图有未文档化的行数上限**：`(label, number)` 结果集在 15 行时渲染为内联 SVG
   条形图，34 行时静默退化为 `<table>`——没有任何提示说明阈值存在。聚焦改版的
   分数历程图因此被迫砍到「最近 12 次」。**建议**：写明阈值（看起来在 16~33 之间），
   或提供显式控制。

7. **面板内 SQL 语法错误在 export 里静默**：一处 `CAST x AS T`（漏括号）导致首个
   query 失败，export 输出 `{"queries":1,"rows":0,"panel_errors":0}`——panel_errors
   为 0 但实际上一条语句都没跑出来（面板 catch 后走 error 分支，错误不进 report）。
   **建议**：`panel_errors` 应覆盖「面板进入 error 态/零语句」的情况，或至少在
   report 里留一条 warning。附带的正面发现：SQL 首行注释会显示在 `<pre class="sql">`
   里，可当作节标题 workaround——建议升级为正式能力（`-- title:` 约定）。
