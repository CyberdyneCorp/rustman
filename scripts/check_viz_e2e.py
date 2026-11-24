#!/usr/bin/env python3
"""End-to-end check of `rustman run --viz` with the real companion (issue #121).

Runs the demo collection (examples/demo/workspace) against the demo API
(examples/demo/backend.py) with `--viz`, and the real `rustman-viz` binary
drawing to a real window: on CI, Xvfb with Mesa's software Vulkan driver
(lavapipe), not Raven's headless backend.

A small wrapper sits between `rustman` and the companion and records what
crosses the pipe, so the check can assert:

- the handshake: `hello` from rustman, `hello_ack` from the companion,
  advertising the run dashboard;
- the full event stream: `open_view`, `run_started`, one
  `exchange_completed` per request, `run_finished` and `close`, with the
  API token never sent (it is a secret of the environment);
- a clean shutdown: once stdin ends, the companion draws its window, saves
  it with `--capture` and exits 0 (3 would mean no Vulkan), with nothing
  wrong in its log.

Usage: check_viz_e2e.py <rustman> <rustman-viz> [<artifacts-dir>]
"""

import json
import os
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEMO = REPO / "examples" / "demo"
TOKEN = "demo-token"
# The demo workspace: health, then the todos collection (list, create, get).
REQUESTS = 4

WRAPPER = """#!/usr/bin/env bash
# Records what rustman and the companion exchange, then runs the companion.
if [ "$1" = --protocol-check ]; then exec "{real}" "$@"; fi
tee "{logs}/in.jsonl" | "{real}" --capture "{logs}/capture" | tee "{logs}/out.jsonl"
status=("${{PIPESTATUS[@]}}")
echo "${{status[1]}}" > "{logs}/status.tmp" && mv "{logs}/status.tmp" "{logs}/status"
"""


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for(url: str, seconds: float) -> None:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=1):
                return
        except OSError:
            time.sleep(0.2)
    fail(f"{url} did not answer within {seconds} s")


def lines(path: Path) -> list:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def main() -> None:
    if len(sys.argv) not in (3, 4):
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    rustman = Path(sys.argv[1]).resolve()
    companion = Path(sys.argv[2]).resolve()
    artifacts = Path(sys.argv[3]).resolve() if len(sys.argv) == 4 else None

    tmp = Path(tempfile.mkdtemp(prefix="viz-e2e-"))
    workspace = tmp / "workspace"
    shutil.copytree(DEMO / "workspace", workspace)
    port = free_port()
    (workspace / "environments" / "local.yaml").write_text(
        f"variables:\n  base_url: http://127.0.0.1:{port}\nsecrets:\n  - api_token\n"
    )
    logs = tmp / "logs"
    (logs / "capture").mkdir(parents=True)
    wrapper = tmp / "rustman-viz"
    wrapper.write_text(WRAPPER.format(real=companion, logs=logs))
    wrapper.chmod(wrapper.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    env = dict(os.environ, RUSTMAN_SECRET_STORE="file", RUSTMAN_VIZ=str(wrapper))
    backend = subprocess.Popen(
        [sys.executable, str(DEMO / "backend.py")],
        env=dict(os.environ, DEMO_PORT=str(port)),
    )
    try:
        wait_for(f"http://127.0.0.1:{port}/health", 20)
        subprocess.run(
            [str(rustman), "secret", "set", "api_token", "--env", "local"],
            cwd=workspace, env=env, input=TOKEN, text=True, check=True,
        )
        run = subprocess.run(
            [str(rustman), "run", "--env", "local", "--viz"],
            cwd=workspace, env=env, capture_output=True, text=True, timeout=120,
        )
        print(run.stdout)
        print(run.stderr, file=sys.stderr)
        if run.returncode != 0:
            fail(f"rustman run --viz exited {run.returncode}")
        if "warning" in run.stderr or "continuing without visuals" in run.stderr:
            fail("rustman did not stream to the companion (see its stderr above)")

        # The companion outlives rustman: it draws, captures and exits.
        status_file = logs / "status"
        deadline = time.monotonic() + 120
        while not status_file.exists() and time.monotonic() < deadline:
            time.sleep(0.5)
        viz_log = workspace / ".rustman" / "viz.log"
        log = viz_log.read_text() if viz_log.exists() else ""
        print(f"--- companion log ---\n{log}", file=sys.stderr)
        if not status_file.exists():
            fail("the companion did not exit within 120 s after the run")
        status = status_file.read_text().strip()
        if status != "0":
            fail(f"the companion exited {status} (3: graphics could not start)")
        for bad in ("cannot open", "lacks", "skipped a line", "cannot capture", "panicked"):
            if bad in log:
                fail(f"the companion log says {bad!r}")

        # The handshake.
        sent = lines(logs / "in.jsonl")
        received = lines(logs / "out.jsonl")
        if not sent or sent[0].get("type") != "hello" or sent[0].get("protocol") != "rustman-viz/1":
            fail(f"rustman did not start with hello: {sent[:1]}")
        if not received or received[0].get("type") != "hello_ack":
            fail(f"the companion did not answer hello_ack: {received}")
        if "run_dashboard" not in received[0].get("views", []):
            fail(f"hello_ack lacks run_dashboard: {received[0]}")
        print(f"handshake: {received[0]}")

        # The full event stream.
        kinds = [m.get("type") for m in sent]
        wanted = (
            ["hello", "open_view", "run_started"]
            + ["exchange_completed"] * REQUESTS
            + ["run_finished", "close"]
        )
        if kinds != wanted:
            fail(f"event stream {kinds}, wanted {wanted}")
        if sent[1].get("view") != "run_dashboard":
            fail(f"open_view: {sent[1]}")
        if sent[2].get("total") != REQUESTS:
            fail(f"run_started: {sent[2]}")
        exchanges = [m for m in sent if m.get("type") == "exchange_completed"]
        if any(m.get("outcome") != "passed" for m in exchanges):
            fail(f"not every exchange passed: {exchanges}")
        summary = sent[-2].get("summary", {})
        if sent[-2].get("state") != "completed" or summary.get("passed") != REQUESTS:
            fail(f"run_finished: {sent[-2]}")
        if TOKEN in (logs / "in.jsonl").read_text():
            fail("the API token reached the companion")
        print(f"event stream: {kinds}")

        # The window was drawn by the real backend.
        png = logs / "capture" / "run_dashboard.png"
        if not png.exists() or png.stat().st_size < 10_000:
            fail(f"no capture of the run dashboard at {png}")
        print(f"capture: {png} ({png.stat().st_size} bytes)")
        print("OK: run --viz with the real companion")
    finally:
        backend.terminate()
        backend.wait(timeout=10)
        if artifacts is not None:
            shutil.copytree(logs, artifacts, dirs_exist_ok=True)
            viz_log = workspace / ".rustman" / "viz.log"
            if viz_log.exists():
                shutil.copy(viz_log, artifacts / "viz.log")
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
