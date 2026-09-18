# 开发

**[English](../development.md)** · [文档](index.md)

## 构建与运行

```bash
cargo run                     # 启动空工作区
cargo run -- ./data/logs/     # 或直接打开数据
cargo test                    # 运行测试套件
```

DuckDB 从随仓库携带的源码构建，因此首次构建耗时很长，还需要几 GB 磁盘空间。dev profile 的依赖以 `opt-level = 3` 编译，这让首次构建更慢，但开发迭代仍然顺畅——之后只有 `ducklocal` 自身会重新编译。

**工具链。** 仓库里既没有 `rust-toolchain.toml`，也没有 `rust-version` 字段，因此实际的版本下限由依赖决定：`duckdb` 要求 `1.85.1`。CI 直接使用 stable。

## 被钉住的工具链

UI 工具包、分析面板所依赖的脚本运行时、面板绘制所用的组件目录，是同一个仓库
`longbridge/gpui-kit` 里的三个 crate，`rquickjs` 也被 patch 到同一个仓库。四者一起钉在同一个
revision 是有意为之：两个 revision 会让构建里出现两份 `gpui-base` 或两份 `rquickjs`，报出来的是
一大片 trait 不匹配，而其中没有一条会说出原因。

```bash
./scripts/check-deps.sh
```

它检查 Cargo 不会替你检查的三件事：依赖与 `[patch]` 用的是同一个 revision、`Cargo.lock` 与之一致、
没有任何 crate 在构建里出现两次（一次来自该仓库，一次来自 crates.io）。它只读这两个清单文件，
所以离线、即时。CI 会在跑测试之前执行它。要升级工具包，请把 `Cargo.toml` 里所有 `rev` 一起改掉，
执行 `cargo update`，再跑一次这个脚本。

按 revision 钉依赖，也是这个 crate 无法发布到 crates.io 的原因：git 依赖和 `[patch]` 在那里都不被接受。
分发走的是下面的 macOS 签名包。

## 打包 macOS 应用

```bash
./scripts/bundle.sh
# 生成 target/release/DuckLocal.app
```

它会组装好应用包——可执行文件位于 `Contents/MacOS/ducklocal`，再加上 `assets/Info.plist` 和 `assets/AppIcon.icns`——但**不做签名**。产物在你自己的机器上可以运行，到了其他机器上会被 Gatekeeper 拒绝。

## 打包、签名与公证

```bash
scripts/package-macos.sh <binary> <output.dmg> <version>
```

请在仓库根目录运行；它会把 `DuckLocal.app` 和 `dmg-root` 写到当前工作目录，dmg 就放在它们旁边。脚本在没有凭据时会拒绝运行——未公证的产物在当前 macOS 上用户根本打不开，所以它宁可快速失败，也不愿意意外产出一个。

需要以下内容：

| 变量 | 含义 |
| --- | --- |
| `NOTARY_KEY` | App Store Connect API 密钥（`.p8`）的路径 |
| `NOTARY_KEY_ID` | 该密钥的 Key ID |
| `NOTARY_ISSUER` | 该密钥所属的 issuer UUID |
| Developer ID Application 签名身份 | 当钥匙串中恰好只有一个时自动检测，否则设置 `MACOS_SIGN_IDENTITY` |

脚本会按参数中的版本号给应用包打上版本，用 `--options runtime --timestamp` 和 `scripts/entitlements.plist` 对内部可执行文件和应用本身签名，构建一个带 `/Applications` 符号链接的 UDZO dmg，**对 dmg 本身签名**，用 `notarytool submit --wait --timeout 300m` 提交，把公证票据装订到文件上，最后执行 `stapler validate` 和两次 `spctl --assess` 校验。

使用前需要了解两件事：

- 五小时的超时是客户端等待时间，而不是取消。超时后提交仍会在 Apple 那边继续处理，票据稍后依旧可以领取——但**票据只对提交时的那串字节有效**，所以不要重新构建；请对你提交的那个文件装订票据。
- 只有 dmg 经过公证，因此内部的 `.app` 上没有装订票据。从刚挂载的镜像中首次启动时需要联网，以便向 Apple 校验。

必须启用 `com.apple.security.cs.disable-library-validation` 授权，因为 DuckDB 会用 `dlopen` 加载它的扩展——`httpfs` 就是其中之一。

## CI 与发布

| 工作流 | 触发条件 | 作用 |
| --- | --- | --- |
| `ci.yml` | 推送到 `main`、任意 pull request | 在 `macos-latest` 上先执行 `scripts/check-deps.sh`，再执行 `cargo test --locked` |
| `release.yml` | 匹配 `v*` 的 tag，或手动指定版本触发 | 构建 `aarch64-apple-darwin`，把签名证书导入临时钥匙串，运行 `package-macos.sh`，上传 `ducklocal-macos-arm64.dmg`，然后用自动生成的说明创建 GitHub 发布 |
| `gh-pages.yml` | `main` 分支上 `docs/` 或工作流有改动时，或手动触发 | 使用 Node.js 24 构建文档站并部署到 GitHub Pages |

发布构建需要以下仓库密钥：`MACOS_CERTIFICATE`、`MACOS_CERTIFICATE_PWD`、`NOTARY_KEY_BASE64`、`NOTARY_KEY_ID` 和 `NOTARY_ISSUER`。

## 平台支持

Apple 芯片上的 macOS 是唯一受支持的目标平台，而且这是有意为之：打包路径是 `.app` 加 dmg，没有 Windows 或 Linux 产物，发布工作流也是这么写的。`src/` 里没有任何 `cfg(target_os)`，窗口层还带着 X11 和 Wayland feature 一起构建，所以构建出 Linux 版本是可能的——但 CI 里没有任何环节跑它，也没有做过测试。

## 文档站

`docs/` 下的指南同时以站点形式发布，用 [VitePress](https://vitepress.dev/) 构建。
需要 Node.js 22 或更新版本；**推荐 Node.js 24 LTS**，文档工作流也使用该版本。
在仓库根目录执行：

```bash
npm --prefix docs ci             # 按锁文件安装文档依赖
npm --prefix docs run dev        # http://localhost:5173/DuckLocal/
npm --prefix docs run build      # 产物在 target/docs-site
npm --prefix docs run preview    # http://localhost:4173/DuckLocal/
```

英文页面直接位于 `docs/`，中文页面位于 `docs/zh/`。这些就是 README 链接、GitHub 渲染的
同一批 Markdown，不生成源页面副本。`docs/.vitepress/config.mts` 定义两种语言、各自的
导航与侧栏、本地搜索，以及 GitHub Pages 的 `/DuckLocal/` 基础路径；还会为旧中文
`.zh-CN.html` 地址生成保留查询参数和锚点的跳转页。`docs/.vitepress/theme/` 在原生默认
主题上增加少量品牌样式，`docs/assets/` 存放图片，`docs/public/` 存放 favicon。

文档包固定使用稳定版 VitePress 1.6.4。其间接开发工具依赖目前存在 npm audit 安全告警；
开发与预览服务器应仅在本机使用，不要暴露给不可信网络。发布的站点是静态文件。

`.github/workflows/gh-pages.yml` 会在 `main` 分支的 `docs/` 或该工作流本身有改动时
构建并部署，也支持手动触发。**需要先在 Settings → Pages 里把 Source 选为
“GitHub Actions”**，之后才会自动部署。

## 项目结构

| 路径 | 内容 |
| --- | --- |
| `src/main.rs` | 入口点、提前分流 CLI、GUI 窗口设置 |
| `src/cli.rs` | 无界面参数、DuckDB 解析验证、独立连接、JSON 错误 |
| `tests/cli.rs` | 真实二进制集成测试，临时数据位于 `target/cli-tests` |
| `skills/ducklocal/` | 官方 schema 优先 SQL agent skill 与 CLI 参考 |
| `src/sources.rs` | 把路径解析成要挂载的文件和要打开的数据库 |
| `src/db.rs` | DuckDB 连接、挂载文件、服务器信息 |
| `src/query.rs` | 查询执行、结果序列化、导出 |
| `src/schema.rs` | 为侧栏做 catalog 内省 |
| `src/analysis/` | JavaScript 分析面板：脚本运行时、`ducklocal` host 模块、面板标签页、重载监控 |
| `src/history.rs` | 应用自己的数据库：查询历史、已登记的文件、设置 |
| `src/s3.rs` | SigV4 签名与 S3 列举 |
| `src/i18n.rs` | 字符串表和语言选择 |
| `src/state.rs` | 各视图共享的应用状态 |
| `src/ui/` | 编辑器、结果表格、图表、侧栏、对话框、标题栏和状态栏 |
| `docs/` | 英文指南、`zh/` 下的中文指南，以及 VitePress 依赖、配置与主题 |
| `assets/` | 图标、`Info.plist`、README 与文档使用的示意图 |
| `scripts/` | `bundle.sh`、`package-macos.sh`、`check-deps.sh`、授权文件 |

## 测试说明

测试套件就是 `cargo test`。查询执行测试通过内存模式的连接来跑 DuckDB；导出测试同时覆盖 CSV 与 Parquet，后者会把写出的文件用 `read_parquet` 读回来验证。
