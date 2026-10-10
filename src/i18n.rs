//! UI string internationalization (zh / en), hand-rolled with no extra
//! dependencies.
//!
//! Every user-visible string lives in the `STRINGS` table below as
//! `(key, zh, en)`. Keys describe stable intent (`dialog.export.title`),
//! never a source-language sentence; each locale gets a full-sentence
//! template with sequential `{}` placeholders for dynamic values.
//!
//! Usage in views: `i18n::tr("sidebar.tab.schema")` for plain strings,
//! `i18n::trf("status_bar.last_query", &[&elapsed, &rows, &cols])` for
//! templates (extra `{}` placeholders are left untouched).
//!
//! Language switching (for the title_bar language-toggle UI):
//! call `i18n::set_language(Language::En)` — it updates the global and
//! persists the choice to the history store (blocking write, fine from a
//! click handler) — then call `cx.refresh_windows()` so every view
//! re-renders in the new language without per-view subscriptions.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Language {
    Zh,
    En,
}

impl Language {
    pub fn code(&self) -> &'static str {
        match self {
            Language::Zh => "zh",
            Language::En => "en",
        }
    }

    /// Unknown codes fall back to Chinese, the app's original language.
    pub fn from_code(code: &str) -> Language {
        match code {
            "en" => Language::En,
            _ => Language::Zh,
        }
    }
}

static CURRENT: RwLock<Language> = RwLock::new(Language::Zh);

pub fn current() -> Language {
    *CURRENT.read().unwrap_or_else(|e| e.into_inner())
}

pub fn set_current(lang: Language) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = lang;
}

/// Language to use at startup: the persisted `language` setting when the
/// history store is reachable, otherwise the detected system locale. Never
/// persists anything — that only happens on an explicit user switch.
pub fn initial_language() -> Language {
    if let Ok(Some(code)) = crate::history::get_setting("language") {
        return Language::from_code(&code);
    }
    detect_system_language()
}

/// Best-effort system locale detection: LC_ALL / LANG containing "zh"
/// (`zh_CN.UTF-8`, `zh-Hans`, …) means Chinese, anything else English.
pub fn detect_system_language() -> Language {
    let locale = std::env::var("LC_ALL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_default()
        .to_lowercase();
    if locale.contains("zh") {
        Language::Zh
    } else {
        Language::En
    }
}

/// Switch the UI language and persist the choice. Blocking (writes to the
/// history store); callers on the UI thread should follow with
/// `cx.refresh_windows()` to repaint all views in the new language.
pub fn set_language(lang: Language) {
    set_current(lang);
    if let Err(e) = crate::history::set_setting("language", lang.code()) {
        tracing::warn!("Failed to persist language setting: {e}");
    }
}

static ZH: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| STRINGS.iter().map(|(key, zh, _)| (*key, *zh)).collect());
static EN: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| STRINGS.iter().map(|(key, _, en)| (*key, *en)).collect());

/// Translate `key` in the current language, falling back to the zh value,
/// then to the key itself. Never panics on a missing key.
pub fn tr(key: &'static str) -> &'static str {
    translate(current(), key)
}

fn translate(lang: Language, key: &'static str) -> &'static str {
    let map = match lang {
        Language::Zh => &*ZH,
        Language::En => &*EN,
    };
    map.get(key).or_else(|| ZH.get(key)).copied().unwrap_or(key)
}

/// Translate `key` and substitute sequential `{}` placeholders with `args`
/// in order. Leftover `{}` placeholders stay as-is; extra args are ignored.
pub fn trf(key: &'static str, args: &[&str]) -> String {
    format_template(tr(key), args)
}

/// [`trf`] in a given language rather than the UI's: the CLI speaks English
/// whatever the window does.
pub fn trf_in(lang: Language, key: &'static str, args: &[&str]) -> String {
    format_template(translate(lang, key), args)
}

fn format_template(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    for arg in args {
        match rest.split_once("{}") {
            Some((before, after)) => {
                out.push_str(before);
                out.push_str(arg);
                rest = after;
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// `(key, zh, en)` for every user-visible string, grouped by source file.
static STRINGS: &[(&str, &str, &str)] = &[
    // ── Common ──────────────────────────────────────────────────────────
    ("common.cancel", "取消", "Cancel"),

    // ── src/ui/sidebar.rs ───────────────────────────────────────────────
    ("sidebar.tab.schema", "表结构", "Schema"),
    ("sidebar.tab.history", "查询历史", "History"),
    ("sidebar.tab.extensions", "扩展", "Extensions"),
    ("sidebar.extensions.refresh", "刷新扩展列表", "Refresh extensions"),
    ("sidebar.extensions.loading", "正在读取扩展…", "Reading extensions…"),
    ("sidebar.extensions.status.loaded", "已加载", "Loaded"),
    ("sidebar.extensions.status.installed", "已安装", "Installed"),
    ("sidebar.extensions.status.available", "未安装", "Not installed"),
    ("sidebar.extensions.origin.builtin", "内置", "built in"),
    ("sidebar.extensions.install", "安装", "Install"),
    ("sidebar.extensions.load", "加载", "Load"),
    ("sidebar.extensions.update", "更新", "Update"),
    ("sidebar.extensions.installed_notice", "已安装 {}", "Installed {}"),
    ("sidebar.extensions.loaded_notice", "已加载 {}", "Loaded {}"),
    ("sidebar.extensions.updated_notice", "已更新 {}", "Updated {}"),
    ("sidebar.extensions.failed", "{} 操作失败：{}", "{} failed: {}"),
    ("sidebar.refresh_schema", "刷新 Schema", "Refresh schema"),
    ("sidebar.collapse", "收起侧栏", "Hide sidebar"),
    ("sidebar.expand", "展开侧栏", "Show sidebar"),
    (
        "sidebar.schema.empty",
        "当前没有表或视图。\n拖入数据文件、通过“打开数据…”导入，或运行 CREATE TABLE 后点击刷新。",
        "No tables or views yet.\nDrop a data file in, import one from “Open data…”, or run CREATE TABLE and click refresh.",
    ),
    (
        "sidebar.history.empty",
        "还没有查询记录。\n运行一条查询后会显示在这里。",
        "No queries yet.\nQueries you run will show up here.",
    ),
    ("sidebar.history.rows", "{} 行", "{} rows"),
    ("sidebar.history.failed", "失败", "Failed"),
    ("sidebar.group.local_files", "本地文件", "Local files"),
    ("sidebar.group.dashboards", "仪表盘", "Dashboards"),
    ("sidebar.s3.configured", "已配置", "Configured"),
    ("sidebar.s3.loading", "加载中…", "Loading…"),
    ("sidebar.s3.empty", "（空）", "(Empty)"),
    ("sidebar.s3.load_failed", "加载失败：{}", "Failed to load: {}"),
    ("sidebar.s3.refresh_buckets", "刷新 bucket 列表", "Refresh bucket list"),
    ("sidebar.table.generate_select", "生成 SELECT 查询", "Generate SELECT query"),
    ("sidebar.column.edit_type", "修改数据类型", "Change data type"),
    ("sidebar.menu.copy_name", "复制名称", "Copy name"),
    ("sidebar.menu.copy_table_info", "复制表信息", "Copy table info"),
    ("sidebar.menu.remove_recent", "从列表移除", "Remove from list"),
    ("sidebar.file.remove", "从本地文件移除", "Remove from local files"),
    ("sidebar.menu.overview", "列概览", "Column overview"),
    ("sidebar.database.read_only", "只读", "read-only"),
    ("sidebar.database.use", "设为默认数据库", "Use as default database"),
    ("sidebar.database.detach", "分离数据库", "Detach database"),
    (
        "sidebar.recent.remove",
        "从列表移除（文件不受影响）",
        "Remove from this list (the files stay)",
    ),
    (
        "sidebar.recents.gone",
        "{} 已不存在，已从最近列表移除",
        "{} no longer exists; removed from recents",
    ),
    ("dialog.remove_file.title", "移除“{}”？", "Remove “{}”?"),
    (
        "dialog.remove_file.description",
        "将从本地文件注册表移除，并删除当前数据库中的视图；原始文件不受影响。",
        "This removes the file from the local registry and drops its view from the current database. The original file is not affected.",
    ),
    ("dialog.remove_file.confirm", "移除", "Remove"),
    ("dialog.alter_type.title", "修改 {} 的数据类型", "Change data type of {}"),
    ("dialog.alter_type.current_type", "{}.{} · 当前类型 {}", "{}.{} · current type {}"),
    ("dialog.alter_type.confirm", "修改", "Change"),
    ("notify.alter_type.success", "已将 {} 的类型修改为 {}", "Changed type of {} to {}."),
    ("notify.alter_type.failed", "修改数据类型失败：{}", "Failed to change data type: {}"),

    // ── src/ui/results.rs ───────────────────────────────────────────────
    ("results.tab.table", "结果", "Results"),
    ("results.tab.chart", "图表", "Chart"),
    ("results.tab.overview", "概览", "Overview"),
    (
        "results.overview.summary",
        "{} · {} 行 · {} 列 · 耗时 {}",
        "{} · {} rows · {} columns · took {}",
    ),
    ("results.overview.query", "当前查询结果", "Current result"),
    ("results.overview.running", "正在统计每一列…", "Summarizing every column…"),
    (
        "results.overview.empty",
        "运行一个查询，或在侧栏右键表、视图、文件选择“列概览”",
        "Run a query, or right-click a table, view or file in the sidebar and choose Column overview",
    ),
    (
        "results.overview.not_query",
        "只有 SELECT / WITH / FROM 这类查询的结果可以概览：概览会把查询再运行一次。",
        "Only the result of a query (SELECT, WITH, FROM…) can be summarized: the overview runs it again.",
    ),
    ("results.overview.nulls", "空值 {}", "{} null"),
    ("results.overview.distinct", "约 {} 个不同值", "~{} distinct"),
    ("results.overview.median", "中位数 {}", "median {}"),
    ("results.overview.estimated", "高频值（估算）", "Common values (estimated)"),
    ("results.summary", "{} 行 · {} 列", "{} rows · {} columns"),
    ("results.export_csv", "导出 CSV", "Export CSV"),
    ("results.export_parquet", "导出 Parquet", "Export Parquet"),
    ("results.running", "正在运行查询…", "Running query…"),
    ("results.truncated", "结果已截断到 {} 行。", "Results truncated to {} rows."),
    ("results.affected", "完成 · {} 行受影响 · 耗时 {}", "Done · {} rows affected · took {}"),
    ("results.failed.title", "查询失败", "Query failed"),
    ("results.explain.elapsed", "EXPLAIN · 耗时 {}", "EXPLAIN · took {}"),
    (
        "results.profile.summary",
        "性能分析 · 总耗时 {} · 算子合计 {}",
        "Profile · took {} · operators {}",
    ),
    ("results.profile.hottest", "最耗时：{}（{}）", "Hottest: {} ({})"),
    ("results.profile.rows", "{} 行", "{} rows"),
    ("results.profile.estimated", "预估 {}", "est. {}"),
    (
        "results.profile.misestimate",
        "预估与实际相差超过 10 倍",
        "Estimate off by more than 10×",
    ),
    ("results.empty.title", "运行查询以查看结果", "Run a query to see results"),
    ("results.script.count", "{} 条语句", "{} statements"),
    ("results.script.rows", "{} 行", "{} rows"),
    ("results.script.done", "完成", "done"),
    ("results.script.failed", "失败", "failed"),
    ("results.script.skipped", "未执行", "not run"),
    ("results.empty_chart.title", "运行查询以查看图表", "Run a query to see a chart"),
    // The empty-state hint wraps a Kbd widget: prefix, keystroke, suffix.
    ("results.empty.hint_prefix", "在编辑器中输入 SQL，按", "Type SQL in the editor and press"),
    ("results.empty.hint_suffix", "运行", "to run"),
    ("results.filter.placeholder", "过滤结果…", "Filter results…"),
    ("results.filter.counts", "{} / {} 行", "{} / {} rows"),
    ("results.cell.copy", "复制", "Copy"),
    ("dialog.export.title", "导出 {}", "Export {}"),
    (
        "dialog.export.description",
        "将当前查询结果以 {} 格式写入目标路径。",
        "Write the current query result to the target path in {} format.",
    ),
    ("dialog.export.confirm", "导出", "Export"),
    ("notify.export.empty_path", "导出路径不能为空。", "Export path cannot be empty."),
    ("notify.export.success", "已导出到 {}", "Exported to {}."),
    ("notify.export.failed", "导出失败：{}", "Export failed: {}"),

    // ── src/ui/title_bar.rs ─────────────────────────────────────────────
    ("title_bar.open_data", "打开数据", "Open data"),
    ("title_bar.open", "打开", "Open"),
    ("title_bar.open.files", "打开文件", "Open files"),
    ("title_bar.open.folder", "打开文件夹", "Open folder"),
    ("title_bar.open.path", "输入路径或通配符", "Enter a path or glob"),
    ("title_bar.open.attach_database", "附加数据库…", "Attach database…"),
    ("title_bar.open.s3", "连接 S3", "Connect S3"),
    ("title_bar.open.memory", "新建内存数据库", "New in-memory database"),
    ("title_bar.configure_s3", "配置 S3 数据源（httpfs）", "Configure S3 source (httpfs)"),
    ("title_bar.toggle_theme", "切换明暗主题", "Toggle light/dark theme"),
    ("title_bar.toggle_language", "切换语言", "Switch language"),
    (
        "title_bar.ui_size",
        "界面大小",
        "Interface size",
    ),
    ("ui_size.small", "小", "Small"),
    ("ui_size.default", "默认", "Default"),
    ("ui_size.large", "大", "Large"),
    ("ui_size.xlarge", "特大", "Extra large"),
    ("dialog.s3.title", "配置 S3 数据源", "Configure S3 source"),
    (
        "dialog.s3.description",
        "配置后可在侧栏展开 S3 浏览 bucket 与文件，或直接查询 s3://bucket/路径。凭据仅当前会话有效，不会写入磁盘；切换数据库连接后需重新配置。",
        "Once configured, you can browse buckets and files under S3 in the sidebar, or query s3://bucket/path directly. Credentials are session-only and never written to disk; configure them again after switching database connections.",
    ),
    ("dialog.s3.field.endpoint", "Endpoint", "Endpoint"),
    ("dialog.s3.field.region", "Region", "Region"),
    ("dialog.s3.field.access_key_id", "Access Key ID", "Access Key ID"),
    ("dialog.s3.field.secret_access_key", "Secret Access Key", "Secret Access Key"),
    ("dialog.s3.confirm", "启用 S3", "Enable S3"),
    (
        "notify.s3.missing_keys",
        "Access Key ID 与 Secret Access Key 不能为空。",
        "Access Key ID and Secret Access Key cannot be empty.",
    ),
    (
        "notify.s3.configured",
        "S3 已配置，可在侧栏浏览 bucket 或查询 s3:// 路径",
        "S3 configured. Browse buckets in the sidebar or query s3:// paths.",
    ),
    ("notify.s3.failed", "S3 配置失败：{}", "Failed to configure S3: {}"),
    ("dialog.open_source.title", "打开数据", "Open data"),
    ("dialog.attach_database.title", "附加数据库", "Attach database"),
    (
        "dialog.attach_database.description",
        "把另一个 DuckDB 或 SQLite 数据库文件和当前数据库一起打开，用 名称.表 跨库查询。下次启动会自动重新附加。",
        "Open another DuckDB or SQLite database beside the current one and query across them as name.table. It is attached again on every launch.",
    ),
    (
        "dialog.attach_database.picker_prompt",
        "选择数据库文件",
        "Choose a database file",
    ),
    ("dialog.attach_database.read_only", "只读", "Read-only"),
    ("dialog.attach_database.attach", "附加", "Attach"),
    (
        "dialog.open_source.description",
        "输入路径，或浏览选择数据文件、文件夹或 .duckdb 数据库。数据文件会作为视图加入当前连接，文件夹会递归展开。输入新的 .duckdb 路径可创建数据库及其父目录。",
        "Enter a path, or browse to a data file, a folder, or a .duckdb database. Data files join the current connection as views; folders are expanded recursively. Enter a new .duckdb path to create a database and its parent folders.",
    ),
    ("dialog.open_source.browse", "浏览", "Browse"),
    ("dialog.open_source.s3", "S3", "S3"),
    (
        "dialog.open_source.picker_prompt",
        "选择数据文件、文件夹或数据库",
        "Choose data files, a folder, or a database",
    ),
    ("dialog.open_source.memory", "内存模式", "In-memory"),
    ("dialog.open_source.open_file", "打开", "Open"),
    ("notify.attach.success", "已创建视图 {}", "Created view {}."),
    ("notify.attach.count", "已导入 {} 个数据文件", "Imported {} data files."),
    ("notify.attach.truncated", "文件过多，只导入了前 {} 个。", "Too many files; imported the first {}."),
    ("notify.attach.more_failed", "另有 {} 个文件导入失败。", "{} more files could not be imported."),
    ("notify.attach.failed", "无法导入数据文件：{}", "Could not import data file: {}"),
    (
        "error.database_already_attached",
        "这个数据库已经打开了，名为 {}。",
        "This database is already open, as {}.",
    ),
    (
        "error.database_alias_taken",
        "无法附加 {}：名称 {} 已被当前数据库占用。",
        "Could not attach {}: the name {} is taken in the open database.",
    ),
    ("notify.database.attached", "已附加数据库 {}", "Attached database {}"),
    ("notify.database.attach_failed", "无法附加数据库：{}", "Could not attach database: {}"),
    ("notify.database.detached", "已分离数据库 {}", "Detached database {}"),
    ("notify.database.detach_failed", "无法分离数据库：{}", "Could not detach database: {}"),
    (
        "error.relative_attachment",
        "无法恢复相对路径 {}：原工作目录未知。请移除该记录并重新打开文件。",
        "Cannot restore relative path {}: its original working directory is unknown. Remove the registration and open the file again.",
    ),
    ("notify.connect.success", "已连接到 {}", "Connected to {}."),
    ("notify.connect.failed", "无法打开数据库：{}", "Could not open database: {}"),

    // ── src/sources.rs ──────────────────────────────────────────────────
    ("error.glob_no_match", "没有匹配的文件：{}", "No files match: {}"),
    (
        "error.no_data_files",
        "目录中没有可导入的数据文件：{}",
        "No data files in that folder: {}",
    ),

    // ── src/ui/workspace.rs ─────────────────────────────────────────────
    (
        "workspace.welcome_sql",
        "SELECT '你好，DuckDB' AS greeting;",
        "SELECT 'Hello, DuckDB' AS greeting;",
    ),
    ("workspace.tab.default_title", "查询 {}", "Query {}"),
    ("workspace.new_query", "新建查询", "New query"),
    ("workspace.run", "运行", "Run"),
    ("workspace.run.tooltip", "运行查询", "Run query"),
    ("workspace.stop", "停止", "Stop"),
    ("menu.quit", "退出 DuckLocal", "Quit DuckLocal"),
    ("setup.open", "命令行与 AI", "Command line & AI"),
    ("setup.title", "命令行与 AI", "Command line & AI"),
    (
        "setup.offer",
        "安装 ducklocal 命令，就能在终端里查询数据，也能交给 AI 助手分析。",
        "Install the ducklocal command to query data from the terminal and let AI agents analyze it.",
    ),
    ("setup.offer.action", "设置", "Set up"),
    ("setup.command.heading", "终端命令", "Terminal command"),
    (
        "setup.command.description",
        "安装后可在任意终端运行 ducklocal，查询文件并得到 JSON 结果。",
        "Run ducklocal in any terminal to query files and get JSON back.",
    ),
    ("setup.command.installed", "已安装在 {}", "Installed at {}"),
    (
        "setup.command.elsewhere",
        "{} 指向另一个 DuckLocal；重新安装会改为指向这一个。",
        "{} points to another copy of DuckLocal; installing again points it here.",
    ),
    ("setup.command.not_installed", "尚未安装", "Not installed"),
    ("setup.command.install", "安装命令", "Install command"),
    ("setup.command.reinstall", "重新安装", "Reinstall"),
    ("setup.ai.heading", "AI 助手", "AI agents"),
    (
        "setup.ai.description",
        "让 AI 助手用 DuckLocal 分析你的数据：先读结构，再用 SQL 计算，基于真实结果回答。",
        "Let an AI agent analyze your data with DuckLocal: it reads the schema, computes in SQL and answers from real results.",
    ),
    ("setup.ai.claude", "Claude Code", "Claude Code"),
    ("setup.ai.claude.installed", "已添加 DuckLocal 技能", "DuckLocal skill added"),
    ("setup.ai.claude.outdated", "技能有新版本", "A newer skill is available"),
    ("setup.ai.claude.missing", "尚未添加技能", "Skill not added"),
    ("setup.ai.claude.install", "添加到 Claude Code", "Add to Claude Code"),
    ("setup.ai.claude.update", "更新技能", "Update skill"),
    ("setup.ai.other", "其他 AI 助手", "Other agents"),
    (
        "setup.ai.other.description",
        "复制一段说明，粘贴给 Codex、Cursor 或任何 AI 助手。",
        "Copy a brief to paste into Codex, Cursor or any other agent.",
    ),
    ("setup.ai.other.copy", "复制说明", "Copy brief"),
    (
        "setup.error.move_to_applications",
        "请先把 DuckLocal 拖到“应用程序”文件夹，再安装命令。",
        "Move DuckLocal to the Applications folder first, then install the command.",
    ),
    (
        "setup.error.not_ours",
        "{} 已存在，且不是 DuckLocal 创建的，未作改动。",
        "{} already exists and was not made by DuckLocal, so it was left alone.",
    ),
    ("setup.error.cancelled", "已取消安装。", "Installation cancelled."),
    (
        "notify.setup.command_installed",
        "已安装 ducklocal 命令，打开新的终端即可使用。",
        "Installed the ducklocal command. Open a new terminal to use it.",
    ),
    (
        "notify.setup.command_needs_path",
        "已安装在 {}，但该目录不在 PATH 中；把它加入 PATH 后即可使用。",
        "Installed at {}, but that folder is not on your PATH; add it to use the command.",
    ),
    ("notify.setup.command_failed", "无法安装命令：{}", "Could not install the command: {}"),
    (
        "notify.setup.skill_installed",
        "已添加到 Claude Code，新会话即可使用：{}",
        "Added to Claude Code; new sessions can use it: {}",
    ),
    ("notify.setup.skill_failed", "无法添加技能：{}", "Could not add the skill: {}"),
    (
        "notify.setup.prompt_copied",
        "已复制，粘贴给你的 AI 助手即可。",
        "Copied. Paste it into your agent.",
    ),
    ("menu.file", "文件", "File"),
    ("menu.edit", "编辑", "Edit"),
    ("menu.query", "查询", "Query"),
    ("menu.view", "显示", "View"),
    ("menu.close_tab", "关闭标签页", "Close tab"),
    ("menu.undo", "撤销", "Undo"),
    ("menu.redo", "重做", "Redo"),
    ("menu.cut", "剪切", "Cut"),
    ("menu.copy", "拷贝", "Copy"),
    ("menu.paste", "粘贴", "Paste"),
    ("menu.select_all", "全选", "Select all"),
    ("menu.toggle_sidebar", "显示或隐藏侧边栏", "Show or hide sidebar"),
    ("menu.zoom_in", "放大界面", "Larger interface"),
    ("menu.zoom_out", "缩小界面", "Smaller interface"),
    ("menu.zoom_reset", "默认大小", "Default size"),
    (
        "menu.check_updates",
        "检查更新…",
        "Check for Updates…",
    ),
    (
        "menu.check_updates_at_launch",
        "启动时检查更新",
        "Check for Updates at Launch",
    ),
    ("status_bar.update.check", "检查更新", "Check for updates"),
    ("status_bar.update.checking", "正在检查更新…", "Checking for updates…"),
    ("status_bar.update.latest", "已是最新", "Up to date"),
    ("status_bar.update.available", "有新版本 {}", "Update available: {}"),
    ("status_bar.update.download", "打开下载页", "Open the download page"),
    ("status_bar.update.failed", "检查更新失败", "Update check failed"),
    (
        "menu.base_map",
        "在线底图（OpenStreetMap）",
        "Online base map (OpenStreetMap)",
    ),
    ("workspace.stop.tooltip", "中断正在运行的查询", "Interrupt the running query"),
    ("workspace.format", "格式化", "Format"),
    ("workspace.format.tooltip", "格式化当前 SQL", "Format current SQL"),
    ("workspace.explain", "执行计划", "Explain"),
    ("workspace.explain.tooltip", "查看查询计划", "View query plan"),
    ("workspace.profile", "性能分析", "Profile"),
    (
        "workspace.profile.tooltip",
        "运行查询并查看每个算子的耗时（EXPLAIN ANALYZE）",
        "Run the query and see what each operator cost (EXPLAIN ANALYZE)",
    ),
    (
        "query.profile.read_only",
        "只能分析读取数据的语句（SELECT、WITH、FROM 等）：分析会真正执行语句。",
        "Only statements that read can be profiled (SELECT, WITH, FROM…): profiling runs the statement.",
    ),
    (
        "query.single_statement",
        "一次只能对一条语句执行计划或分析：把要看的那条单独放进一个标签页。",
        "Explain and Profile take one statement: put the one to look at in a tab of its own.",
    ),
    ("workspace.rename.tooltip", "双击重命名", "Double-click to rename"),
    ("workspace.empty.title", "把数据拖进来", "Drop your data in"),
    (
        "workspace.empty.description",
        "拖入 CSV、TSV、Excel、Parquet、JSON 文件，或整个文件夹。数据只留在这台机器上。",
        "Drag in CSV, TSV, Excel, Parquet, or JSON files — or a whole folder. Your data stays on this machine.",
    ),
    ("workspace.empty.open_files", "打开文件", "Open files"),
    ("workspace.empty.open_folder", "打开文件夹", "Open folder"),
    ("workspace.empty.files_prompt", "选择数据文件", "Choose data files"),
    ("workspace.empty.folder_prompt", "选择数据文件夹", "Choose a data folder"),
    (
        "workspace.empty.cli_hint",
        "也可以从命令行打开：ducklocal ./logs/",
        "Or from a terminal: ducklocal ./logs/",
    ),

    // ── src/ui/chart.rs ─────────────────────────────────────────────────
    (
        "chart.notice.series_capped",
        "仅显示前 {} / {} 个数值列",
        "Showing the first {} of {} numeric columns",
    ),
    (
        "chart.notice.downsampled",
        "已降采样：{} 点 → {} 点（每点为 {} 个采样的均值）",
        "Downsampled: {} points → {} points (each point is the average of {} samples)",
    ),
    (
        "chart.notice.bars_capped",
        "仅显示前 {} / {} 行",
        "Showing the first {} of {} rows",
    ),
    (
        "chart.empty.no_numeric",
        "当前结果没有数值列，无法绘制图表。",
        "This result has no numeric columns to chart.",
    ),
    ("chart.empty.detected_columns", "检测到的列：{}", "Detected columns: {}"),
    (
        "chart.empty.detected_more",
        "{} … 等 {} 列",
        "{} … and {} more columns",
    ),
    (
        "chart.empty.hint",
        "提示：文本列可用 cast(列名 AS DOUBLE) 或聚合函数（如 avg/sum/count）转换为数值。",
        "Tip: cast a text column with cast(column AS DOUBLE), or use an aggregate such as avg/sum/count.",
    ),
    ("chart.empty.no_rows", "没有可绘制的数据行。", "No data rows to plot."),
    ("chart.title.series_more", "{} … 共 {} 列", "{} … {} columns total"),
    ("chart.title.by", "{}（按 {}）", "{} by {}"),
    ("chart.title.point_count", "绘制 {} 点", "{} points plotted"),
    ("chart.kind.auto", "自动", "Auto"),
    ("chart.kind.bar", "柱状图", "Bar"),
    ("chart.kind.line", "折线图", "Line"),
    ("chart.kind.area", "面积图", "Area"),
    ("chart.kind.scatter", "散点图", "Scatter"),
    ("chart.kind.pie", "饼图", "Pie"),
    ("chart.kind.map", "地图", "Map"),
    ("chart.pie.title", "{} 占比（按 {}）", "Share of {} by {}"),
    ("chart.pie.total", "合计 {}", "{} total"),
    (
        "chart.pie.no_positive",
        "饼图需要正数值，当前第一个数值列没有正数。",
        "A pie needs positive values; the first numeric column has none.",
    ),
    (
        "chart.pie.notice.skipped",
        "{} 行为零、负数或空值，未计入",
        "{} rows with zero, negative or missing values left out",
    ),

    // ── src/ui/geo.rs ───────────────────────────────────────────────────
    ("chart.map.title", "地图（{} / {}）", "Map of {} / {}"),
    ("chart.map.colored_by", "按 {} 着色", "Colored by {}"),
    ("chart.map.sized_by", "按 {} 定大小", "Sized by {}"),
    (
        "chart.map.notice.unsized",
        "{} 个点没有可用的大小值，按最小绘制",
        "{} points have no usable size value; drawn smallest",
    ),
    ("chart.map.other", "其他", "Other"),
    ("chart.map.row", "第 {} 行", "Row {}"),
    ("chart.map.no_label", "（结果中没有名称列）", "(no name column in the result)"),
    (
        "chart.map.notice.dropped",
        "{} 行缺少有效经纬度，未绘制",
        "{} rows without valid coordinates not plotted",
    ),
    (
        "chart.map.notice.capped",
        "仅绘制前 {} / {} 个点",
        "Plotting the first {} of {} points",
    ),

    // ── src/script.rs ───────────────────────────────────────────────────
    ("script.dot.help.tables", "列出所有表和视图，可用 ILIKE 模式过滤（如 stat%）", "List tables and views, optionally filtered by an ILIKE pattern (e.g. stat%)"),
    ("script.dot.help.schema", "显示表和视图的 CREATE 语句，可用模式过滤", "Show the CREATE statements of tables and views, optionally filtered"),
    ("script.dot.help.databases", "列出已挂载的数据库", "List attached databases"),
    ("script.dot.help.help", "显示本帮助", "Show this help"),
    ("script.dot.unknown", "不支持的点命令 {}。支持：{}", "Unsupported dot command {}. Supported: {}"),
    ("script.dot.no_args", "{} 不接受参数", "{} takes no arguments"),
    ("script.dot.too_many_args", "{} 最多接受一个模式参数", "{} takes at most one pattern"),

    // ── src/storage.rs ──────────────────────────────────────────────────
    (
        "storage.too_new",
        "这个数据库文件由 DuckDB {} 创建，用的存储格式比 DuckLocal 内置的 DuckDB {} 更新，所以打不开。\n\n用创建它的那个版本的 DuckDB 把它复制成兼容格式，再打开新文件：\n\nATTACH {} AS src (READ_ONLY);\nATTACH 'compat.duckdb' AS dst (STORAGE_VERSION '{}');\nCOPY FROM DATABASE src TO dst;",
        "This database was created by DuckDB {}, in a storage format newer than DuckLocal's built-in DuckDB {} can read.\n\nCopy it into a compatible format with the DuckDB that created it, then open the copy:\n\nATTACH {} AS src (READ_ONLY);\nATTACH 'compat.duckdb' AS dst (STORAGE_VERSION '{}');\nCOPY FROM DATABASE src TO dst;",
    ),
    (
        "storage.too_old",
        "这个数据库文件来自 DuckDB v0.9 或更早的版本，DuckDB {} 已经读不了这种存储格式。\n\n用原来的 DuckDB 版本执行 EXPORT DATABASE 'dir'，再在这里用 IMPORT DATABASE 'dir' 导入。",
        "This database comes from DuckDB v0.9 or earlier, a storage format DuckDB {} no longer reads.\n\nRun EXPORT DATABASE 'dir' with the DuckDB that wrote it, then IMPORT DATABASE 'dir' here.",
    ),
    (
        "storage.sqlite",
        "这是 SQLite 数据库，不是 DuckDB 文件。可以在查询里附加它来读取：\n\nINSTALL sqlite;\nATTACH {} (TYPE sqlite);",
        "This is a SQLite database, not a DuckDB file. Attach it in a query to read it:\n\nINSTALL sqlite;\nATTACH {} (TYPE sqlite);",
    ),
    (
        "storage.not_duckdb",
        "这个文件不是 DuckDB 数据库文件。CSV、Parquet、JSON、Excel 文件请用“打开数据”作为数据文件附加。",
        "This file is not a DuckDB database. Open CSV, Parquet, JSON or Excel files as data files instead.",
    ),
    (
        "storage.tooltip",
        "存储格式：DuckDB {} 可读",
        "Storage format: readable by DuckDB {}",
    ),
    ("storage.tooltip.created_by", "由 DuckDB {} 创建", "Created by DuckDB {}"),
    // ── src/ui/status_bar.rs ────────────────────────────────────────────
    ("status_bar.connected", "已连接", "Connected"),
    (
        "status_bar.search_path",
        "当前数据库.schema：不带库名的表名在这里查找，可用 USE 切换",
        "Current database.schema: unqualified names resolve here; switch with USE",
    ),
    ("status_bar.disconnected", "未连接", "Disconnected"),
    ("status_bar.opening", "正在打开数据源…", "Opening data source…"),
    (
        "status_bar.last_query",
        "耗时 {} · {} 行 · {} 列",
        "Took {} · {} rows · {} columns",
    ),

    // ── src/app.rs ──────────────────────────────────────────────────────
    (
        "notify.init_memory.failed",
        "初始化内存数据库失败：{}",
        "Failed to initialize the in-memory database: {}",
    ),
    ("notify.open.failed", "打开数据失败：{}", "Could not open the data: {}"),

    // ── src/db.rs ───────────────────────────────────────────────────────
    ("error.file_not_found", "文件不存在: {}", "File not found: {}"),
    (
        "error.relation_taken",
        "未挂载 {}：当前数据库已有名为 \"{}\" 的表或视图，DuckLocal 不会替换它。",
        "Not attached {}: the database already has a table or view named \"{}\", which DuckLocal will not replace.",
    ),
    ("error.unsupported_file_type", "不支持的文件类型: {}", "Unsupported file type: {}"),
    (
        "error.excel_empty",
        "Excel 文件中没有可导入的工作表: {}",
        "Workbook has no importable sheets: {}",
    ),
    (
        "error.view_name_derivation",
        "无法从路径推导视图名: {}",
        "Cannot derive a view name from the path: {}",
    ),

    // ── src/s3.rs ───────────────────────────────────────────────────────
    (
        "error.s3.request_failed",
        "S3 请求失败（HTTP {}）：{}",
        "S3 request failed (HTTP {}): {}",
    ),

    ("dashboard.reload", "重新加载", "Reload"),
    (
        "dashboard.not_updated",
        "Dashboard 未更新",
        "Dashboard not updated",
    ),
    ("workspace.open_dashboard", "打开 Dashboard", "Open dashboard"),
    ("dashboard.picker.prompt", "选择 .dash 文件", "Choose a .dash file"),
    (
        "dashboard.picker.not_a_spec",
        "{} 不是 .dash 文件",
        "{} is not a .dash file",
    ),
    (
        "workspace.add_tab.tooltip",
        "新建查询，或打开一个 Dashboard",
        "New query, or open a dashboard",
    ),

    // ── src/spec/view.rs ──────────────────────────────────────────────────
    (
        "dashboard.reload.tooltip",
        "重新读取 spec 文件，并重跑它的所有查询",
        "Re-read the spec file and re-run its queries",
    ),
    (
        "dashboard.source.show",
        "查看 spec",
        "View spec",
    ),
    (
        "dashboard.source.back",
        "返回 dashboard",
        "Back to dashboard",
    ),
    (
        "dashboard.source.tooltip",
        "阅读这个 dashboard 的 .dash 源码",
        "Read this dashboard's .dash source",
    ),
    (
        "dashboard.source.unreadable",
        "无法读取 {}：{}",
        "Could not read {}: {}",
    ),
    (
        "dashboard.spec_failed",
        "仪表盘 spec 无法加载",
        "The dashboard spec could not be loaded",
    ),
    (
        "dashboard.plot_failed",
        "这个图没有画出来",
        "This plot could not be drawn",
    ),
    (
        "dashboard.query_missing",
        "plot 引用的查询 {} 不存在",
        "The query {} a plot references does not exist",
    ),
    (
        "dashboard.query_not_rows",
        "查询 {} 没有返回结果集，无法绘图",
        "Query {} returned no result set to plot",
    ),
    (
        "dashboard.invalid_plot",
        "这个 plot 没有声明 y 列，无法绘制。",
        "This plot declares no y column to draw.",
    ),
    (
        "dashboard.missing_column_named",
        "查询结果中没有列 {}。可用的列：{}",
        "The query result has no column {}. Available columns: {}",
    ),
    (
        "dashboard.map_no_coordinates",
        "地图需要经纬度列：请用 lat 和 lng 指定。查询返回的列：{}",
        "A map needs coordinate columns: name them with lat and lng. The query returns: {}",
    ),
    (
        "dashboard.card.first_row",
        "显示的是 {} 行中的第一行",
        "Showing the first of {} rows",
    ),
    (
        "dashboard.card.no_rows",
        "查询没有返回行",
        "The query returned no rows",
    ),
    (
        "dashboard.empty",
        "这个 spec 没有声明任何 plot 块。",
        "This spec declares no plot blocks.",
    ),
    (
        "dashboard.notice.zero_filled",
        "{} 个（系列 × x）组合没有数据，按 0 补齐",
        "{} (series, x) cells had no data and were filled with 0",
    ),
    (
        "dashboard.restore.gone",
        "无法恢复上次打开的仪表盘 {}（{}）：文件已不存在",
        "Could not restore dashboard {} ({}) from last session: the file is gone",
    ),
    (
        "dashboard.save",
        "保存",
        "Save",
    ),
    (
        "dashboard.save.tooltip",
        "把修改写回 .dash 文件并重跑",
        "Write changes back to the .dash file and re-run",
    ),
    (
        "dashboard.save_failed",
        "保存 {} 失败：{}",
        "Could not save {}: {}",
    ),
    (
        "dashboard.conflict",
        "文件在磁盘上已被修改",
        "The file changed on disk",
    ),
    ("dashboard.filter.label", "筛选", "Filtered by"),
    ("dashboard.filter.clear", "清除此筛选", "Clear this filter"),
    ("dashboard.filter.clear_all", "全部清除", "Clear all"),
    ("dashboard.sql.hide", "图表", "Chart"),
    ("dashboard.sql.show_tooltip", "查看这张图的 SQL", "Show this plot's SQL"),
    ("dashboard.sql.hide_tooltip", "回到图表", "Back to the plot"),
    ("dashboard.sql.copy", "复制", "Copy"),
    ("dashboard.sql.open", "在查询页打开", "Open in a query tab"),
    (
        "dashboard.comments.show_tooltip",
        "评论这张图，交给 Agent 审阅修改",
        "Comment on this plot for an agent to review",
    ),
    ("dashboard.comments.placeholder", "写下要改的地方…", "What should change?"),
    ("dashboard.comments.send", "评论", "Comment"),
    ("dashboard.comments.reply", "回复", "Reply"),
    ("dashboard.comments.resolve", "解决", "Resolve"),
    ("dashboard.comments.reopen", "重新打开", "Reopen"),
    ("dashboard.comments.resolved", "已解决", "Resolved"),
    ("dashboard.comments.cancel", "取消", "Cancel"),
    ("dashboard.comments.replying", "回复 {}", "Replying to {}"),
    ("dashboard.comments.shortcut", "Ctrl/⌘ + Enter 发送", "Ctrl/⌘ + Enter to send"),
    ("dashboard.comments.you", "你", "You"),
    ("dashboard.comments.agent", "Agent", "Agent"),
    (
        "dashboard.comments.hint",
        "评论保存在 .dash 文件旁，Agent 用 `ducklocal comments` 读取并回复。",
        "Saved beside the .dash file; an agent reads and answers with `ducklocal comments`.",
    ),
    (
        "dashboard.filter.hint",
        "点击可筛选其他图表",
        "Click to filter the other plots",
    ),
    (
        "dashboard.conflict.hint",
        "你的修改尚未保存；保存会覆盖磁盘上的改动。",
        "You have unsaved edits; saving overwrites the changes on disk.",
    ),
    (
        "dashboard.close_unsaved.title",
        "未保存的修改",
        "Unsaved changes",
    ),
    (
        "dashboard.close_unsaved.body",
        "“{}”有未保存的修改，关闭前要先保存吗？",
        "\"{}\" has unsaved changes. Save before closing?",
    ),
    (
        "dashboard.close_unsaved.save_close",
        "保存并关闭",
        "Save & close",
    ),
    (
        "dashboard.close_unsaved.discard",
        "不保存",
        "Don't save",
    ),
];

#[cfg(test)]
mod tests {
    use super::{format_template, translate, Language};

    #[test]
    fn language_codes_roundtrip() {
        assert_eq!(Language::from_code("zh"), Language::Zh);
        assert_eq!(Language::from_code("en"), Language::En);
        assert_eq!(Language::from_code("fr"), Language::Zh);
        assert_eq!(Language::Zh.code(), "zh");
        assert_eq!(Language::En.code(), "en");
    }

    #[test]
    fn translate_falls_back_to_zh_then_key() {
        assert_eq!(translate(Language::En, "sidebar.tab.schema"), "Schema");
        assert_eq!(translate(Language::Zh, "sidebar.tab.schema"), "表结构");
        assert_eq!(translate(Language::En, "no.such.key"), "no.such.key");
    }

    #[test]
    fn every_key_has_both_locales_and_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for (key, zh, en) in super::STRINGS {
            assert!(seen.insert(key), "duplicate key: {key}");
            assert!(!zh.is_empty(), "empty zh for {key}");
            assert!(!en.is_empty(), "empty en for {key}");
        }
    }

    #[test]
    fn format_template_substitutes_sequentially() {
        assert_eq!(
            format_template("{} rows · {} cols", &["3", "2"]),
            "3 rows · 2 cols"
        );
        // Leftover placeholders stay; extra args are ignored.
        assert_eq!(format_template("{} {}", &["a"]), "a {}");
        assert_eq!(format_template("{}", &["a", "b"]), "a");
        assert_eq!(
            format_template("no placeholders", &["a"]),
            "no placeholders"
        );
    }
}
