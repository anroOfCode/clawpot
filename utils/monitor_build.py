#!/usr/bin/env python3
"""Monitor a Buildkite build by commit SHA, polling until it completes.

On failure, prints all job logs to the console.

Usage:
    python utils/monitor_build.py <commit-sha>
    python utils/monitor_build.py HEAD

Environment variables:
    BUILDKITE_API_TOKEN  - Required. API token with read_builds and read_build_logs scopes.
    BUILDKITE_ORG        - Org slug (default: clawpot)
    BUILDKITE_PIPELINE   - Pipeline slug (default: clawpot)
"""

import argparse
import html
import os
import re
import subprocess
import sys
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen
import json

API_BASE = "https://api.buildkite.com/v2"
DEFAULT_POLL_INTERVAL = 10  # seconds
DEFAULT_BUILD_WAIT_TIMEOUT = 120  # seconds to wait for a build to appear


def get_token():
    token = os.environ.get("BUILDKITE_API_TOKEN")
    if not token:
        print("Error: BUILDKITE_API_TOKEN environment variable is not set.", file=sys.stderr)
        print("Create one at https://buildkite.com/user/api-access-tokens", file=sys.stderr)
        print("Required scopes: read_builds, read_build_logs", file=sys.stderr)
        sys.exit(1)
    return token


def api_get(token: str, path: str) -> dict | list | str:
    url = f"{API_BASE}{path}"
    req = Request(url, headers={"Authorization": f"Bearer {token}"})
    try:
        with urlopen(req) as resp:
            content_type = resp.headers.get("Content-Type", "")
            body = resp.read().decode()
            if "application/json" in content_type:
                return json.loads(body)
            return body
    except HTTPError as e:
        if e.code == 404:
            return None
        print(f"API error {e.code}: {e.read().decode()}", file=sys.stderr)
        sys.exit(1)


def resolve_commit(ref: str) -> str:
    """Resolve a git ref (like HEAD, branch name) to a full SHA."""
    try:
        result = subprocess.run(
            ["git", "rev-parse", ref],
            capture_output=True, text=True, check=True,
        )
        return result.stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        # If git isn't available or ref isn't valid, use the input as-is
        return ref


def find_build(token: str, org: str, pipeline: str, commit: str) -> dict | None:
    builds = api_get(token, f"/organizations/{org}/pipelines/{pipeline}/builds?commit={commit}")
    if builds:
        return builds[0]
    return None


def wait_for_build(token: str, org: str, pipeline: str, commit: str,
                   poll_interval: int, timeout: int) -> dict:
    short = commit[:10]
    print(f"Looking for build with commit {short}...")

    start = time.time()
    while time.time() - start < timeout:
        build = find_build(token, org, pipeline, commit)
        if build:
            return build
        elapsed = int(time.time() - start)
        print(f"  No build found yet ({elapsed}s elapsed), retrying in {poll_interval}s...")
        time.sleep(poll_interval)

    print(f"Error: No build found for commit {short} after {timeout}s.", file=sys.stderr)
    print("Has the commit been pushed and the pipeline triggered?", file=sys.stderr)
    sys.exit(1)


def strip_html(text: str) -> str:
    """Strip HTML tags and decode entities from Buildkite log output."""
    text = re.sub(r"<time[^>]*>[^<]*</time>", "", text)
    text = re.sub(r"<[^>]+>", "", text)
    text = html.unescape(text)
    return text


def get_job_log(token: str, org: str, pipeline: str, build_number: int, job_id: str) -> str:
    log = api_get(token, f"/organizations/{org}/pipelines/{pipeline}/builds/{build_number}/jobs/{job_id}/log")
    if log is None:
        return "(no log available)"
    if isinstance(log, dict):
        content = log.get("content", "(empty log)")
    else:
        content = log
    return strip_html(content)


def format_duration(seconds: float) -> str:
    m, s = divmod(int(seconds), 60)
    if m:
        return f"{m}m{s}s"
    return f"{s}s"


def state_symbol(state: str) -> str:
    symbols = {
        "passed": "\033[32m✓\033[0m",
        "failed": "\033[31m✗\033[0m",
        "running": "\033[33m●\033[0m",
        "scheduled": "○",
        "waiting": "○",
        "blocked": "◉",
        "canceled": "\033[31m⊘\033[0m",
        "canceling": "\033[31m⊘\033[0m",
        "skipped": "\033[90m–\033[0m",
        "not_run": "\033[90m–\033[0m",
    }
    return symbols.get(state, "?")


def print_build_status(build: dict):
    number = build["number"]
    state = build["state"]
    url = build.get("web_url", "")
    sym = state_symbol(state)
    print(f"\n  Build #{number} {sym} {state}  {url}")

    for job in build.get("jobs", []):
        if job.get("type") != "script":
            continue
        name = job.get("name", job.get("id", "?"))
        jstate = job.get("state", "unknown")
        jsym = state_symbol(jstate)
        duration = ""
        if job.get("started_at") and job.get("finished_at"):
            # Parse ISO timestamps to compute duration
            pass
        print(f"    {jsym} {name}: {jstate}")


def monitor(token: str, org: str, pipeline: str, commit: str,
            poll_interval: int, timeout: int):
    build = wait_for_build(token, org, pipeline, commit, poll_interval, timeout)
    build_number = build["number"]
    url = build.get("web_url", "")

    print(f"\nFound build #{build_number}: {url}")
    print(f"Polling every {poll_interval}s until complete...\n")

    terminal_states = {"passed", "failed", "canceled", "not_run"}
    last_state = None

    while True:
        build = api_get(token, f"/organizations/{org}/pipelines/{pipeline}/builds/{build_number}")
        state = build["state"]

        if state != last_state:
            print_build_status(build)
            last_state = state

        if state in terminal_states:
            break

        time.sleep(poll_interval)

    # Final summary
    print(f"\n{'='*60}")
    if state == "passed":
        print(f"\033[32mBuild #{build_number} passed.\033[0m")
    elif state == "failed":
        print(f"\033[31mBuild #{build_number} failed.\033[0m")
        print(f"\nFetching logs for failed jobs...\n")
        print_failed_logs(token, org, pipeline, build)
    elif state == "canceled":
        print(f"Build #{build_number} was canceled.")
    else:
        print(f"Build #{build_number} ended with state: {state}")
    print(f"{'='*60}")

    return 0 if state == "passed" else 1


def print_failed_logs(token: str, org: str, pipeline: str, build: dict):
    build_number = build["number"]
    for job in build.get("jobs", []):
        if job.get("type") != "script":
            continue
        if job.get("state") != "failed":
            continue

        name = job.get("name", job.get("id", "?"))
        job_id = job["id"]

        print(f"{'─'*60}")
        print(f"Job: {name}")
        print(f"{'─'*60}")

        log = get_job_log(token, org, pipeline, build_number, job_id)
        print(log)
        print()


def main():
    parser = argparse.ArgumentParser(
        description="Monitor a Buildkite build by commit SHA.",
    )
    parser.add_argument(
        "commit",
        help="Git commit SHA or ref (e.g. HEAD, abc1234, main)",
    )
    parser.add_argument(
        "--org", default=os.environ.get("BUILDKITE_ORG", "clawpot"),
        help="Buildkite org slug (default: $BUILDKITE_ORG or 'clawpot')",
    )
    parser.add_argument(
        "--pipeline", default=os.environ.get("BUILDKITE_PIPELINE", "clawpot"),
        help="Buildkite pipeline slug (default: $BUILDKITE_PIPELINE or 'clawpot')",
    )
    parser.add_argument(
        "--poll-interval", type=int, default=DEFAULT_POLL_INTERVAL,
        help=f"Seconds between status checks (default: {DEFAULT_POLL_INTERVAL})",
    )
    parser.add_argument(
        "--timeout", type=int, default=DEFAULT_BUILD_WAIT_TIMEOUT,
        help=f"Seconds to wait for build to appear (default: {DEFAULT_BUILD_WAIT_TIMEOUT})",
    )
    args = parser.parse_args()

    token = get_token()
    commit = resolve_commit(args.commit)
    print(f"Monitoring build for commit {commit[:10]}...")

    exit_code = monitor(token, args.org, args.pipeline, commit,
                        args.poll_interval, args.timeout)
    sys.exit(exit_code)


if __name__ == "__main__":
    main()
