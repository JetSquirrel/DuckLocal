#!/usr/bin/env python3
"""Write data/cloudtrail.json.gz: a week of made-up CloudTrail events.

Nothing here came from a real account. The account IDs and access keys are the
ones AWS's documentation uses as examples, and the addresses are from the ranges
reserved for documentation (RFC 5737). The output is the same on every run, so
the committed file can be checked against this script.
"""

import gzip
import json
import random
from datetime import datetime, timedelta, timezone
from pathlib import Path

ACCOUNT = "111122223333"
START = datetime(2026, 3, 2, tzinfo=timezone.utc)
DAYS = 7

OFFICE_IP = "198.51.100.10"
CI_IP = "198.51.100.20"
ATTACKER_IP = "203.0.113.77"
SECOND_IP = "203.0.113.78"

CI_KEY = "AKIAIOSFODNN7EXAMPLE"
NEW_KEY = "AKIAI44QH8DHBEXAMPLE"

SOURCES = {
    "s3": "s3.amazonaws.com",
    "ec2": "ec2.amazonaws.com",
    "iam": "iam.amazonaws.com",
    "sts": "sts.amazonaws.com",
    "ecs": "ecs.amazonaws.com",
    "cloudtrail": "cloudtrail.amazonaws.com",
}


class Log:
    def __init__(self, rng):
        self.rng = rng
        self.events = []

    def add(
        self,
        when,
        service,
        name,
        user,
        ip,
        key=None,
        agent="aws-cli/2.15.0",
        error=None,
        params=None,
        read_only=None,
    ):
        event = {
            "eventVersion": "1.10",
            "eventTime": when.strftime("%Y-%m-%dT%H:%M:%SZ"),
            "eventSource": SOURCES[service],
            "eventName": name,
            "awsRegion": "us-east-1",
            "sourceIPAddress": ip,
            "userAgent": agent,
            "userIdentity": {
                "type": "IAMUser",
                "userName": user,
                "accountId": ACCOUNT,
                "arn": f"arn:aws:iam::{ACCOUNT}:user/{user}",
                "accessKeyId": key,
            },
            "requestParameters": params,
            "errorCode": error,
            "errorMessage": f"User: {user} is not authorized" if error else None,
            "readOnly": name.startswith(("Get", "List", "Describe"))
            if read_only is None
            else read_only,
            "eventID": f"{self.rng.getrandbits(128):032x}",
            "eventType": "AwsApiCall",
            "recipientAccountId": ACCOUNT,
        }
        self.events.append(event)


def routine(log):
    """People and the build going about a normal week, in office hours."""
    rng = log.rng
    people = [
        ("alice", ["DescribeInstances", "ListBuckets", "GetObject", "DescribeSecurityGroups"]),
        ("bob", ["DescribeInstances", "GetObject", "ListBuckets", "StartInstances", "StopInstances"]),
    ]
    services = {
        "DescribeInstances": "ec2",
        "DescribeSecurityGroups": "ec2",
        "StartInstances": "ec2",
        "StopInstances": "ec2",
        "ListBuckets": "s3",
        "GetObject": "s3",
        "PutObject": "s3",
        "UpdateService": "ecs",
        "DescribeServices": "ecs",
    }
    for day in range(DAYS):
        date = START + timedelta(days=day)
        weekday = date.weekday() < 5
        for user, names in people:
            for _ in range(rng.randint(30, 45) if weekday else rng.randint(0, 3)):
                name = rng.choice(names)
                when = date + timedelta(
                    hours=rng.randint(9, 17), minutes=rng.randint(0, 59), seconds=rng.randint(0, 59)
                )
                params = {"bucketName": "team-reports"} if services[name] == "s3" else None
                log.add(when, services[name], name, user, OFFICE_IP, agent="console.amazonaws.com", params=params)
        # The build runs through the working day with the key that later leaks.
        for hour in range(8, 19) if weekday else []:
            when = date + timedelta(hours=hour, minutes=rng.randint(0, 20))
            for name in ["PutObject", "DescribeServices", "UpdateService"]:
                params = {"bucketName": "build-artifacts"} if services[name] == "s3" else None
                when += timedelta(seconds=rng.randint(5, 40))
                log.add(when, services[name], name, "ci-deploy", CI_IP, CI_KEY, agent="ci-runner/4.1", params=params)


def incident(log):
    """A leaked build key is used at night from somewhere new, and stays."""
    rng = log.rng
    at = datetime(2026, 3, 5, 2, 14, 0, tzinfo=timezone.utc)
    agent = "python-requests/2.31"

    def step(seconds=(5, 25)):
        nonlocal at
        at += timedelta(seconds=rng.randint(*seconds))
        return at

    # Who am I, and what can this key do? Mostly, not much.
    log.add(step(), "sts", "GetCallerIdentity", "ci-deploy", ATTACKER_IP, CI_KEY, agent)
    for name, service in [
        ("ListUsers", "iam"),
        ("ListRoles", "iam"),
        ("GetAccountAuthorizationDetails", "iam"),
        ("ListAccessKeys", "iam"),
        ("DescribeInstances", "ec2"),
        ("ListSecrets", "iam"),
        ("GetAccountSummary", "iam"),
    ] * 4:
        log.add(step((1, 6)), service, name, "ci-deploy", ATTACKER_IP, CI_KEY, agent, error="AccessDenied")
    log.add(step(), "s3", "ListBuckets", "ci-deploy", ATTACKER_IP, CI_KEY, agent)

    # The key turns out to be able to make users, which is the whole incident.
    new = {"userName": "svc-backup"}
    log.add(step(), "iam", "CreateUser", "ci-deploy", ATTACKER_IP, CI_KEY, agent, params=new)
    log.add(
        step(),
        "iam",
        "AttachUserPolicy",
        "ci-deploy",
        ATTACKER_IP,
        CI_KEY,
        agent,
        params={**new, "policyArn": "arn:aws:iam::aws:policy/AdministratorAccess"},
    )
    log.add(step(), "iam", "CreateAccessKey", "ci-deploy", ATTACKER_IP, CI_KEY, agent, params=new)

    # The new user, from a neighbouring address: it stops the log, and reads.
    at += timedelta(minutes=9)
    log.add(step(), "cloudtrail", "DescribeTrails", "svc-backup", SECOND_IP, NEW_KEY, agent)
    log.add(
        step(),
        "cloudtrail",
        "StopLogging",
        "svc-backup",
        SECOND_IP,
        NEW_KEY,
        agent,
        error="AccessDenied",
        params={"name": "org-trail"},
    )
    log.add(step(), "s3", "ListBuckets", "svc-backup", SECOND_IP, NEW_KEY, agent)
    for number in range(240):
        log.add(
            step((1, 4)),
            "s3",
            "GetObject",
            "svc-backup",
            SECOND_IP,
            NEW_KEY,
            agent,
            params={"bucketName": "customer-exports", "key": f"exports/2026/part-{number:04d}.parquet"},
        )


def main():
    rng = random.Random(71)
    log = Log(rng)
    routine(log)
    incident(log)
    log.events.sort(key=lambda event: (event["eventTime"], event["eventID"]))
    target = Path(__file__).resolve().parent / "data" / "cloudtrail.json.gz"
    target.parent.mkdir(exist_ok=True)
    text = json.dumps({"Records": log.events}, separators=(",", ":")).encode()
    with target.open("wb") as raw:
        # No name or time in the header: the same bytes on every run.
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as out:
            out.write(text)
    print(f"{len(log.events)} events, {target.stat().st_size} bytes: {target}")


if __name__ == "__main__":
    main()
