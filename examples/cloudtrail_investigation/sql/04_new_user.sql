-- What the user that was created went on to do.
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
  event_name,
  params.bucketName AS bucket,
  error_code,
  count(*) AS events,
  min(event_time) AS first_seen,
  max(event_time) AS last_seen
FROM events
WHERE user_name = 'svc-backup'
GROUP BY ALL
ORDER BY first_seen
