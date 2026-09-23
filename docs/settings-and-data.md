---
description: "Where DuckLocal stores its data on your Mac, what it remembers between launches, interface size, themes, language, and how to reset it."
---

# Settings and app data

**[中文](zh/settings-and-data.md)** · [Docs](index.md)

DuckLocal keeps one file on disk. Everything else lives in memory for the
duration of a session.

## Where things are stored

| Platform | Path |
| --- | --- |
| macOS | `~/Library/Application Support/DuckLocal/history.duckdb` |
| Linux (if built there) | `~/.local/share/ducklocal/history.duckdb` |

That single DuckDB file holds three tables: the query history, the registered
data files, and a small key/value settings table — the interface language, the
open app and dashboard tabs, the recent-documents list, and the app folders
you have trusted. It lives beside a `.wal`
while the app is running.

Your data is never copied into it. Registered files are referenced by path and
re-attached on launch.

## What survives a restart

| Remembered | Where |
| --- | --- |
| Registered data files — path, view name, kind | app data file |
| Query history, including failures | app data file |
| Interface language | app data file |
| Interface size | app data file |
| Whether the sidebar is hidden | app data file |
| Open app and dashboard tabs, with their titles | app data file |
| Recently opened apps and dashboards (the sidebar's **Apps** / **Dashboards**) | app data file |
| App folders you chose to trust | app data file |

| Not remembered | Notes |
| --- | --- |
| Open query tabs and their SQL | in memory only — past queries are in **History** |
| The results panel contents | in memory only |
| Which database file is open | each launch starts in memory unless you name one |
| Window size and position | fixed at 1440×900, minimum 960×600 |
| Sidebar and results panel sizes | fixed per launch |
| The results filter | in memory only |
| **The theme** | always starts light — see below |
| S3 credentials | session-only by design |

The theme is the odd one out: the language toggle persists, the theme toggle
does not, and every launch begins in light mode.

## Language

The `EN` / `中` button in the title bar switches the interface language
immediately and records the choice.

On first run — before any setting is stored — the language is taken from the
system locale: `LC_ALL` or `LANG` containing `zh` selects Chinese, anything
else selects English. An unrecognized stored value falls back to Chinese.

A few strings are deliberately not translated: `DuckLocal`, the `S3` and
`EXPLAIN` button labels, the `EN` / `中` toggle, and the credential placeholders.
Engine and network error messages are surfaced verbatim and are always in
English, whatever the interface language is.

## Themes

The sun/moon button toggles between the light and dark theme. There is no
theme picker and no theme file to edit — the two modes are what you get.

## Interface size {#interface-size}

Text, icons and controls scale together, in four steps:

| Size | Base text | Editor text |
| --- | --- | --- |
| Small | 13px | 12px |
| Default | 14px | 13px |
| Large | 16px | 15px |
| Extra large | 18px | 17px |

Choose one from the **Aa** button in the title bar, or step through them with
**⌘+** (larger), **⌘−** (smaller) and **⌘0** (back to default) — these work
wherever the focus is, the SQL editor included. The choice is remembered, and
survives switching between light and dark.

Panel sizes you dragged, and the height of a dashboard's plots, stay in pixels:
they are yours to adjust, not the interface's.

## Resetting

To start over — clearing history, registered files, remembered tabs, trusted
app folders and the language setting —
quit DuckLocal and remove its app data directory:

```bash
rm -rf ~/Library/Application\ Support/DuckLocal
```

The next launch creates it fresh. **Your data files are never affected by
this**, only DuckLocal's own records.

## Troubleshooting

Symptoms, causes and fixes are collected on the [Troubleshooting](troubleshooting.md) page.
