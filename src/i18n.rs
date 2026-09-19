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
    ("sidebar.refresh_schema", "刷新 Schema", "Refresh schema"),
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
    ("sidebar.s3.configured", "已配置", "Configured"),
    ("sidebar.s3.loading", "加载中…", "Loading…"),
    ("sidebar.s3.empty", "（空）", "(Empty)"),
    ("sidebar.s3.load_failed", "加载失败：{}", "Failed to load: {}"),
    ("sidebar.s3.refresh_buckets", "刷新 bucket 列表", "Refresh bucket list"),
    ("sidebar.table.generate_select", "生成 SELECT 查询", "Generate SELECT query"),
    ("sidebar.column.edit_type", "修改数据类型", "Change data type"),
    ("sidebar.file.remove", "从本地文件移除", "Remove from local files"),
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
    ("results.summary", "{} 行 · {} 列", "{} rows · {} columns"),
    ("results.export_csv", "导出 CSV", "Export CSV…"),
    ("results.export_parquet", "导出 Parquet", "Export Parquet…"),
    ("results.running", "正在运行查询…", "Running query…"),
    ("results.truncated", "结果已截断到 {} 行。", "Results truncated to {} rows."),
    ("results.affected", "完成 · {} 行受影响 · 耗时 {}", "Done · {} rows affected · took {}"),
    ("results.failed.title", "查询失败", "Query failed"),
    ("results.explain.elapsed", "EXPLAIN · 耗时 {}", "EXPLAIN · took {}"),
    ("results.empty.title", "运行查询以查看结果", "Run a query to see results"),
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
    ("title_bar.open_data", "打开数据…", "Open data…"),
    ("title_bar.configure_s3", "配置 S3 数据源（httpfs）", "Configure S3 source (httpfs)"),
    ("title_bar.toggle_theme", "切换明暗主题", "Toggle light/dark theme"),
    ("title_bar.toggle_language", "切换语言", "Switch language"),
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
    (
        "dialog.open_source.description",
        "输入路径，或浏览选择数据文件、文件夹或 .duckdb 数据库。数据文件会作为视图加入当前连接，文件夹会递归展开。输入新的 .duckdb 路径可创建数据库及其父目录。",
        "Enter a path, or browse to a data file, a folder, or a .duckdb database. Data files join the current connection as views; folders are expanded recursively. Enter a new .duckdb path to create a database and its parent folders.",
    ),
    ("dialog.open_source.browse", "浏览…", "Browse…"),
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
        "-- 在编辑器中输入 SQL，按 {} 运行\nSELECT '你好，DuckDB' AS greeting;",
        "-- Type SQL in the editor and press {} to run\nSELECT 'Hello, DuckDB' AS greeting;",
    ),
    ("workspace.tab.default_title", "查询 {}", "Query {}"),
    ("workspace.new_query", "新建查询", "New query"),
    ("workspace.run", "运行", "Run"),
    ("workspace.run.tooltip", "运行查询", "Run query"),
    ("workspace.format", "格式化", "Format"),
    ("workspace.format.tooltip", "格式化当前 SQL", "Format current SQL"),
    ("workspace.explain.tooltip", "查看查询计划", "View query plan"),
    ("workspace.rename", "重命名", "Rename"),
    ("workspace.rename.tooltip", "重命名当前查询 Tab", "Rename current query tab"),
    ("workspace.server_info", "线程 {} · 内存上限 {}", "Threads {} · memory limit {}"),
    ("dialog.rename.title", "重命名查询", "Rename query"),
    ("dialog.rename.confirm", "重命名", "Rename"),
    ("workspace.empty.title", "把数据拖进来", "Drop your data in"),
    (
        "workspace.empty.description",
        "拖入 CSV、TSV、Parquet、JSON 文件，或整个文件夹。数据只留在这台机器上。",
        "Drag in CSV, TSV, Parquet, or JSON files — or a whole folder. Your data stays on this machine.",
    ),
    ("workspace.empty.open_files", "打开文件…", "Open files…"),
    ("workspace.empty.open_folder", "打开文件夹…", "Open folder…"),
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

    // ── src/ui/status_bar.rs ────────────────────────────────────────────
    ("status_bar.connected", "已连接", "Connected"),
    ("status_bar.disconnected", "未连接", "Disconnected"),
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
    ("error.unsupported_file_type", "不支持的文件类型: {}", "Unsupported file type: {}"),
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

    // ── src/analysis/ ───────────────────────────────────────────────────
    (
        "analysis.empty.title",
        "这个面板没有目录",
        "This panel has no folder",
    ),
    (
        "analysis.empty.hint",
        "面板是包含 main.js 的文件夹，里面的 JavaScript 会查询当前连接的数据。",
        "A panel is a folder with a main.js; its JavaScript queries the current connection.",
    ),
    (
        "analysis.picker.prompt",
        "选择分析面板目录（包含 main.js）",
        "Choose a panel folder (one with a main.js)",
    ),
    (
        "analysis.loading",
        "正在加载分析面板…",
        "Loading the analysis panel…",
    ),
    ("analysis.reload", "重新加载", "Reload"),
    (
        "analysis.reload.tooltip",
        "重新加载面板；保存 .js 文件也会自动重新加载",
        "Reload the panel; saving a .js file reloads it too",
    ),
    (
        "analysis.load_failed",
        "分析面板加载失败",
        "The analysis panel could not be loaded",
    ),
    (
        "analysis.load_failed.hint",
        "修复目录中的 main.js 后点击「重新加载」。",
        "Fix main.js in the folder and press Reload.",
    ),
    (
        "analysis.no_runtime",
        "脚本运行时不可用，无法加载面板。",
        "The script runtime is unavailable, so the panel cannot load.",
    ),
    (
        "analysis.not_updated",
        "面板未更新",
        "Panel not updated",
    ),
    (
        "analysis.rejected.not_a_folder",
        "{} 不是文件夹。",
        "{} is not a folder.",
    ),
    (
        "analysis.rejected.no_entry",
        "{} 里没有 main.js。",
        "{} has no main.js.",
    ),
    (
        "analysis.rejected.no_longer_there",
        "{} 已不存在。",
        "{} is no longer there.",
    ),
    (
        "analysis.restore.not_a_folder",
        "上次打开的面板 {}（{}）已不是文件夹，未重新打开。",
        "The panel {} ({}) is no longer a folder; it was not reopened.",
    ),
    (
        "analysis.restore.no_entry",
        "上次打开的面板 {}（{}）里没有 main.js，未重新打开。",
        "The panel {} ({}) has no main.js; it was not reopened.",
    ),
    (
        "analysis.definition.show",
        "视图定义",
        "View definition",
    ),
    (
        "analysis.definition.back",
        "返回面板",
        "Back to panel",
    ),
    (
        "analysis.definition.tooltip",
        "读取这个面板的 JavaScript 源码",
        "Read this panel's JavaScript source",
    ),
    (
        "analysis.definition.hint",
        "面板是留在该目录里的一个 JavaScript 视图——由 agent 或你自己编写；保存文件即会重新加载。",
        "A panel is a JavaScript view left in that folder — by an agent, or by you. Saving the file reloads it.",
    ),
    (
        "analysis.definition.unreadable",
        "无法读取 {}：{}",
        "Could not read {}: {}",
    ),
    (
        "workspace.open_panel",
        "打开分析面板…",
        "Open panel…",
    ),
    (
        "workspace.add_tab.tooltip",
        "新建查询，或打开一个分析面板",
        "New query, or open a panel",
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
