# Development

**[中文](zh/development.md)** · [Docs](index.md)

## Build and run

```bash
cargo run                     # empty in-memory workspace
cargo run -- ./data/logs/     # open something straight away
cargo test                    # run the test suite
```

DuckDB is built from the vendored sources, so the first build is long and
needs a few gigabytes of disk. Dev-profile dependencies are compiled at
`opt-level = 3`, which makes that first build slower still but keeps the
development loop usable — only `ducklocal` itself recompiles afterwards.

**Toolchain.** There is no `rust-toolchain.toml` and no `rust-version` key, so
the effective floor is set by dependencies: `1.85.1`, required by `duckdb`.
CI uses plain stable.

## The pinned toolkit

The UI toolkit, the script runtime that analysis apps run on, and the
component catalog they draw from are three crates out of one repository,
`longbridge/gpui-kit`, and `rquickjs` is patched to the same one. All four are
pinned to a single revision on purpose: two revisions put two copies of
`gpui-base`, or of `rquickjs`, in the build, and that fails as a wall of trait
mismatches naming neither cause.

```bash
./scripts/check-deps.sh
```

It checks the three things Cargo will not: one revision across the
dependencies and the `[patch]`, a `Cargo.lock` that agrees with it, and no
crate in the build twice — once from the repository and once from crates.io.
It reads the two manifests and nothing else, so it is offline and immediate.
CI runs it before the tests. To move to a newer toolkit, change every `rev` in
`Cargo.toml` together, run `cargo update`, and run the script.

Pinning by revision is also why this crate is not publishable to crates.io: a
git dependency and a `[patch]` are both refused there. Distribution is the
signed macOS bundle below.

## Bundle a macOS app

```bash
./scripts/bundle.sh
# produces target/release/DuckLocal.app
```

That assembles the bundle — binary at `Contents/MacOS/ducklocal`, plus
`assets/Info.plist` and `assets/AppIcon.icns` — but **does not sign it**. The
result runs on your machine and will be refused by Gatekeeper anywhere else.

## Package, sign, and notarize

```bash
scripts/package-macos.sh <binary> <output.dmg> <version>
```

Run it from the repository root; it writes `DuckLocal.app` and `dmg-root` into
the working directory and the dmg next to them. The script refuses to run
without credentials — an unnotarized artifact is not something a user can open
on current macOS, so it fails fast rather than producing one by accident.

Required:

| Variable | Meaning |
| --- | --- |
| `NOTARY_KEY` | Path to an App Store Connect API key (`.p8`) |
| `NOTARY_KEY_ID` | That key's Key ID |
| `NOTARY_ISSUER` | The issuer UUID the key belongs to |
| a Developer ID Application identity | Auto-detected when exactly one is in the keychain, otherwise set `MACOS_SIGN_IDENTITY` |

The script builds the bundle with the version stamped from the argument, signs
the inner executable and the app with `--options runtime --timestamp` and
`scripts/entitlements.plist`, builds a UDZO dmg with an `/Applications`
symlink, **signs the dmg itself**, submits it with `notarytool submit --wait
--timeout 300m`, staples the ticket, and finishes with `stapler validate` plus
two `spctl --assess` checks.

Two things to know before using it:

- The five-hour timeout is a client-side wait, not a cancellation. If it
  expires, the submission keeps processing on Apple's side, and the ticket can
  be collected later — but **a ticket binds to the exact bytes submitted**, so
  do not rebuild; staple the file you submitted.
- Only the dmg is notarized, so the inner `.app` carries no stapled ticket. A
  first launch from a just-mounted image needs network to check in with Apple.

The `com.apple.security.cs.disable-library-validation` entitlement is required,
because DuckDB `dlopen`s its extensions — `httpfs` among them.

## CI and releases

| Workflow | Trigger | What it does |
| --- | --- | --- |
| `ci.yml` | Push to `main`, any pull request | `scripts/check-deps.sh`, then `cargo test --locked`, on `macos-latest` |
| `release.yml` | Tag matching `v*`, or manual dispatch with a version | Builds `aarch64-apple-darwin`, imports the signing certificate into a throwaway keychain, runs `package-macos.sh`, uploads `ducklocal-macos-arm64.dmg`, then creates the GitHub release with generated notes |
| `gh-pages.yml` | Push to `main` touching `docs/` or the workflow, or manual dispatch | Builds the documentation site with Node.js 24 and deploys it to GitHub Pages |

The release build needs these repository secrets: `MACOS_CERTIFICATE`,
`MACOS_CERTIFICATE_PWD`, `NOTARY_KEY_BASE64`, `NOTARY_KEY_ID`, and
`NOTARY_ISSUER`.

## Platform support

macOS on Apple silicon is the only supported target, and deliberately so: the
packaging path is `.app` plus dmg, there is no Windows or Linux artifact, and
the release workflow says as much. There is no `cfg(target_os)` anywhere in
`src/`, and the windowing layer builds with X11 and Wayland features, so a
Linux build is plausible — but nothing in CI exercises it, and it is not
tested.

## The documentation site

The guides in `docs/` are also published as a site, built with
[VitePress](https://vitepress.dev/). Use Node.js 22 or newer; **Node.js 24 LTS
is recommended** and used by the documentation workflow. Run these commands
from the repository root:

```bash
npm --prefix docs ci             # install the locked documentation dependencies
npm --prefix docs run dev        # http://localhost:5173/DuckLocal/
npm --prefix docs run build      # output lands in target/docs-site
npm --prefix docs run preview    # http://localhost:4173/DuckLocal/
```

English pages live directly in `docs/`, and Chinese pages in `docs/zh/`.
These are the same Markdown files linked from the README and rendered on GitHub;
there are no generated source-page copies. `docs/.vitepress/config.mts` defines
both locales, their navigation and sidebars, local search, and the
`/DuckLocal/` GitHub Pages base path. It also generates redirects for the old
Chinese `.zh-CN.html` URLs, preserving query strings and anchors.
`docs/.vitepress/theme/` extends the native default theme with a small brand
stylesheet, `docs/assets/` holds the images, and `docs/public/` holds the favicon.

The documentation package pins stable VitePress 1.6.4. Its transitive development
tooling currently has npm audit advisories; keep development and preview servers
local, and do not expose them to untrusted networks. The published site is static.

`.github/workflows/gh-pages.yml` builds and deploys the site on pushes to `main`
that touch `docs/` or the workflow itself, and can also be run manually.
**GitHub Pages has to be switched on once**, in Settings → Pages, with Source
set to “GitHub Actions”.

## Project layout

| Path | Contents |
| --- | --- |
| `src/main.rs` | Entry point, early CLI dispatch, GUI window setup |
| `src/cli.rs` | Headless arguments, DuckDB parser validation, isolated connections, JSON errors |
| `tests/cli.rs` | Real executable integration tests; scratch data under `target/cli-tests` |
| `skills/ducklocal/` | Official schema-first SQL agent skill and CLI reference |
| `src/sources.rs` | Resolving paths into files to attach and databases to open |
| `src/db.rs` | The DuckDB connection, attaching files, server info |
| `src/query.rs` | Query execution, result serialization, export |
| `src/profile.rs` | Column profiling behind `ducklocal profile` |
| `src/schema.rs` | Catalog introspection for the sidebar |
| `src/analysis/` | JavaScript analysis apps: the script runtime, the `ducklocal` host module, the app tab, the reload watcher |
| `src/app_export/` | `ducklocal export`: running an app headless and writing its statements and results as one standalone HTML file |
| `src/history.rs` | The app's own database: history, registered files, settings |
| `src/s3.rs` | SigV4 signing and S3 listing |
| `src/i18n.rs` | The string table and language selection |
| `src/state.rs` | Application state shared across views |
| `src/ui/` | Editor, results grid, charts, sidebar, dialogs, title and status bars |
| `src/perf_probe.rs` | Test-only performance probe harness: wall time, allocation count, and bytes for paths that scale with data size |
| `docs/` | English guides, Chinese guides in `zh/`, and the VitePress package, config, and theme |
| `assets/` | Icon, `Info.plist`, the diagram the README and docs use |
| `scripts/` | `bundle.sh`, `package-macos.sh`, `check-deps.sh`, entitlements |

## Testing notes

The test suite is `cargo test`. Query execution tests exercise DuckDB through
an in-memory connection; the export tests cover both CSV and Parquet, the
latter by reading the written file back with `read_parquet`.
