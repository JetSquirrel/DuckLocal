-- The build key's address on working days, against everything else it did.
WITH events AS (
  SELECT
    r.eventName AS event_name,
    -- Epoch, so a day means the same day in every time zone.
    make_timestamp(epoch_us(r.eventTime)) AS event_time,
    r.eventSource AS service,
    r.sourceIPAddress AS ip,
    r.userIdentity.userName AS user_name,
    r.errorCode AS error_code,
    r.readOnly AS read_only,
    r.requestParameters AS params
  FROM (SELECT unnest(Records) AS r FROM 'data/cloudtrail.json.gz')
)
SELECT
  ip,
  count(*) AS events,
  count(*) FILTER (hour(event_time) NOT BETWEEN 8 AND 19) AS outside_hours,
  count(DISTINCT event_name) AS distinct_calls
FROM events
WHERE user_name = 'ci-deploy'
GROUP BY ip
ORDER BY events DESC
