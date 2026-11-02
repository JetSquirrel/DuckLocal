-- Who did how much, from where, and how often were they refused?
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
  user_name,
  ip,
  count(*) AS events,
  count(error_code) AS denied,
  min(event_time) AS first_seen,
  max(event_time) AS last_seen
FROM events
GROUP BY user_name, ip
ORDER BY denied DESC, events DESC
