# What limits agent-run SQL

An agent writes SQL, and sometimes copies it from data it has read. DuckDB's SQL can read any file you can, write files, reach the network and load extensions. This page says what stops that, and what doesn't.

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

## What this does not protect against

- **The agent's own command line.** `--restricted` binds the SQL, not whoever chooses the flags. An agent with a shell can drop the flag, add `--allow-read /`, or skip DuckLocal entirely. This guards against mistakes and against SQL planted in data, not a hostile agent. Containing that takes OS-level isolation, around the agent rather than around DuckLocal.
- **Reading what you allowed.** Anything under `--allow-read` is readable, and query results go to the agent. Allow the narrowest path that works.
- **Links inside an allowed directory.** DuckDB's check appears to compare path text, so assume a symbolic link in an allowed directory that points outside it can be followed; this isn't tested. Don't allow a directory whose contents someone else controls.
- **Other commands.** Only `query` takes `--restricted`. `schema` and `profile` read the targets they are given. `check` runs a `.dash` file's SQL with no such limits, so review a dashboard from an untrusted source before checking it. `open --run` runs SQL in the window's session, with the window's data and credentials.
- **Resource use in general.** The memory limit applies to DuckDB's buffer manager, not every allocation, and DuckLocal holds up to 2,000,000 result cells in memory on top. The timeout covers running the statement, not writing its output.

## Platforms

Enforcement is inside DuckDB, so it works the same way on every platform, and CI runs the same tests on macOS, Linux and Windows. DuckLocal applies **no operating-system isolation on any platform**: no sandbox profile, no Landlock or seccomp, no AppContainer. A flaw in DuckDB's checks has nothing behind it. Differences in how platforms spell a path (case, Windows short names) aren't tested, so name paths in SQL the way you named them in `--allow-read`.

| | macOS | Linux | Windows |
|---|---|---|---|
| `--restricted` (DuckDB's checks) | Yes | Yes | Yes |
| OS sandbox applied by DuckLocal | No | No | No |

## When an agent should ask first

The agent skill tells agents to ask the person before they:

- add an `--allow-read` path the person didn't name;
- drop `--restricted`, for example to export a file, read a remote source or use an extension;
- raise `--timeout` or `--memory-limit` well past the defaults.

Running without `--restricted` stays available for all of those. It is the CLI's ordinary mode, and the choice should be the person's.
