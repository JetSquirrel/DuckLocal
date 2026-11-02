# A leaked key, investigated by an agent and a person

A runnable walk through the way DuckLocal is meant to work: an external agent investigates a log with the CLI, the person sees and questions the work in the window, and the result can be rechecked later somewhere else.

`tests/cloudtrail_example.rs` runs the agent's half of this on every change and asserts the numbers below. The person's half has not been run: see [What was tested](#what-was-tested).

## The data

`data/cloudtrail.json.gz` is one made-up week of CloudTrail: 795 events in the shape AWS writes them, from 2 to 8 March 2026. `generate.py` writes the same bytes every time. Nothing came from a real account: the account ID and access keys are the examples from AWS's documentation, and the addresses are from the ranges reserved for documentation (RFC 5737).

## What there is to find

The build key `ci-deploy` normally runs from `198.51.100.20`, in working hours. All times are UTC.

| When, 5 March | From | What happened | Evidence |
|---|---|---|---|
| 02:14 | `203.0.113.77` | The key appears at a new address, and most of what it tries is refused | 33 calls, 28 `AccessDenied` |
| 02:16 | `203.0.113.77` | It creates a user, makes it an administrator and gives it a key | `CreateUser`, `AttachUserPolicy` (`AdministratorAccess`), `CreateAccessKey` for `svc-backup` |
| 02:26 | `203.0.113.78` | The new user tries to stop the log, and is refused | `StopLogging`, `AccessDenied` |
| 02:26 to 02:36 | `203.0.113.78` | It reads from a bucket nobody else touches | 240 `GetObject` on `customer-exports` |

The log can show that those objects were read, but not what was in them or where they went.

## The prompt

Give an agent that has the DuckLocal skill this, from this folder:

> `data/cloudtrail.json.gz` is a week of CloudTrail from one account. Find out whether anything here needs a person's attention. Show me the SQL you rely on, build a dashboard I can review, and don't change it without asking once I've seen it.

## The agent's commands

**1. Record the work, and read the shape.**

```bash
export DUCKLOCAL_SESSION=investigation.session.jsonl
ducklocal schema data/cloudtrail.json.gz --stats
```

The whole log is one row with one column, `Records`, so these statistics say little. The queries unnest it first.

**2. Investigate, under limits.** SQL read from a log is SQL someone else may have influenced, so these run restricted.

```bash
ducklocal query --restricted --allow-read data --sql-file sql/01_who.sql --format md
ducklocal query --restricted --allow-read data --sql-file sql/02_outside_hours.sql --format md
ducklocal query --restricted --allow-read data --sql-file sql/03_changes.sql --format md
ducklocal query --restricted --allow-read data --sql-file sql/04_new_user.sql --format md
ducklocal query --restricted --allow-read data --sql-file sql/05_timeline.sql --limit 5
```

The last one reports `"truncated": true` and `"total_rows": 276`. Those five rows aren't the whole timeline, and the session keeps that doubt.

**3. Show the person.**

```bash
ducklocal open --title "Who did what" --run --wait --sql-file sql/01_who.sql
```

**4. Hand over a dashboard.**

```bash
ducklocal check reports/investigation.dash --summary
ducklocal open reports/investigation.dash
```

**5. Answer the person's question with a proposal,** once they have commented. Read the question, then propose the change rather than editing the file.

```bash
ducklocal comments reports/investigation.dash
ducklocal propose reports/investigation.dash --from proposals/answer-new-user.dash \
  --note "Adds what svc-backup did, to answer your question on Changes to access"
ducklocal propose reports/investigation.dash --status
ducklocal comments reports/investigation.dash --reply c1 \
  --text "Yes: it read 240 objects from customer-exports. The proposal adds it." --resolve
```

**6. Say what it rests on.**

```bash
ducklocal session note --text "The build key leaked: svc-backup read 240 objects" --refs 1,3,4
ducklocal session show --last 10
```

## The person's part, in the window

1. Open the SQL tab the agent sent, and check the result against the agent's claim.
2. On the dashboard, open a plot's SQL and data source. Compare the 28 refused calls with the first query.
3. Hover **Changes to access, in order**, choose **Comment**, and ask: *Did the new user do anything after this?*
4. When the proposal arrives, click **Review**. This runs the proposed SQL, and shows the diff and the checks.
5. Apply or reject it. Applying keeps the old version beside the report as `investigation.dash.bak`.

## Reopen it elsewhere

```bash
ducklocal project check
```

Copy this folder anywhere and run it again: `ok` and `portable` stay true. Delete the data, and it names the missing source and how to recover. Passing doesn't prove the data is unchanged, and the report's `limitations` says so.

## What was tested

| Step | Status |
|---|---|
| Finding, restricted queries, truncation, session doubts | Asserted by `tests/cloudtrail_example.rs` |
| `check`, comments, `propose`, `--status`, reply | Asserted, with the question asked from the CLI as a stand-in for the person |
| `project check`, a moved folder, missing data | Asserted |
| `open --run --wait`, `open FILE` | **Not run in this walkthrough** |
| The window's parts: drawing the dashboard, commenting, Review, Apply | **Not run in this walkthrough.** Applying has its own unit tests in `src/spec/proposal.rs`, but nobody has clicked through this example |

The window was left out because on Windows it can't be started with its own state, so it would have changed the saved workspace of whoever ran it.

## How the agent's half went

One real run of the commands above, written by an agent as it worked.

**Manual handoffs: 4 expected, and 0 of them run yet.** The person reads the first result, reads the dashboard, asks the question, and decides on the proposal. Everything else went through the CLI.

| # | What happened | Cost | Outcome |
|---|---|---|---|
| 1 | `schema` on `.json.gz`, CloudTrail's own format, failed with "This file is not a DuckDB database" | Blocking and misleading | **Fixed here:** `.json.gz`, `.csv.gz` and similar now count as data files in `schema` and `source` blocks. The window's file attach shares that check, but wasn't run |
| 2 | `schema --stats` reports one row and one `Records` column, so it says nothing about 795 events | One extra query to see the shape | Open |
| 3 | `eventTime` reads as a time-zone-aware timestamp, shown in the machine's zone, and ICU isn't always installed to convert it | A day could differ between machines | Worked round with `make_timestamp(epoch_us(...))` |
| 4 | A `.dash` file has no shared view, so each of 5 queries repeats the same 13-line flattening | About 65 repeated lines | Open |
| 5 | Used `tone = "bad"`; the error listed the valid tones | One retry | Fine as it is |
| 6 | `project init` listed the proposal draft as a second dashboard | Edited the manifest by hand | Open |
| 7 | A column aliased `access_key` made the session drop the query's SQL, as it does for anything that may hold credentials, and it then blamed the SQL for its unknown sources | A misleading doubt on every step | **Doubt fixed here** to give the real reason. The alias is gone from this example. The word list still costs provenance on any log with a column by that name |

No query was denied with `--allow-read data`, so this run never had to ask for more access.

## What works today, and what needs more

| Journey step | On the current build |
|---|---|
| Read the log, investigate under limits | Works, with the fix above |
| Send SQL and a dashboard to the window | Commands exist (#66); not run in this example |
| Comment, proposal, diff, approval | CLI half asserted (#65); window half not run in this example |
| Record provenance, including truncation | Works (#69) |
| Reopen from a portable project | Works (#70) |
| A dashboard checked under limits before anyone opens it | Needs `check --restricted` (#80) |
| Drill from a card into the events behind it | Not available; would need new work |
