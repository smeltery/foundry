"""Decide the aggregate required check for one CI run.

Every planned job must succeed. One narrow exception covers hosted runner
outages: a macOS job that GitHub cancelled without ever assigning a runner is
excused only when the same commit carries a successful local verification
status from the advisory macOS profile. A macOS job that ran and failed, or any
other job that did not succeed, still fails the gate.
"""

import json
import os
import sys
import time
import urllib.request

LOCAL_STATUS = "bellows/verify"
GATE_JOB = "required"


def unassigned_macos(job):
    name = job.get("name", "")
    return (("macos" in name or "darwin" in name)
            and job.get("conclusion") == "cancelled"
            and not job.get("runner_name"))


def decide(plan, check, activation, activation_plan, jobs, local_verified):
    """Return (passed, explanation) for the run's needs and job records."""
    if plan != "success":
        return False, "plan did not succeed"
    activation_ok = activation == "success" or (
        activation == "skipped" and activation_plan == '{"include":[]}')
    if check == "success" and activation_ok:
        return True, "every planned job succeeded"
    blocking = [job for job in jobs
                if job.get("name") != GATE_JOB
                and job.get("conclusion") not in ("success", "skipped")]
    excused = [job for job in blocking if unassigned_macos(job)]
    if not blocking or len(excused) != len(blocking):
        names = ", ".join(sorted("{} ({}, runner {})".format(
            job["name"], job.get("conclusion"), job.get("runner_name") or "none")
            for job in blocking if job not in excused))
        return False, "jobs did not succeed: " + (names or "unknown")
    if not local_verified:
        return False, "macOS jobs never got a runner and no " + LOCAL_STATUS + " status succeeded"
    names = ", ".join(sorted(job["name"] for job in excused))
    return True, "excused unassigned macOS jobs with local verification: " + names


def api(path):
    request = urllib.request.Request(
        os.environ["GITHUB_API_URL"] + path,
        headers={"Authorization": "Bearer " + os.environ["GITHUB_TOKEN"],
                 "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def settled_jobs(attempts=6, delay=5):
    """The jobs API can lag a just-cancelled job; wait briefly for final records."""
    for _ in range(attempts):
        jobs = run_jobs()
        if all(job.get("conclusion") for job in jobs if job.get("name") != GATE_JOB):
            return jobs
        time.sleep(delay)
    return jobs


def run_jobs():
    repository, run, attempt = (os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_RUN_ID"],
                                os.environ["GITHUB_RUN_ATTEMPT"])
    jobs, page = [], 1
    while True:
        body = api(f"/repos/{repository}/actions/runs/{run}/attempts/{attempt}/jobs"
                   f"?per_page=100&page={page}")
        jobs += body["jobs"]
        if len(jobs) >= body["total_count"] or not body["jobs"]:
            return jobs
        page += 1


def locally_verified(sha):
    statuses = api(f"/repos/{os.environ['GITHUB_REPOSITORY']}/commits/{sha}/status")["statuses"]
    return any(status["context"] == LOCAL_STATUS and status["state"] == "success"
               for status in statuses)


def main():
    env = os.environ
    needs = (env["PLAN_RESULT"], env["CHECK_RESULT"], env["ACTIVATION_RESULT"], env["ACTIVATION_PLAN"])
    passed, explanation = decide(*needs, jobs=[], local_verified=False)
    if not passed and needs[0] == "success":
        jobs = settled_jobs()
        passed, explanation = decide(*needs, jobs=jobs,
                                     local_verified=locally_verified(env["HEAD_SHA"]))
    print(explanation)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
