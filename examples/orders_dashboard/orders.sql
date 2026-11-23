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

-- %% name=channels
SELECT channel, revenue, orders, round(revenue / orders, 2) AS per_order
FROM by_channel
ORDER BY revenue DESC;

-- %% [plot]
-- type = "bar"
-- data = channels
-- x = "channel"
-- y = "revenue"
-- title = "Revenue by channel"
-- format = "usd"

-- %% [markdown]
-- ## Day by day
--
-- A plot draws the SQL cell its `data` names, or the nearest one above it.

-- %%
SELECT date, channel, sum(orders) AS orders
FROM 'orders.csv'
GROUP BY ALL
ORDER BY date;

-- %% [plot]
-- type = "line"
-- x = "date"
-- y = "orders"
-- series = "channel"
-- title = "Orders per day"

-- %% [markdown]
-- ## A dashboard in the notebook
--
-- A `[dash]` cell holds `.dash` blocks — sources, queries, plots, filters —
-- and draws them as a dashboard. Click a channel's bar to filter the rest.

-- %% [dash]
-- source "orders" {
--   path = "orders.csv"
-- }
--
-- query "totals" {
--   sql = <<SQL
--     SELECT sum(revenue) AS revenue, sum(orders) AS orders
--     FROM orders
--     WHERE $channel
--   SQL
-- }
--
-- query "by_channel" {
--   sql = <<SQL
--     SELECT channel, sum(revenue) AS revenue
--     FROM orders
--     GROUP BY channel
--     ORDER BY revenue DESC
--   SQL
-- }
--
-- query "by_region" {
--   sql = <<SQL
--     SELECT region, sum(orders) AS orders
--     FROM orders
--     WHERE $channel
--     GROUP BY region
--     ORDER BY orders DESC
--   SQL
-- }
--
-- plot "revenue" {
--   type   = "card"
--   query  = query.totals
--   value  = revenue
--   title  = "Revenue"
--   format = "usd"
--   width  = 6
-- }
--
-- plot "orders" {
--   type  = "card"
--   query = query.totals
--   value = orders
--   title = "Orders"
--   width = 6
-- }
--
-- plot "channels" {
--   type   = "bar"
--   query  = query.by_channel
--   x      = channel
--   y      = revenue
--   title  = "Revenue by channel"
--   format = "usd"
--   width  = 6
-- }
--
-- plot "regions" {
--   type  = "bar"
--   query = query.by_region
--   x     = region
--   y     = orders
--   title = "Orders by region"
--   width = 6
-- }
--
-- filter "channel" {
--   plot = plot.channels
-- }
