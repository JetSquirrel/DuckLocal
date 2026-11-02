# Network and privacy

What DuckLocal sends over the network, when, and how to check that for yourself rather than taking our word for it.

## The short version

Opening local files, querying them and viewing dashboards over them make no outbound connection. DuckLocal has no telemetry, no analytics, no crash upload, no account and no AI service inside it. The check described below tests that on Linux, on every change.

Everything that does reach the network is either something you turn on, something you click, or SQL that names a remote address. The table lists all of it.

## Every outbound path

| Path | What starts it | On by default? | Goes to | What it sends | How to stop it |
|---|---|---|---|---|---|
| **Update check** | Clicking the version in the status bar, the menu's Check for Updates, or the launch check if you turned that on | No | `dl.ducklocal.app` | A request for `latest/latest.json`, then the package if newer; your IP address, and the app version in the User-Agent | Don't click it; leave Check for Updates at Launch off |
| **Map base tiles** | View → Online base map, then showing a map chart | No | `tile.openstreetmap.org`, or the server in the `map_tile_url` setting | Which tiles are wanted, which reveals roughly the area and zoom you're viewing; your IP address, and the app version in the User-Agent. Never your rows | Leave it off; maps then draw over a grid |
| **S3 browsing** | Configuring S3 (title bar), then expanding it in the sidebar | No | The endpoint you typed | Requests signed with your key. The secret signs the request and is not itself sent | Don't configure S3 |
| **`httpfs` download** | Enabling S3 runs `INSTALL httpfs` | No | DuckDB's extension server (`extensions.duckdb.org`) | A request for the extension; your IP address | Don't configure S3 |
| **Extensions** | Install or Update in the Extensions tab, or `INSTALL` in your SQL | No | DuckDB's extension server, or the source your SQL names | A request for the extension; your IP address | Don't install them. DuckDB's automatic install is turned off in every DuckLocal connection |
| **Remote data in SQL** | SQL naming `https://`, `s3://`, a Quack server or another remote database | No, and needs an extension already installed | The address in the SQL | What the SQL asks for, including anything written into the URL | Don't run it, or use `query --restricted` from the command line |
| **A link you click** | The download link the status bar shows when an update can't install itself | No | Your browser opens it | Whatever your browser sends | Don't click it |

Local only, and listed so you can find them: the window listens on `127.0.0.1` on a random port for `ducklocal open`, protected by a token in a file only your user can read. `ducklocal lsp` speaks over stdin and stdout.

### What is stored on your machine

Query history, settings and saved tabs live in the app's data folder (the menu's Open logs folder opens the folder beside them), and map tiles in its cache folder. Query history keeps your SQL text, including anything you typed into it. Crash reports stay in that folder and are shown to you on the next launch; nothing uploads them. S3 credentials last for the session and are not written to disk by DuckLocal.

## Where the boundaries are

- **Opening a `.dash` file runs its SQL.** That SQL can only read, but "read" includes `read_csv('https://...')` when `httpfs` is already installed, and the address can carry data from other tables in the same query. So a dashboard someone sends you can make a network request, or read any local file you can, when you open it. Don't open dashboards from people you don't trust if you've installed `httpfs`. Without it, DuckDB can't reach remote addresses and won't install it on its own.
- **Extensions are third-party code.** They run inside DuckLocal with your permissions, and we can't vouch for them, community extensions least of all.
- **Credentials you configure** can be used by any SQL in that session, including a dashboard's.
- **SQL you run** does what it says. `COPY` can write files and remote queries can send data. Read-only mode protects only the database file. `query --restricted` limits SQL from the command line, but it isn't an OS sandbox: see [agent-security.md](agent-security.md).
- **An external agent you use with DuckLocal** is separate software. DuckLocal doesn't send anything to it, but whatever the agent reads through the CLI goes wherever that agent sends it, and that's between you and its provider.
- **What the servers log.** The servers above see your IP address and what you asked for. `dl.ducklocal.app` is a Cloudflare R2 bucket; `tile.openstreetmap.org` follows the OpenStreetMap Foundation's policies.
- **DuckDB and the libraries** are large dependencies. We test the paths below, but haven't audited every line of them.

## Check it yourself

**Watch it run.** On Linux, `scripts/check-offline.sh` runs the CLI and the window over local files under `strace`, and fails on any connection attempt beyond the local machine. CI runs it on every change. To run it yourself:

```bash
cargo build --locked
scripts/check-offline.sh target/debug/ducklocal
```

The script only proves those paths, on Linux, on that run. On macOS or Windows, a firewall that logs per-app connections (Little Snitch, Windows Firewall logging) gives you the same evidence.

**Read the code.** Every use of a network API is listed in `tests/network_paths.rs`, and that test fails if a new one appears without being added there and to this page. The only HTTP client the native app depends on is `ureq`. `Cargo.lock` also lists `hyper`, `quinn` and `gpui-pre-reqwest`, which only `gpui-kit-assets`' WebAssembly build uses. To confirm that for your platform:

```bash
cargo tree --locked -e normal | grep -E " (ureq|reqwest|hyper) "
```

**Build it from source.** Everything is in this repository:

```bash
git clone https://github.com/JetSquirrel/DuckLocal && cd DuckLocal
git checkout v0.3.3   # the release you want
cargo build --release --locked
```

### Verifying a download

Releases are built by the public workflow in `.github/workflows/release.yml`, and you can see each run on GitHub.

| Package | Signed? | How to check |
|---|---|---|
| macOS `.dmg` | Signed and notarized by Apple | `spctl -a -vv -t install DuckLocal.dmg`, and `codesign -dv` on the app |
| Windows `.zip` | Not signed | Compare its SHA-256 (below) |
| Linux `.tar.gz` | Not signed | Compare its SHA-256 (below) |

`https://dl.ducklocal.app/latest/latest.json` lists each package's SHA-256. Compare that with `sha256sum FILE`, or `Get-FileHash FILE` on Windows.

That hash comes from the same server as the package, so it catches a corrupted download but not a compromised server. The built-in updater has the same limit on Windows and Linux. On macOS it also checks that the new app was signed by the same team as the running one. If that matters to you, build from source.

## Adding a new outbound path

A change that adds one must:

1. Add it to the table above: what starts it, where it goes, what it sends, and how to stop it.
2. Add it to `tests/network_paths.rs`, which will otherwise fail.
3. Keep it off until someone asks for it, and say what it does when they do.
4. Keep `scripts/check-offline.sh` passing, or explain in the pull request why the ordinary local path must change.
