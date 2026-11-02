# What limits agent-run SQL

An agent writes SQL, and sometimes copies it from data it has read. DuckDB's SQL can read any file you can, write files, reach the network and load extensions. This page says what stops that, and what doesn't.

Enforcement covers `ducklocal query` and `ducklocal check`, and only with `--restricted`.

## Read-only is not a sandbox

`ducklocal query` opens a `--database` file read-only by default. That protects that file and nothing else: `COPY` can still write anywhere you can, `read_text` can read your SSH keys, and `FROM 'https://...'` can send a request. Use `--restricted` for SQL you did not write yourself.

## `ducklocal query --restricted`

```bash
ducklocal query --restricted --allow-read ./data --sql "SELECT * FROM 'data/sales.csv'"
```

| The SQL tries to | Result |
|---|---|
| Read a file under `--allow-read` | Allowed |
| Read any other file, by path, glob or `..` | Denied |
| Write a file with `COPY` or `EXPORT`, even under `--allow-read` | Denied |
| `ATTACH` a database, or create one | Denied |
| Write to the `--database` file (it is opened read-only, and `--read-write` is refused) | Denied |
| Reach `http`, `https`, `s3` or any other URL | Denied |
| `INSTALL` or `LOAD` an extension, or autoload one | Denied |
| Read stored DuckDB secrets | Denied |
| Change a setting, including these limits | Denied |
| Spill to a temporary file | Fails with `resource_limit` instead |
| Run longer than `--timeout` (default 60s) | Interrupted, kind `timeout` |
| Use more than `--memory-limit` (default 2GB) | Fails, kind `resource_limit` |

With no `--allow-read`, no files are readable. The `--database` file is opened before the limits apply, so it needs no allowlist entry. JSON output reports the limits it ran under, in `restricted`.

A denied statement exits 1 with kind `policy_denied` and never falls back to running unrestricted. If the limits themselves can't be applied, the kind is `policy_setup` and nothing runs.

### How it is enforced

There are two layers, and both are DuckDB's own. There is no search of the SQL text.

1. **Settings.** `enable_external_access = false` with `allowed_directories` and `allowed_paths` blocks files, URLs and extensions at DuckDB's file system. Then `lock_configuration = true` stops the SQL from undoing it.
2. **The parser.** DuckDB's allowlist also permits writes: `COPY ... TO` and `ATTACH` both succeed inside an allowed directory. So DuckDB's parser must first see exactly one SELECT (through `json_serialize_sql`). This also rules out `EXPLAIN ANALYZE`, which would run the statement it explains.

`src/duckdb_contract.rs` pins this DuckDB behaviour, so a DuckDB upgrade that changes it fails a test. `tests/cli.rs` attempts each row above.

A side effect of layer 2: `EXPLAIN`, `PRAGMA`, `CALL` and top-level `PIVOT` are refused. A PIVOT that names its values works as a subquery: `SELECT * FROM (PIVOT t ON a IN ('x', 'y') USING sum(b))`.

## `ducklocal check --restricted`

```bash
ducklocal check report.dash --restricted --allow-read ./data
```

Checking a dashboard runs its SQL, so a dashboard you didn't write gets the same limits as `query --restricted`. Every row of the table above holds, and three things are specific to `check`:

- **Nothing is allowed by being nearby.** The dashboard's own folder is unreadable unless you name it, because a dashboard can sit in a folder of things it shouldn't read.
- **Paths start from the spec's folder, and the file they find is the one judged.** `FROM 'data/x.csv'` that resolves to `reports/data/x.csv` needs that folder allowed, even if a `data` folder is allowed where you ran the command. A path in SQL that starts with `..` is judged from where you ran the command, so it is usually refused; use a `source` block, whose `path` can start that way, or an absolute path.
- **The timeout covers the whole check**, not each query. One run lists every query refused, so you can ask the person once.

The window doesn't apply this. Opening a dashboard, and clicking Review on a proposed change, run its SQL as `check` does without the flag.

## What this does not protect against

- **The agent's own command line.** `--restricted` binds the SQL, not whoever chooses the flags. An agent with a shell can drop the flag, add `--allow-read /`, or skip DuckLocal entirely. This guards against mistakes and against SQL planted in data, not a hostile agent. Containing that takes OS-level isolation, around the agent rather than around DuckLocal.
- **Reading what you allowed.** Anything under `--allow-read` is readable, and query results go to the agent. Allow the narrowest path that works.
- **Hard links.** DuckDB resolves symbolic links and Windows junctions before it compares, so one inside an allowed directory that points out of it is refused, and a recursive glob that meets one fails rather than skips it. A hard link is the same file under another name, so nothing can tell, and it is readable. Don't allow a directory whose contents someone else controls. That also covers something not tested: another process swapping a link while a query runs.
- **Other commands.** Only `query` and `check` take `--restricted`. `schema` and `profile` read the targets they are given. `project check` runs each dashboard's SQL unrestricted, apart from not running one that reads remote data. `open --run` runs SQL in the window's session, with the window's data and credentials.
- **Resource use in general.** The memory limit applies to DuckDB's buffer manager, not every allocation, and DuckLocal holds up to 2,000,000 result cells in memory on top. The timeout covers running the statement, not writing its output.

## Platforms

Enforcement is inside DuckDB, so it works the same way on every platform, and CI runs the `--restricted` tests on macOS, Linux and Windows. DuckLocal applies **no operating-system isolation on any platform**: no sandbox profile, no Landlock or seccomp, no AppContainer. A flaw in DuckDB's checks has nothing behind it.

| | macOS | Linux | Windows |
|---|---|---|---|
| `--restricted` (DuckDB's checks) | Yes | Yes | Yes |
| Symbolic links out of an allowed directory refused | Yes | Yes | Yes |
| Junctions out of an allowed directory refused | n/a | n/a | Yes |
| Another spelling of an allowed path still reads it (case, `\\?\`) | Not tested | Not tested | Yes |
| OS sandbox applied by DuckLocal | No | No | No |

Windows short names (`ALLOWE~1`) read the file they name, the same way; that was checked by hand once and isn't in CI, because a volume can have short names turned off. Don't rely on another spelling of a path being refused, or accepted, on macOS or Linux.

### What an OS sandbox would add

Each platform has a way to limit a process from outside DuckDB, so that a flaw in DuckDB's checks would have something behind it. None is used. This comes from each platform's documentation; none has been tried with DuckLocal.

| Platform | Mechanism | What it could do | Why it isn't used |
|---|---|---|---|
| Linux | Landlock, no root needed | Limit file access (kernel 5.13+) and TCP connections (6.7+) from inside the process | Older kernels get less or nothing, so it would need to report what it applied. The closest fit if this work is taken up |
| Linux | seccomp, or bubblewrap around the process | Filter system calls, or give the process its own view of the file system | seccomp sees no path text; bubblewrap has to be installed |
| macOS | `sandbox-exec` | Limit files and network by profile | Marked deprecated by Apple, with an undocumented profile language |
| macOS | App Sandbox | Limit a signed app by entitlement | Fixed when the app is signed, so it can't follow `--allow-read` |
| Windows | AppContainer | Limit files and network | A path has to grant the container access through its ACL, which means changing permissions on the person's files |
| Windows | Job objects | Limit memory and process count | Doesn't limit files or network |

To contain an agent you don't trust today, run the agent itself inside a container, a VM, or a sandbox its own tool provides. That also covers the rest of what its shell can do, which no limit inside DuckLocal can reach.

## When an agent should ask first

The agent skill tells agents to ask the person before they:

- add an `--allow-read` path the person didn't name;
- drop `--restricted`, for example to export a file, read a remote source or use an extension, or to check a dashboard they didn't write;
- raise `--timeout` or `--memory-limit` well past the defaults.

Running without `--restricted` stays available for all of those. It is the CLI's ordinary mode, and the choice should be the person's.
