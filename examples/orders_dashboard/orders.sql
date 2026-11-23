-- %% [markdown]
-- # Orders, as a notebook
--
-- The same `orders.csv` the dashboard beside this file reads, explored a
-- cell at a time. Open it in the GUI:
--
--     ducklocal examples/orders_dashboard/orders.sql
--
-- Run a cell with **Ctrl+Enter** (⌘↵ on macOS), or run it and move on with
-- **Shift+Enter**. Paths are relative to this file. The file stays a plain
-- SQL script: `duckdb < orders.sql` runs it too.

-- %%
SELECT * FROM 'orders.csv' LIMIT 20;

-- %% [markdown]
-- ## Revenue by channel
--
-- A table made in one cell is there for the next.

-- %%
CREATE OR REPLACE TEMP TABLE by_channel AS
SELECT channel, round(sum(revenue), 2) AS revenue, sum(orders) AS orders
FROM 'orders.csv'
GROUP BY channel;

-- %%
SELECT channel, revenue, orders, round(revenue / orders, 2) AS per_order
FROM by_channel
ORDER BY revenue DESC;
