#!/usr/bin/env python3
"""Write sample monitor results for the tui-monitors frame.

The Monitors view reads `.rustman/monitors/<name>/` relative to the current
time (24-hour uptime, hourly p95 sparkline, recent runs), so the results are
generated for the last 24 hours before now: a fixed seed keeps their shape the
same on every render, only the timestamps move. No request is sent.

Usage: monitor-store.py WORKSPACE
"""

import json
import random
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path


def stamp(at):
    return at.strftime("%Y-%m-%dT%H:%M:%SZ")


def result(at, outcome, latency, request, code):
    failed = outcome == "fail"
    line = {
        "at": stamp(at),
        "outcome": outcome,
        "duration_ms": latency,
        "summary": {"total": 1, "passed": 0 if failed else 1, "failed": 1 if failed else 0, "errored": 0, "skipped": 0},
        "requests": [{"id": request, "status": "failed" if failed else "passed", "code": code, "duration_ms": latency}],
    }
    if failed:
        line["failures"] = [f"{request}: status: expected 200, got {code}"]
    return line


def write(root, name, runs, firing):
    folder = root / ".rustman" / "monitors" / name
    days = {}
    for run in runs:
        days.setdefault(run["at"][:10], []).append(json.dumps(run, separators=(",", ":")))
    for day, lines in days.items():
        path = folder / "raw" / f"{day}.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("\n".join(lines) + "\n")
    last = runs[-1]
    state = {"last_run": {"at": last["at"], "outcome": last["outcome"], "duration_ms": last["duration_ms"]}}
    if firing:
        state["alerts"] = {"firing": firing}
    (folder / "state.json").write_text(json.dumps(state, indent=2) + "\n")


def monitor(now, every, count, failing, base, request):
    rng = random.Random(f"{request}{every}")
    start = now - every * count
    runs = []
    for i in range(1, count + 1):
        at = start + every * i
        latency = round(base + rng.uniform(-15, 25) + 30 * (12 < (i * 24 // count) < 16), 1)
        outcome = "fail" if i > count - failing else "pass"
        runs.append(result(at, outcome, latency, request, 503 if outcome == "fail" else 200))
    return runs


def main():
    root = Path(sys.argv[1])
    now = datetime.now(timezone.utc).replace(second=0, microsecond=0)
    now -= timedelta(minutes=now.minute % 5)
    pets = monitor(now, timedelta(minutes=5), 288, 3, 95, "pets/get-pet")
    since = pets[-3]["at"]
    write(root, "pet-api", pets, {"consecutive_failures: 3": {"since": since, "last_notified": since}})
    write(root, "store-api", monitor(now, timedelta(minutes=15), 96, 0, 140, "store/inventory"), None)


if __name__ == "__main__":
    main()
