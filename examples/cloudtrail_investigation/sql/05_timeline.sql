-- Everything from the two addresses, in order. Long: read it with --limit.
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
SELECT event_time, ip, user_name, event_name, error_code
FROM events
WHERE ip IN ('203.0.113.77', '203.0.113.78')
ORDER BY event_time
