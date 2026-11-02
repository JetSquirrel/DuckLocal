-- Every change to who can do what, in order.
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
  event_time,
  user_name,
  ip,
  event_name,
  params.userName AS target_user,
  params.policyArn AS policy,
  error_code
FROM events
WHERE service = 'iam.amazonaws.com' AND NOT read_only AND error_code IS NULL
ORDER BY event_time
